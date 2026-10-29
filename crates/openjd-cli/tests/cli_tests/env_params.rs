// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` (Environment Parameters RFC, draft) end to end through the
//! `openjd` binary: `run`, `check --environment`, `summary`, and context
//! help.
//!
//! The fixtures in `templates/env_params/` are copies of the RFC's samples
//! (`conda-env-params.yaml`, `docker-env-params.yaml`,
//! `video-sfm-composite.yaml` in openjd-specifications) whose application
//! commands are replaced with Python stubs that print the values they
//! receive, plus small fixtures for exiting and re-entering Environments
//! between Steps and for submission errors.

use super::*;

fn fixture(name: &str) -> String {
    templates_dir()
        .join("env_params")
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

/// Assert that `needles` occur in `haystack` in this order, each after the
/// end of the previous one.
fn assert_in_order(haystack: &str, needles: &[&str]) {
    let mut rest = haystack;
    for needle in needles {
        match rest.find(needle) {
            Some(at) => rest = &rest[at + needle.len()..],
            None => panic!("expected {needle:?} (in order {needles:?}) in:\n{haystack}"),
        }
    }
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The video sample with few Composite Tasks, so the runs stay short.
fn run_video(extra: &[&str]) -> (i32, String, String) {
    let video = fixture("video-sfm-composite.yaml");
    let mut args = vec!["run", video.as_str(), "-p", "FrameCount=20"];
    args.extend_from_slice(extra);
    run_cli(&args)
}

// ── openjd run with the RFC samples ─────────────────────────────────

#[test]
fn each_step_enters_conda_with_its_own_values() {
    let conda = fixture("conda-env-params.yaml");
    let (code, stdout, stderr) = run_video(&["--environment", &conda]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    // One Session runs every Step. Consecutive Steps all differ
    // (DecodeFrames and EncodeVideo are equal but not adjacent), so the
    // Session exits Conda after each Step and enters it again with the next
    // Step's values.
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(count(&stdout, "Entering Environment: Conda"), 4, "{stdout}");
    assert_eq!(count(&stdout, "Exiting Environment: Conda"), 4, "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "\tSession start",
            "Entering Environment: Conda",
            "\tPackages(STRING) = ffmpeg=7",
            "Conda onEnter: Packages='ffmpeg=7' Channels='conda-forge'",
            "Running step 'DecodeFrames'",
            "DecodeFrames task: conda='ffmpeg=7' docker=None",
            "Exiting Environment: Conda",
            "Conda onExit: Packages='ffmpeg=7'",
            "Entering Environment: Conda",
            "Conda onEnter: Packages='colmap=3.11' Channels='conda-forge'",
            "Running step 'SolveSfM'",
            "SolveSfM task: conda='colmap=3.11' docker=None",
            "Conda onExit: Packages='colmap=3.11'",
            "\tChannels(STRING) = studio-internal conda-forge",
            "\tPackages(STRING) = maya=2025 mtoa",
            "Conda onEnter: Packages='maya=2025 mtoa' Channels='studio-internal conda-forge'",
            "Composite task: frames 1-10",
            "Composite task: conda='maya=2025 mtoa' docker=None",
            "Composite task: frames 11-20",
            "Composite task: conda='maya=2025 mtoa' docker=None",
            "Conda onExit: Packages='maya=2025 mtoa'",
            "Conda onEnter: Packages='ffmpeg=7' Channels='conda-forge'",
            "EncodeVideo task: conda='ffmpeg=7' docker=None",
            "Conda onExit: Packages='ffmpeg=7'",
            "All actions completed successfully!",
        ],
    );
    // Every Step's Docker entry is `required: false`, and no Docker
    // Environment is attached.
    assert_eq!(count(&stderr, "WARNING: "), 4, "{stderr}");
    assert!(
        stderr.contains(&format!(
            "WARNING: JobTemplate -> steps[1] -> dependencies[2] -> environment: names \
             Environment 'Docker', which is not a jobEnvironments entry of the Job Template, and \
             none of the attached Environment Templates ({conda}) defines it. The entry has \
             'required: false', so it has no effect."
        )),
        "{stderr}"
    );
}

#[test]
fn docker_wraps_each_step_with_its_own_image_and_mapped_mounts() {
    let docker = fixture("docker-env-params.yaml");
    // The CLI resolves the Job Template's directory with `canonicalize`
    // (and strips Windows' `\\?\` prefix), so the expected paths do too.
    let source = std::fs::canonicalize(templates_dir().join("env_params")).unwrap();
    let source = std::path::PathBuf::from(
        source
            .to_str()
            .unwrap()
            .strip_prefix(r"\\?\")
            .unwrap_or(source.to_str().unwrap()),
    );
    let (source_format, destination) = if cfg!(windows) {
        ("WINDOWS", r"C:\mapped")
    } else {
        ("POSIX", "/mapped")
    };
    let rules = serde_json::json!({
        "version": "pathmapping-1.0",
        "path_mapping_rules": [{
            "source_path_format": source_format,
            "source_path": source.to_str().unwrap(),
            "destination_path": destination,
        }]
    })
    .to_string();
    let (code, stdout, stderr) =
        run_video(&["--environment", &docker, "--path-mapping-rules", &rules]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");

    let mapped = |rel: &[&str]| {
        let mut p = std::path::PathBuf::from(destination);
        p.extend(rel);
        p.to_str().unwrap().to_string()
    };
    let raw = |rel: &[&str]| {
        let mut p = source.clone();
        p.extend(rel);
        p.to_str().unwrap().to_string()
    };
    let list = |paths: [String; 4]| {
        let quoted: Vec<String> = paths
            .iter()
            .map(|p| format!("{p:?}").replace('"', "'"))
            .collect();
        format!("[{}]", quoted.join(", "))
    };
    let mapped_mounts = list([
        mapped(&["input", "plate.mp4"]),
        mapped(&["input", "objects.ma"]),
        mapped(&["work"]),
        mapped(&["output", "composite.mp4"]),
    ]);
    let raw_mounts = list([
        raw(&["input", "plate.mp4"]),
        raw(&["input", "objects.ma"]),
        raw(&["work"]),
        raw(&["output", "composite.mp4"]),
    ]);
    assert_in_order(
        &stdout,
        &[
            "Docker onEnter: Image='docker.io/linuxserver/ffmpeg:latest' Gpus=''",
            // Env.Param.Mounts is path-mapped; Env.RawParam.Mounts is not.
            &format!("Docker onEnter: Mounts={mapped_mounts}"),
            &format!("Docker onEnter: RawMounts={raw_mounts}"),
            "Docker wrap in docker.io/linuxserver/ffmpeg:latest: python",
            "DecodeFrames task: conda=None docker='docker.io/linuxserver/ffmpeg:latest'",
            "Docker onExit: stopping docker.io/linuxserver/ffmpeg:latest",
            "Docker onEnter: Image='docker.io/colmap/colmap:latest' Gpus='all'",
            "SolveSfM task: conda=None docker='docker.io/colmap/colmap:latest'",
            "Docker onExit: stopping docker.io/colmap/colmap:latest",
            "Docker onEnter: Image='registry.example.com/studio/maya:2025' Gpus=''",
            "Composite task: conda=None docker='registry.example.com/studio/maya:2025'",
            "Docker onExit: stopping registry.example.com/studio/maya:2025",
            "Docker onEnter: Image='docker.io/linuxserver/ffmpeg:latest' Gpus=''",
            "EncodeVideo task: conda=None docker='docker.io/linuxserver/ffmpeg:latest'",
            "Docker onExit: stopping docker.io/linuxserver/ffmpeg:latest",
            "All actions completed successfully!",
        ],
    );
    // The Conda entries are `required: false`, but their `enableIf`
    // disables them when Docker is attached, so they are not resolved and
    // give no warning.
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn optional_entries_are_satisfied_without_any_environment() {
    let (code, stdout, stderr) = run_video(&[]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    // No Environment declares parameters, so every Step has equal (no)
    // values and the Job runs in one Session, as before ENV_PARAMS.
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert!(!stdout.contains("Entering Environment"), "{stdout}");
    assert!(!stdout.contains("Starting a new Session"), "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "DecodeFrames task: conda=None docker=None",
            "SolveSfM task: conda=None docker=None",
            "Composite task: frames 1-10",
            "EncodeVideo task: conda=None docker=None",
            "All actions completed successfully!",
        ],
    );
    // One warning per `required: false` entry: four Steps, two entries.
    assert_eq!(count(&stderr, "WARNING: "), 8, "{stderr}");
    assert!(
        stderr.contains(
            "WARNING: JobTemplate -> steps[0] -> dependencies[0] -> environment: names \
             Environment 'Conda', which is not a jobEnvironments entry of the Job Template, and \
             no Environment Template is attached. The entry has 'required: false', so it has no \
             effect."
        ),
        "{stderr}"
    );
}

#[test]
fn a_job_template_without_env_params_takes_the_defaults() {
    // Backward compatibility (RFC; plan G4): the Job Template does not list
    // ENV_PARAMS, and the Conda Environment's defaults read its own Job
    // Parameters.
    let conda = fixture("conda-env-params.yaml");
    let job = templates_dir().join("wrap_simple_job.yaml");
    let (code, stdout, stderr) = run_cli(&[
        "run",
        job.to_str().unwrap(),
        "--environment",
        &conda,
        "-p",
        "CondaPackages=numpy scipy",
    ]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "\tEnvironment parameter values:",
            "\tChannels(STRING) = conda-forge",
            "\tPackages(STRING) = numpy scipy",
            "Conda onEnter: Packages='numpy scipy' Channels='conda-forge'",
            "TaskBody Ran",
            "Conda onExit: Packages='numpy scipy'",
        ],
    );
    assert!(stderr.is_empty(), "{stderr}");
}

// ── Exiting and re-entering Environments between Steps ──────────────

#[test]
fn a_step_with_other_values_reenters_the_environment_in_the_same_session() {
    let (code, stdout, stderr) = run_cli(&["run", &fixture("grouping.yaml")]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(count(&stdout, "\tRunning job 'Grouping'"), 1, "{stdout}");
    assert_eq!(count(&stdout, "Seats onEnter"), 2, "{stdout}");
    assert_eq!(count(&stdout, "Seats onExit"), 2, "{stdout}");
    // The Session runs every Step once.
    for step in ["A", "B", "C", "D"] {
        assert_eq!(count(&stdout, &format!("Step {step} ran")), 1, "{stdout}");
    }
    assert_in_order(
        &stdout,
        &[
            "\tSession start",
            "\tCount(INT) = 2",
            "Seats onEnter Count=2",
            "Step A ran",
            // INT 2 written as 2 and as "2" are equal (§3.2.2).
            "Step B ran",
            // C takes the default, so the Session exits Seats and enters
            // it again with C's value before C runs (Template Schemas
            // §3.2.2).
            "Exiting Environment: Seats",
            "Seats onExit Count=2",
            "Entering Environment: Seats",
            "\tCount(INT) = 1",
            "Seats onEnter Count=1",
            "Running step 'C'",
            "Step C ran",
            "Step D ran",
            "Seats onExit Count=1",
            "All actions completed successfully!",
        ],
    );
    assert!(!stdout.contains("Starting a new Session"), "{stdout}");
}

#[test]
fn a_step_that_does_not_request_an_environment_runs_after_it_is_exited() {
    // `activation: ON_REQUEST` (Template Schemas §4 item 4): only the
    // requesting Steps' Tasks run with the Environment entered or its
    // variables set (§3.2.2, fourth paragraph). The Session exits it before
    // C and keeps Always, which every Step enters, entered throughout.
    let (code, stdout, stderr) = run_cli(&["run", &fixture("activation_grouping.yaml")]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(count(&stdout, "Always onEnter"), 1, "{stdout}");
    assert_eq!(count(&stdout, "OnRequest onEnter"), 1, "{stdout}");
    assert_eq!(count(&stdout, "Exiting Environment: Always"), 1, "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "\tSession start",
            "Always onEnter",
            "OnRequest onEnter",
            "Step A ran ON_REQUEST=set",
            "Step B ran",
            "Exiting Environment: OnRequest",
            "OnRequest onExit",
            "Running step 'C'",
            "Step C ran ON_REQUEST=unset",
            "Exiting Environment: Always",
            "All actions completed successfully!",
        ],
    );
}

#[test]
fn an_environment_reads_job_name_and_job_env_names() {
    // Each Environment is entered with the table job creation froze for it
    // (Template Schemas §7.3.1); Job.EnvNames lists an unrequested
    // ON_REQUEST Environment, which no Session enters.
    let (code, stdout, stderr) = run_cli(&["run", &fixture("env_names_in_environment.yaml")]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_in_order(
        &stdout,
        &[
            "First onEnter EnvNamesInEnvironment First Second",
            "Step S ran",
        ],
    );
    assert!(!stdout.contains("Entering Environment: Second"), "{stdout}");
}

#[test]
fn a_selected_step_with_its_dependencies_reenters_environments_the_same_way() {
    let (code, stdout, stderr) = run_cli(&[
        "run",
        &fixture("grouping.yaml"),
        "--step",
        "C",
        "--run-dependencies",
    ]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "Step A ran",
            "Step B ran",
            "Seats onExit Count=2",
            "Seats onEnter Count=1",
            "Step C ran",
        ],
    );
    assert!(!stdout.contains("Step D ran"), "{stdout}");
}

#[test]
fn a_run_that_reenters_environments_has_one_working_directory() {
    let child = cli_command(&["run", &fixture("grouping.yaml"), "--preserve"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to execute openjd");
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    let dirs: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("Working directory preserved at: "))
        .collect();
    // One Session, so one working directory, although the run exits and
    // enters Seats again between Steps.
    assert_eq!(dirs.len(), 1, "{stdout}");
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn an_error_after_reentering_an_environment_still_prints_the_preserved_directory() {
    // Step B takes Seats' default, so the Session enters Seats again for it,
    // and its explicit Task value is outside its parameter space, which is
    // an error only when B runs.
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("late_error.yaml");
    std::fs::write(
        &job,
        r#"specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: LateError
jobEnvironments:
- name: Seats
  envParameterDefinitions: [{name: Count, type: INT, default: 1}]
  variables: {C: "{{Env.Param.Count}}"}
steps:
- name: A
  dependencies: [{environment: Seats, parameters: {Count: 2}}]
  script: {actions: {onRun: {command: python, args: ["-c", "print('Step A ran')"]}}}
- name: B
  dependencies: [{dependsOn: A}]
  parameterSpace:
    taskParameterDefinitions: [{name: T, type: INT, range: "1-2"}]
  script: {actions: {onRun: {command: python, args: ["-c", "print('Step B ran')"]}}}
"#,
    )
    .unwrap();
    let (code, stdout, stderr) = run_cli(&[
        "run",
        job.to_str().unwrap(),
        "--step",
        "B",
        "--run-dependencies",
        "--task-param",
        "T=9",
        "--preserve",
    ]);
    assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert!(stdout.contains("Step A ran"), "{stdout}");
    // The error still exits every entered Environment.
    assert_eq!(count(&stdout, "Entering Environment: Seats"), 2, "{stdout}");
    assert_eq!(count(&stdout, "Exiting Environment: Seats"), 2, "{stdout}");
    assert!(
        stderr.contains(
            "ERROR: Task parameter set 0: Parameter 'T' value '9' is not in the parameter space \
             range."
        ),
        "{stderr}"
    );
    let dirs: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("Working directory preserved at: "))
        .collect();
    assert_eq!(dirs.len(), 1, "{stdout}");
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn an_invalid_environment_file_is_named_in_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.yaml");
    std::fs::write(
        &bad,
        "specificationVersion: environment-2023-09\nenvironment: {name: E}\n",
    )
    .unwrap();
    let conda = fixture("conda-env-params.yaml");
    let job = fixture("grouping.yaml");
    let bad = bad.to_str().unwrap();
    for command in ["check", "run", "summary"] {
        let (code, _stdout, stderr) =
            run_cli(&[command, &job, "--environment", &conda, "--environment", bad]);
        assert_ne!(code, 0, "{command}");
        assert!(
            stderr.contains(&format!(
                "ERROR: Environment Template '{bad}': Model validation error: "
            )),
            "{command}: {stderr}"
        );
        assert!(
            stderr.contains("environment:\n\tmust have at least one of 'script' or 'variables'."),
            "{command}: {stderr}"
        );
    }
}

#[test]
fn a_failure_stops_the_run_before_the_next_step_reenters_environments() {
    let (code, stdout, stderr) = run_cli(&["run", &fixture("fail_first_group.yaml")]);
    assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(count(&stdout, "Seats onEnter"), 1, "{stdout}");
    assert!(!stdout.contains("Step B ran"), "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "Seats onEnter Count=2",
            "Step A failing",
            "Process exited with code: 3",
            "Seats onExit Count=2",
            "Session ended with errors.",
        ],
    );
}

#[test]
fn an_attached_environment_named_like_a_job_environment_keeps_its_own_values() {
    // §3.2.1 item 4: the entries name the Job Environment, and the
    // attachment of the same name is entered with its default. Values are
    // picked by (document, name).
    let seats = fixture("seats-env.yaml");
    let (code, stdout, stderr) = run_cli(&[
        "run",
        &fixture("grouping.yaml"),
        "--environment",
        &seats,
        "--step",
        "A",
    ]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_in_order(
        &stdout,
        &[
            "Entering Environment: Seats",
            "\tCount(INT) = 99",
            "Attached Seats onEnter Count=99",
            "Entering Environment: Seats",
            "\tCount(INT) = 2",
            "Seats onEnter Count=2",
            "Step A ran",
        ],
    );
    assert_eq!(
        stderr,
        format!(
            "WARNING: {seats} -> environment -> name: Environment 'Seats' has the same name as a \
             jobEnvironments entry of the Job Template, so no 'environment:' dependency of the \
             Job Template can reach it (Template Schemas §3.2.1 item 4).\n"
        )
    );
}

#[test]
fn with_both_environments_attached_each_step_chooses_docker() {
    // The sample's Conda entries have `enableIf: "'Docker' not in
    // Job.EnvNames"`, so with Docker attached they are disabled. Conda's
    // activation is ALWAYS, so the Step takes its defaults (no packages),
    // for which Conda's own enableIf is false (Template Schemas §4 item
    // 6): its Sessions do not enter Conda at all, and Docker gets the
    // entry's values.
    let conda = fixture("conda-env-params.yaml");
    let docker = fixture("docker-env-params.yaml");
    let (code, stdout, stderr) = run_video(&[
        "--environment",
        &conda,
        "--environment",
        &docker,
        "--step",
        "SolveSfM",
    ]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_in_order(
        &stdout,
        &[
            "Entering Environment: Docker",
            "\tGpus(STRING) = all",
            "\tImage(STRING) = docker.io/colmap/colmap:latest",
            "Docker onEnter: Image='docker.io/colmap/colmap:latest' Gpus='all'",
            "SolveSfM task: conda=None docker='docker.io/colmap/colmap:latest'",
            "Docker onExit: stopping docker.io/colmap/colmap:latest",
        ],
    );
    assert!(!stdout.contains("Entering Environment: Conda"), "{stdout}");
    assert!(!stdout.contains("Conda onExit"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn uri_path_values_are_allowed_as_for_job_parameters() {
    // The Job Template lists EXPR, and `openjd run` allows URI path values
    // for Job Parameters, so the Docker entry's Mounts may hold one.
    let docker = fixture("docker-env-params.yaml");
    let (code, stdout, stderr) = run_video(&[
        "--environment",
        &docker,
        "--step",
        "DecodeFrames",
        "-p",
        "InputVideo=s3://bucket/plate.mp4",
    ]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("Docker onEnter: RawMounts=['s3://bucket/plate.mp4', "),
        "{stdout}"
    );
}

// ── Submission errors fail before any Session ───────────────────────

const LICENSES_MISSING: &str = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\t\
    names Environment 'Licenses', which is not a jobEnvironments entry of the Job Template, and \
    none of the attached Environment Templates ({conda}) defines it. The entry is required; set \
    'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item \
    2).";

const BOGUS_KEY: &str = "JobTemplate -> steps[0] -> dependencies[1] -> parameters -> Bogus:\n\t\
    Environment 'Conda' ({conda}) declares no parameter 'Bogus'. Declared: Packages, Channels.";

fn expected_submission_errors(conda: &str, errors: &[&str]) -> String {
    let n = errors.len();
    let body: Vec<String> = errors.iter().map(|e| e.replace("{conda}", conda)).collect();
    format!(
        "Model validation error: {n} validation error{} for Submission\n{}",
        if n == 1 { "" } else { "s" },
        body.join("\n")
    )
}

#[test]
fn a_missing_required_environment_and_an_undeclared_key_fail_the_run() {
    let conda = fixture("conda-env-params.yaml");
    let (code, stdout, stderr) = run_cli(&[
        "run",
        &fixture("requires_licenses.yaml"),
        "--environment",
        &conda,
    ]);
    assert_ne!(code, 0);
    assert!(!stdout.contains("Session start"), "{stdout}");
    let expected = expected_submission_errors(&conda, &[LICENSES_MISSING, BOGUS_KEY]);
    // Like every job-creation error, the Submission errors are followed by
    // the Job Template's help text.
    let expected = format!(
        "{expected}\n\nusage: openjd run {} [arguments]\n\nJob: RequiresLicenses\n",
        fixture("requires_licenses.yaml")
    );
    assert!(
        stderr.contains(&expected),
        "expected:\n{expected}\nin:\n{stderr}"
    );
}

#[test]
fn check_with_environments_reports_the_same_errors_as_run() {
    let job = fixture("requires_licenses.yaml");
    let conda = fixture("conda-env-params.yaml");
    let licenses = fixture("licenses-env.yaml");

    // Without Environment Templates, check cannot know which Environments
    // the queue attaches, so the template passes.
    let (code, stdout, stderr) = run_cli(&["check", &job]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("passes validation checks"), "{stdout}");

    let (code, _stdout, stderr) = run_cli(&["check", &job, "--environment", &conda]);
    assert_ne!(code, 0);
    let expected = expected_submission_errors(&conda, &[LICENSES_MISSING, BOGUS_KEY]);
    assert!(
        stderr.contains(&expected),
        "expected:\n{expected}\nin:\n{stderr}"
    );

    let (code, _stdout, stderr) = run_cli(&[
        "check",
        &job,
        "--environment",
        &conda,
        "--environment",
        &licenses,
    ]);
    assert_ne!(code, 0);
    let expected = expected_submission_errors(&conda, &[BOGUS_KEY]);
    assert!(
        stderr.contains(&expected),
        "expected:\n{expected}\nin:\n{stderr}"
    );
}

#[test]
fn check_with_environments_passes_the_sample_and_prints_warnings() {
    let conda = fixture("conda-env-params.yaml");
    let docker = fixture("docker-env-params.yaml");
    let video = fixture("video-sfm-composite.yaml");
    let (code, stdout, stderr) = run_cli(&["check", &video, "--environment", &conda]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("passes validation checks"), "{stdout}");
    assert_eq!(count(&stderr, "WARNING: "), 4, "{stderr}");
    assert!(
        stderr.contains(&format!(
            "WARNING: JobTemplate -> steps[0] -> dependencies[1] -> environment: names \
             Environment 'Docker', which is not a jobEnvironments entry of the Job Template, and \
             none of the attached Environment Templates ({conda}) defines it. The entry has \
             'required: false', so it has no effect.\n"
        )),
        "{stderr}"
    );

    let (code, _stdout, stderr) = run_cli(&[
        "check",
        &video,
        "--environment",
        &conda,
        "--environment",
        &docker,
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn check_with_environments_does_not_resolve_job_environment_entries_externally() {
    // grouping.yaml's entries name its own Job Environment, which template
    // validation checked; attaching an unrelated Environment changes
    // nothing.
    let conda = fixture("conda-env-params.yaml");
    let (code, stdout, stderr) =
        run_cli(&["check", &fixture("grouping.yaml"), "--environment", &conda]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("passes validation checks"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn check_with_environments_reports_a_job_parameter_conflict() {
    // The Job Parameter merge (Template Schemas §1.2.1) needs no values,
    // so check reports a conflict that `openjd run` would.
    let dir = tempfile::tempdir().unwrap();
    let env = dir.path().join("conflict.yaml");
    std::fs::write(
        &env,
        "specificationVersion: environment-2023-09\n\
         parameterDefinitions: [{name: J, type: INT}]\n\
         environment: {name: Z, variables: {A: a}}\n",
    )
    .unwrap();
    let job = templates_dir().join("basic.yaml");
    let (code, _stdout, stderr) = run_cli(&[
        "check",
        job.to_str().unwrap(),
        "--environment",
        env.to_str().unwrap(),
    ]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains(
            "ERROR: Compatibility error: Parameter 'J' has conflicting types: 'INT' in \
             EnvironmentTemplate 'Z' and 'STRING' in JobTemplate"
        ),
        "{stderr}"
    );
}

#[test]
fn check_with_environments_allows_uri_path_values_as_run_does() {
    let docker = fixture("docker-env-params.yaml");
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("uri.yaml");
    std::fs::write(
        &job,
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: S\n  dependencies:\n  - environment: Docker\n    \
         parameters: {Mounts: [\"s3://bucket/plate.mp4\"]}\n  \
         script: {actions: {onRun: {command: python, args: [\"-c\", \"print(1)\"]}}}\n",
    )
    .unwrap();
    let job = job.to_str().unwrap();
    let (code, stdout, stderr) = run_cli(&["check", job, "--environment", &docker]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("passes validation checks"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    let (code, stdout, stderr) = run_cli(&["run", job, "--environment", &docker]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("Docker onEnter: RawMounts=['s3://bucket/plate.mp4']"),
        "{stdout}"
    );
}

/// Write `contents` to `name` in `dir` and return the path.
fn write_file(dir: &tempfile::TempDir, name: &str, contents: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn check_reports_the_new_template_validation_errors() {
    // An unknown activation is a decode error, which, as for every enum
    // field, names the value but not the field.
    let dir = tempfile::tempdir().unwrap();
    let unknown = write_file(
        &dir,
        "unknown.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: E\n  activation: all\n  variables: {V: v}\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, _stdout, stderr) = run_cli(&["check", &unknown]);
    assert_ne!(code, 0);
    assert_eq!(
        stderr,
        "ERROR: Validation error: 'jobtemplate-2023-09' failed checks: unknown variant `all`, \
         expected `ALWAYS` or `ON_REQUEST`\n"
    );
    // activation and hostRequirements on a Step Environment, each at the
    // field it names (Template Schemas §4 items 4 and 5).
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: E\n  variables: {V: v}\n\
         steps:\n- name: S\n  stepEnvironments:\n  - name: StepEnv\n    activation: ALWAYS\n    \
         hostRequirements:\n      attributes: [{name: attr.worker.os.family, anyOf: [linux]}]\n    \
         variables: {V: v}\n  dependencies:\n  - environment: E\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, _stdout, stderr) = run_cli(&["check", &job]);
    assert_ne!(code, 0);
    assert_eq!(
        stderr,
        "ERROR: Model validation error: 2 validation errors for JobTemplate\n\
         steps[0] -> stepEnvironments[0] -> activation:\n\tis not permitted on a stepEnvironments \
         entry, which only its own Step's Sessions enter.\n\
         steps[0] -> stepEnvironments[0] -> hostRequirements:\n\tis not permitted on a \
         stepEnvironments entry, whose Step has hostRequirements of its own.\n"
    );
}

#[test]
fn check_rejects_env_params_without_expr() {
    // A document that lists ENV_PARAMS must also list EXPR (Template
    // Schemas §1.1 item 3; §1.2 item 3 for an Environment Template), and
    // it is the only error reported: the entry below names a key its Job
    // Environment does not declare, and the Environment Template's
    // enableIf does not fit bool, neither of which is reported with it.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS]\nname: J\n\
         jobEnvironments: [{name: E, variables: {V: v}}]\n\
         steps:\n- name: S\n  dependencies: [{environment: E, parameters: {Nope: 1}}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, _stdout, stderr) = run_cli(&["check", &job]);
    assert_ne!(code, 0);
    assert_eq!(
        stderr,
        "ERROR: Model validation error: 1 validation error for JobTemplate\nextensions:\n\t\
         ENV_PARAMS requires EXPR; both must be listed in the template's `extensions` (Template \
         Schemas §1.1 item 3).\n"
    );
    let env = write_file(
        &dir,
        "env.yaml",
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS]\n\
         environment: {name: E, enableIf: 1, variables: {V: v}}\n",
    );
    let (code, _stdout, stderr) = run_cli(&["check", &env]);
    assert_ne!(code, 0);
    assert_eq!(
        stderr,
        "ERROR: Model validation error: 1 validation error for EnvironmentTemplate\nextensions:\n\t\
         ENV_PARAMS requires EXPR; both must be listed in the template's `extensions` (Template \
         Schemas §1.2 item 3).\n"
    );
}

#[test]
fn check_reports_expression_field_errors_for_enable_if() {
    // An entry's enableIf is an expression field of type bool (Template
    // Schemas §3.2 constraint 9, §7.5): written without {{ }}, of a type
    // that fits bool with no conversion, and never null. An Environment's
    // own enableIf (§4 item 6) is an expression field of type bool too.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: Mode, type: STRING, default: 'on'}]\n\
         jobEnvironments:\n- {name: A, variables: {V: v}}\n- {name: B, variables: {V: v}}\n\
         - {name: C, variables: {V: v}}\n- {name: D, enableIf: 1, variables: {V: v}}\n\
         steps:\n- name: S\n  dependencies:\n  - {environment: A, enableIf: \"{{ true }}\"}\n  \
         - {environment: B, enableIf: Param.Mode}\n  - {environment: C, enableIf: null}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, _stdout, stderr) = run_cli(&["check", &job]);
    assert_ne!(code, 0);
    assert_eq!(
        stderr,
        "ERROR: Model validation error: 4 validation errors for JobTemplate\n\
         jobEnvironments[3] -> enableIf:\n\tthe integer scalar 1 does not fit type bool: an \
         expression field of type bool takes a boolean scalar or a string holding one expression \
         (Template Schemas §7.5).\n\
         steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Set literals are not \
         supported. An expression field takes an expression without '{{ }}' (Template Schemas \
         §7.5).\n  {{ true }}\n  ^~~~~~~~~~\n\
         steps[0] -> dependencies[1] -> enableIf:\n\thas type string, which does not fit type \
         bool; an expression field converts no type except int to float (Template Schemas \
         §7.5).\n\
         steps[0] -> dependencies[2] -> enableIf:\n\tmust not be null: an expression field of \
         type bool takes a boolean scalar or a string holding one expression (Template Schemas \
         §7.5).\n"
    );
}

/// A Job Template whose Job Environment `Own` wraps, with one Step that
/// has `dependencies` (YAML, indented for the Step).
fn wrapping_job(dir: &tempfile::TempDir, dependencies: &str) -> String {
    write_file(
        dir,
        "wrap-job.yaml",
        &format!(
            "specificationVersion: jobtemplate-2023-09\n\
             extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\nname: J\njobEnvironments:\n\
             - name: Own\n  script:\n    actions:\n      onWrapEnvEnter: {{command: x}}\n      \
             onWrapTaskRun: {{command: x}}\n      onWrapEnvExit: {{command: x}}\n\
             steps:\n- name: S\n{dependencies}  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ),
    )
}

#[test]
fn check_with_environments_applies_the_single_layer_rule_to_attached_environments() {
    // The Docker sample wraps and is ON_REQUEST: a Step that requests
    // it, in a Job Template whose own Job Environment wraps, would enter
    // two wrapping Environments (RFC 0008).
    let docker = fixture("docker-env-params.yaml");
    let dir = tempfile::tempdir().unwrap();
    let unrequested = wrapping_job(&dir, "");
    let (code, stdout, stderr) = run_cli(&["check", &unrequested, "--environment", &docker]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");

    let requested = wrapping_job(
        &dir,
        "  dependencies:\n  - environment: Docker\n    parameters: {Image: i}\n",
    );
    let expected = format!(
        "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0]:\n\t\
         its Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, \
         onWrapTaskRun, onWrapEnvExit): 'Docker' ({docker}), 'Own'. A Session may have at most \
         one Environment defining wrap hooks (RFC 0008)."
    );
    for command in ["check", "run"] {
        let (code, _stdout, stderr) = run_cli(&[command, &requested, "--environment", &docker]);
        assert_ne!(code, 0, "{command}");
        assert!(
            stderr.contains(&expected),
            "{command}: expected:\n{expected}\nin:\n{stderr}"
        );
    }
}

#[test]
fn check_with_environments_reports_a_literal_host_requirements_conflict() {
    // The Docker sample requires a Linux host, a literal; a Step that
    // requests it and requires Windows cannot be satisfied (§3.3.3).
    let docker = fixture("docker-env-params.yaml");
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "windows.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: S\n  hostRequirements:\n    \
         attributes: [{name: attr.worker.os.family, anyOf: [windows]}]\n  \
         dependencies:\n  - environment: Docker\n    parameters: {Image: i}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let expected = format!(
        "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0]:\n\t\
         Step 'S': no host can satisfy the combined host requirements for attribute \
         'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf \
         values of JobTemplate -> steps[0] -> \
         hostRequirements and {docker} -> environment -> hostRequirements (Environment \
         'Docker') have no value in common. Template Schemas §3.3.3."
    );
    for command in ["check", "summary"] {
        let (code, _stdout, stderr) = run_cli(&[command, &job, "--environment", &docker]);
        assert_ne!(code, 0, "{command}");
        assert!(
            stderr.contains(&expected),
            "{command}: expected:\n{expected}\nin:\n{stderr}"
        );
    }
}

#[test]
fn check_with_environments_names_a_disabled_entry_in_the_missing_parameters_error() {
    let dir = tempfile::tempdir().unwrap();
    let env = write_file(
        &dir,
        "req.yaml",
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment:\n  name: Req\n  envParameterDefinitions: [{name: N, type: STRING}]\n  \
         variables: {V: v}\n",
    );
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: S\n  dependencies:\n  - environment: Req\n    enableIf: false\n    \
         parameters: {N: x}\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, _stdout, stderr) = run_cli(&["check", &job, "--environment", &env]);
    assert_ne!(code, 0);
    // The whole error: exactly one, with the note.
    let expected = format!(
        "ERROR: Model validation error: 1 validation error for Submission\n\
         JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
         parameter without a default of each Environment its Sessions enter (every attached \
         Environment with activation ALWAYS, and each one with activation ON_REQUEST that the Step \
         requests; Template Schemas §3.2.1); missing: 'N' of Environment 'Req' ({env}) (the \
         Step's 'environment:' dependency for it is disabled by its enableIf, so it supplies \
         nothing; Template Schemas §3.2 item 5)."
    );
    assert!(
        stderr.starts_with(&format!("{expected}\n")),
        "expected:\n{expected}\nat the start of:\n{stderr}"
    );
}

#[test]
fn run_step_enters_only_the_selected_steps_environment_set() {
    // C does not request the ON_REQUEST Environment, A does.
    let template = fixture("activation_grouping.yaml");
    let (code, stdout, stderr) = run_cli(&["run", &template, "--step", "C"]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_in_order(&stdout, &["Always onEnter", "Step C ran ON_REQUEST=unset"]);
    assert!(!stdout.contains("OnRequest onEnter"), "{stdout}");
    let (code, stdout, stderr) = run_cli(&["run", &template, "--step", "A"]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_in_order(
        &stdout,
        &[
            "Always onEnter",
            "OnRequest onEnter",
            "Step A ran ON_REQUEST=set",
            "OnRequest onExit",
        ],
    );
}

#[test]
fn check_reports_a_step_dependency_without_env_params() {
    // §3.2 item 1: `step:` is the ENV_PARAMS synonym of `dependsOn:`.
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("step_form.yaml");
    let template = |exts: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\n{exts}name: J\nsteps:\n\
             - name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
             - name: B\n  dependencies: [{{step: A}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    std::fs::write(&job, template("")).unwrap();
    let job = job.to_str().unwrap();
    let (code, _stdout, stderr) = run_cli(&["check", job]);
    assert_ne!(code, 0);
    assert_eq!(
        stderr,
        "ERROR: Model validation error: 1 validation error for JobTemplate\nsteps[1] -> \
         dependencies[0]:\n\tthe 'step' form of a dependency requires the ENV_PARAMS extension.\n"
    );
    std::fs::write(job, template("extensions: [ENV_PARAMS, EXPR]\n")).unwrap();
    let (code, stdout, stderr) = run_cli(&["check", job]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("passes validation checks"), "{stdout}");
}

#[test]
fn check_rejects_environment_with_an_environment_template() {
    let conda = fixture("conda-env-params.yaml");
    let (code, _stdout, stderr) = run_cli(&["check", &conda, "--environment", &conda]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains(
            "--environment applies only when checking a Job Template; this is an Environment \
             Template."
        ),
        "{stderr}"
    );
}

// ── openjd summary ──────────────────────────────────────────────────

#[test]
fn summary_shows_each_steps_effective_values() {
    let conda = fixture("conda-env-params.yaml");
    let video = fixture("video-sfm-composite.yaml");
    let (code, stdout, stderr) = run_cli(&["summary", &video, "--environment", &conda]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // Each Step's Sessions enter Conda; SolveSfM's own GPU requirement is
    // its combined host requirements.
    let block = |step: &str, tasks: usize, packages: &str, channels: &str| {
        format!(
            "'{step}' ({tasks} total Tasks)\n  Session environments:\n    - Conda ({conda})\n  \
             Environment parameters:\n    Conda ({conda}):\n      \
             - Channels (STRING): {channels}\n      - Packages (STRING): {packages}\n"
        )
    };
    for expected in [
        block("DecodeFrames", 1, "ffmpeg=7", "conda-forge"),
        block("SolveSfM", 1, "colmap=3.11", "conda-forge")
            .replace("Tasks)\n", "Tasks)\n  1 dependencies\n")
            .replace(
                "  Environment parameters:",
                "  Host requirements:\n    - amount.worker.gpu: min 1\n  Environment parameters:",
            ),
        block(
            "Composite",
            24,
            "maya=2025 mtoa",
            "studio-internal conda-forge",
        )
        .replace(
            "Tasks)\n",
            "Tasks)\n  Task parameters:\n    - StartFrame (INT)\n  1 dependencies\n",
        ),
        block("EncodeVideo", 1, "ffmpeg=7", "conda-forge")
            .replace("Tasks)\n", "Tasks)\n  1 dependencies\n"),
    ] {
        assert!(
            stdout.contains(&expected),
            "expected:\n{expected}\nin:\n{stdout}"
        );
    }
    // The environment listings stay those of the Job Template.
    assert!(stdout.contains("Total environments: 0"), "{stdout}");
    assert_eq!(count(&stderr, "WARNING: "), 4, "{stderr}");

    let (code, stdout, stderr) = run_cli(&[
        "summary",
        &video,
        "--environment",
        &conda,
        "--output",
        "json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["steps"][2]["environment_parameters"],
        serde_json::json!([{
            "environment": "Conda",
            "document": conda,
            "values": [
                {"name": "Channels", "type": "STRING", "value": "studio-internal conda-forge"},
                {"name": "Packages", "type": "STRING", "value": "maya=2025 mtoa"},
            ],
        }])
    );

    let (code, stdout, stderr) = run_cli(&[
        "summary",
        &video,
        "--environment",
        &conda,
        "--step",
        "SolveSfM",
        "--output",
        "json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["environment_parameters"],
        serde_json::json!([{
            "environment": "Conda",
            "document": conda,
            "values": [
                {"name": "Channels", "type": "STRING", "value": "conda-forge"},
                {"name": "Packages", "type": "STRING", "value": "colmap=3.11"},
            ],
        }])
    );
    // The sample writes `step: DecodeFrames`; the summary reads the Job,
    // where it is a dependency on that Step like any other.
    assert_eq!(
        json["dependencies"],
        serde_json::json!([{"step_name": "DecodeFrames"}])
    );
}

/// POSIX absolute Job Parameter paths keep the expected output short.
#[cfg(unix)]
#[test]
fn summary_of_one_step_lists_each_environment_in_session_order() {
    let conda = fixture("conda-env-params.yaml");
    let docker = fixture("docker-env-params.yaml");
    let video = fixture("video-sfm-composite.yaml");
    let (code, stdout, stderr) = run_cli(&[
        "summary",
        &video,
        "--environment",
        &conda,
        "--environment",
        &docker,
        "--step",
        "SolveSfM",
        "-p",
        "WorkDir=/w",
        "-p",
        "InputVideo=/in.mp4",
        "-p",
        "MayaScene=/s.ma",
        "-p",
        "OutputVideo=/out.mp4",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // The Conda entry's enableIf disabled it, so Conda takes its defaults,
    // for which Conda's own enableIf is false: the Step does not enter it,
    // and the summary lists it as disabled (Template Schemas §4 item 6).
    // The host requirements combine the Step's own GPU amount with
    // Docker's GPU amount (resolved with the Step's Gpus value) and its os
    // family (Template Schemas §3.3.3).
    let expected = format!(
        "\nSession environments:\n  - Docker ({docker})\nDisabled environments:\n  - Conda \
         ({conda})\nHost requirements:\n  - amount.worker.gpu: min 1\n  - \
         attr.worker.os.family: anyOf linux\n\nEnvironment parameters:\n  Docker ({docker}):\n    \
         - Gpus (STRING): all\n    - Image (STRING): docker.io/colmap/colmap:latest\n    - \
         Mounts (LIST[PATH]): [\"/in.mp4\", \"/s.ma\", \"/w\", \"/out.mp4\"]\n\n"
    );
    assert!(
        stdout.ends_with(&expected),
        "expected:\n{expected}\nat the end of:\n{stdout}"
    );

    // JSON: the same Environment set and combined requirements.
    let mut argv = vec![
        "summary",
        video.as_str(),
        "--environment",
        &docker,
        "--step",
        "SolveSfM",
        "--output",
        "json",
    ];
    for p in [
        "WorkDir=/w",
        "InputVideo=/in.mp4",
        "MayaScene=/s.ma",
        "OutputVideo=/out.mp4",
    ] {
        argv.extend(["-p", p]);
    }
    let (code, stdout, stderr) = run_cli(&argv);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["session_environments"],
        serde_json::json!([{"name": "Docker", "source": docker}])
    );
    // No Environment is disabled for it, so the key is absent.
    assert!(json.get("disabled_environments").is_none(), "{json}");
    assert_eq!(
        json["host_requirements"],
        serde_json::json!({
            "amounts": [{"name": "amount.worker.gpu", "min": 1.0}],
            "attributes": [{"name": "attr.worker.os.family", "anyOf": ["linux"]}],
        })
    );
    // DecodeFrames has no requirement of its own and an empty Gpus value,
    // so Docker's GPU minimum resolves to 0 for it.
    let at = argv.iter().position(|a| *a == "SolveSfM").unwrap();
    argv[at] = "DecodeFrames";
    let (code, stdout, stderr) = run_cli(&argv);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["host_requirements"],
        serde_json::json!({
            "amounts": [{"name": "amount.worker.gpu", "min": 0.0}],
            "attributes": [{"name": "attr.worker.os.family", "anyOf": ["linux"]}],
        })
    );
}

#[test]
fn summary_shows_which_steps_enter_an_on_request_environment() {
    let (code, stdout, stderr) = run_cli(&[
        "summary",
        &fixture("activation_grouping.yaml"),
        "--output",
        "json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sets: Vec<serde_json::Value> = json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["session_environments"].clone())
        .collect();
    let env = |name: &str| serde_json::json!({"name": name, "source": "JobTemplate"});
    assert_eq!(
        sets,
        [
            serde_json::json!([env("Always"), env("OnRequest")]),
            serde_json::json!([env("Always"), env("OnRequest")]),
            serde_json::json!([env("Always")]),
        ]
    );
    // Without host requirements the key is absent.
    assert!(
        json["steps"][0].get("host_requirements").is_none(),
        "{stdout}"
    );
}

#[test]
fn summary_lists_step_environments_and_every_requirement_form() {
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "hr.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: Hr\n\
         steps:\n- name: S1\n  script: {actions: {onRun: {command: x}}}\n\
         - name: S2\n  stepEnvironments:\n  - name: Inner\n    variables: {V: v}\n  \
         hostRequirements:\n    amounts:\n    - {name: amount.worker.gpu, max: 2}\n    \
         - {name: amount.worker.vcpu, min: 2, max: 8}\n    attributes:\n    \
         - {name: attr.worker.os.family, anyOf: [linux, macos]}\n    \
         - {name: attr.custom.tags, allOf: [a, b]}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let requirements = "Host requirements:\n  - amount.worker.gpu: max 2\n  - amount.worker.vcpu: \
         min 2, max 8\n  - attr.worker.os.family: anyOf linux, macos\n  - attr.custom.tags: \
         allOf a, b\n";
    let (code, stdout, stderr) = run_cli(&["summary", &job]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let s2 = format!(
        "2. 'S2' (1 total Tasks)\n  1 environments\n  Session environments:\n    - Inner \
         (stepEnvironments)\n  {}\n",
        requirements.trim_end().replace('\n', "\n  ")
    );
    for expected in [
        "1. 'S1' (1 total Tasks)\n  Session environments:\n    (none)\n\n",
        s2.as_str(),
    ] {
        assert!(
            stdout.contains(expected),
            "expected:\n{expected}\nin:\n{stdout}"
        );
    }
    // --step selects the Step's own set, not the first Step's.
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--step", "S2"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // The whole output, so the blank lines between sections are pinned.
    let expected = format!(
        "\n--- Summary for Step 'S2' in Job 'Hr' ---\n\nTotal tasks: 1\nTotal task parameters: \
         0\nTotal environments: 1\n\nEnvironments:\n- Inner (from 'S2')\n\nSession \
         environments:\n  - Inner (stepEnvironments)\n{requirements}\n"
    );
    assert_eq!(stdout, expected);
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--step", "S2", "--output", "json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["session_environments"],
        serde_json::json!([{"name": "Inner", "source": "stepEnvironments"}])
    );
    // Absent bounds and lists are omitted, not null.
    assert_eq!(
        json["host_requirements"],
        serde_json::json!({
            "amounts": [
                {"name": "amount.worker.gpu", "max": 2.0},
                {"name": "amount.worker.vcpu", "min": 2.0, "max": 8.0},
            ],
            "attributes": [
                {"name": "attr.worker.os.family", "anyOf": ["linux", "macos"]},
                {"name": "attr.custom.tags", "allOf": ["a", "b"]},
            ],
        })
    );
}

#[test]
fn summary_shows_an_attached_environments_requirements_without_env_params_in_the_job() {
    // The Job Template lists no extension; the attached Environment does,
    // so its activation and hostRequirements apply and the summary shows them.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "plain.yaml",
        "specificationVersion: jobtemplate-2023-09\nname: Plain\nsteps:\n- name: S\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let env = write_file(
        &dir,
        "gpu.yaml",
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  \
         name: G\n  hostRequirements:\n    amounts: [{name: amount.worker.gpu, min: 1}]\n  \
         variables: {V: v}\n",
    );
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--environment", &env]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let expected = format!(
        "1. 'S' (1 total Tasks)\n  Session environments:\n    - G ({env})\n  Host \
         requirements:\n    - amount.worker.gpu: min 1\n"
    );
    assert!(
        stdout.contains(&expected),
        "expected:\n{expected}\nin:\n{stdout}"
    );
}

#[test]
fn summary_json_omits_an_empty_requirement_list() {
    // An empty `amounts` list is valid (Template Schemas §3.3 sets no
    // minimum length); the JSON lists only non-empty lists.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "empty.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: E\n\
         steps:\n- name: S\n  hostRequirements:\n    amounts: []\n    \
         attributes: [{name: attr.worker.os.family, anyOf: [linux]}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--output", "json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["steps"][0]["host_requirements"],
        serde_json::json!({"attributes": [{"name": "attr.worker.os.family", "anyOf": ["linux"]}]})
    );
    let job = job.replace("empty.yaml", "empty-attrs.yaml");
    std::fs::write(
        &job,
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: E\n\
         steps:\n- name: S\n  hostRequirements:\n    \
         amounts: [{name: amount.worker.gpu, min: 1}]\n    attributes: []\n  \
         script: {actions: {onRun: {command: x}}}\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--output", "json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["steps"][0]["host_requirements"],
        serde_json::json!({"amounts": [{"name": "amount.worker.gpu", "min": 1.0}]})
    );
}

#[test]
fn summary_without_env_params_is_unchanged() {
    let (code, stdout, stderr) = run_cli(&[
        "summary",
        templates_dir().join("basic.yaml").to_str().unwrap(),
        "-p",
        "J=x",
        "--output",
        "json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(!stdout.contains("environment_parameters"), "{stdout}");
    assert!(!stdout.contains("session_environments"), "{stdout}");
    assert!(!stdout.contains("host_requirements"), "{stdout}");
    let basic = templates_dir().join("basic.yaml");
    let basic = basic.to_str().unwrap();
    for extra in [&["--step", "First"][..], &[]] {
        let mut argv = vec!["summary", basic, "-p", "J=x"];
        argv.extend_from_slice(extra);
        let (code, stdout, stderr) = run_cli(&argv);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("--- Summary for"), "{stdout}");
        assert!(!stdout.contains("Environment parameters"), "{stdout}");
        assert!(!stdout.contains("Session environments"), "{stdout}");
    }
}

// ── Context help and the default extension list ─────────────────────

#[test]
fn context_help_accepts_env_params_and_wrap_actions_templates() {
    for template in [
        fixture("video-sfm-composite.yaml"),
        templates_dir()
            .join("wrap_simple_job.yaml")
            .to_str()
            .unwrap()
            .to_string(),
    ] {
        let (code, stdout, stderr) = run_cli(&["run", &template, "--help"]);
        assert_eq!(code, 0, "{template}: stderr: {stderr}");
        assert!(stdout.starts_with("usage: openjd run "), "{stdout}");
    }
}

#[test]
fn extensions_accepts_env_params() {
    let (code, stdout, stderr) = run_cli(&[
        "check",
        &fixture("grouping.yaml"),
        "--extensions",
        "ENV_PARAMS,EXPR",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("passes validation checks"), "{stdout}");
    let (code, _stdout, stderr) =
        run_cli(&["check", &fixture("grouping.yaml"), "--extensions", "EXPR"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains(
            "Model validation error: 1 validation error for JobTemplate\nextensions:\n\t\
             Unsupported extension names: ENV_PARAMS"
        ),
        "{stderr}"
    );
}

// ── Environment enableIf (Template Schemas §4 item 6) ───────────────

/// A Job Template whose Environments use `enableIf` in every form: `Off`
/// (ALWAYS, scalar false), `Unasked` (ON_REQUEST, scalar false, never
/// requested), `Later` (an expression, decided at job creation), and a
/// Step Environment `Scratch` with the scalar false.
const ENABLE_IF_JOB: &str = "specificationVersion: jobtemplate-2023-09\n\
     extensions: [ENV_PARAMS, EXPR]\nname: J\n\
     parameterDefinitions: [{name: Use, type: STRING, default: 'no'}]\n\
     jobEnvironments:\n\
     - {name: Off, enableIf: false, script: {actions: {onEnter: {command: echo, args: [ENTER, Off]}}}}\n\
     - {name: Unasked, activation: ON_REQUEST, enableIf: false, variables: {V: x}}\n\
     - {name: Later, enableIf: \"Param.Use == 'yes'\", script: {actions: {onEnter: {command: echo, args: [ENTER, Later]}}}}\n\
     steps:\n- name: S\n  stepEnvironments:\n  \
     - {name: Scratch, enableIf: false, script: {actions: {onEnter: {command: echo, args: [ENTER, Scratch]}}}}\n  \
     script: {actions: {onRun: {command: echo, args: [TASK]}}}\n\
     - name: T\n  script: {actions: {onRun: {command: echo, args: [TASK]}}}\n";

#[test]
fn check_lists_environments_disabled_by_a_scalar_false() {
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(&dir, "job.yaml", ENABLE_IF_JOB);
    let (code, stdout, stderr) = run_cli(&["check", &job]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // `Unasked` is not listed, since no Step would enter it; `Later` is
    // decided when the Job is created.
    assert_eq!(
        stdout,
        format!(
            "Step 'S': enableIf: false disables Environments 'Off' (jobEnvironments), 'Scratch' \
             (stepEnvironments) for its Sessions.\n\
             Step 'T': enableIf: false disables Environment 'Off' (jobEnvironments) for its \
             Sessions.\n\
             Template at '{job}' passes validation checks.\n"
        )
    );
    // An attached Environment with the scalar false is listed with its
    // path.
    let env = write_file(
        &dir,
        "env.yaml",
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Att, enableIf: false, variables: {V: x}}\n",
    );
    let only_t = write_file(
        &dir,
        "t.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: T\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["check", &only_t, "--environment", &env]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!(
            "Step 'T': enableIf: false disables Environment 'Att' ({env}) for its Sessions.\n\
             Template at '{only_t}' passes validation checks.\n"
        )
    );
}

#[test]
fn run_does_not_enter_a_disabled_environment() {
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(&dir, "job.yaml", ENABLE_IF_JOB);
    let (code, stdout, stderr) = run_cli(&["run", &job, "--step", "S"]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for name in ["Off", "Later", "Scratch"] {
        assert!(!stdout.contains(&format!("ENTER {name}")), "{stdout}");
    }
    assert!(stdout.contains("TASK"), "{stdout}");
    // The expression enables Later for this Job.
    let (code, stdout, stderr) = run_cli(&["run", &job, "--step", "S", "-p", "Use=yes"]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_in_order(&stdout, &["ENTER Later", "TASK"]);
    assert!(!stdout.contains("ENTER Off"), "{stdout}");
}

#[test]
fn summary_lists_the_environments_enable_if_disabled_for_each_step() {
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(&dir, "job.yaml", ENABLE_IF_JOB);
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--step", "S"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains(
            "Session environments:\n  (none)\nDisabled environments:\n  - Off (JobTemplate)\n  \
             - Later (JobTemplate)\n  - Scratch (stepEnvironments)\n"
        ),
        "{stdout}"
    );
    let (code, stdout, stderr) = run_cli(&[
        "summary", &job, "--step", "S", "-p", "Use=yes", "--output", "json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["session_environments"],
        serde_json::json!([{"name": "Later", "source": "JobTemplate"}])
    );
    assert_eq!(
        json["disabled_environments"],
        serde_json::json!([
            {"name": "Off", "source": "JobTemplate"},
            {"name": "Scratch", "source": "stepEnvironments"},
        ])
    );
}

#[test]
fn check_lists_requested_on_request_environments_and_skips_shadowed_attachments() {
    let dir = tempfile::tempdir().unwrap();
    // A requested ON_REQUEST Job Environment with the scalar false.
    let t1 = write_file(
        &dir,
        "t1.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: Req, activation: ON_REQUEST, enableIf: false, variables: {V: x}}\n\
         steps:\n- name: S\n  dependencies: [{environment: Req}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["check", &t1]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!(
            "Step 'S': enableIf: false disables Environment 'Req' (jobEnvironments) for its \
             Sessions.\nTemplate at '{t1}' passes validation checks.\n"
        )
    );
    // A requested ON_REQUEST attachment with the scalar false is listed;
    // one a Job Environment of the same name shadows is not, since the
    // entry names the Job Environment (§3.2.1 item 4).
    let att = write_file(
        &dir,
        "att.yaml",
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Att, activation: ON_REQUEST, enableIf: false, variables: {V: x}}\n",
    );
    let requester = |shadow: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             {shadow}steps:\n- name: S\n  dependencies: [{{environment: Att}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let t2 = write_file(&dir, "t2.yaml", &requester(""));
    let (code, stdout, stderr) = run_cli(&["check", &t2, "--environment", &att]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!(
            "Step 'S': enableIf: false disables Environment 'Att' ({att}) for its Sessions.\n\
             Template at '{t2}' passes validation checks.\n"
        )
    );
    let t3 = write_file(
        &dir,
        "t3.yaml",
        &requester("jobEnvironments:\n- {name: Att, variables: {V: x}}\n"),
    );
    let (code, stdout, stderr) = run_cli(&["check", &t3, "--environment", &att]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!("Template at '{t3}' passes validation checks.\n")
    );
}

#[test]
fn job_summary_lists_each_steps_disabled_environments() {
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(&dir, "job.yaml", ENABLE_IF_JOB);
    let (code, stdout, stderr) = run_cli(&["summary", &job]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains(
            "1. 'S' (1 total Tasks)\n  Session environments:\n    (none)\n  Disabled \
             environments:\n    - Off (JobTemplate)\n    - Later (JobTemplate)\n    - Scratch \
             (stepEnvironments)\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "2. 'T' (1 total Tasks)\n  Session environments:\n    (none)\n  Disabled \
             environments:\n    - Off (JobTemplate)\n    - Later (JobTemplate)\n"
        ),
        "{stdout}"
    );
}

#[test]
fn check_does_not_list_environments_or_entries_whose_enable_if_is_an_expression() {
    // Each is decided when the Job is created: an ON_REQUEST Job
    // Environment requested only by an entry whose enableIf is an
    // expression, an attached Environment and a Step Environment whose
    // enableIf is the expression "false".
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: E, activation: ON_REQUEST, enableIf: false, variables: {V: x}}\n\
         steps:\n- name: S\n  dependencies: [{environment: E, enableIf: \"Job.Name == 'J'\"}]\n  \
         stepEnvironments:\n  - {name: M, enableIf: \"false\", variables: {V: x}}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let env = write_file(
        &dir,
        "env.yaml",
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: A, enableIf: \"false\", variables: {V: x}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["check", &job, "--environment", &env]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!("Template at '{job}' passes validation checks.\n")
    );
}

#[test]
fn check_lists_disabled_environments_in_session_order() {
    // Attached Environments first, then jobEnvironments, then the Step's
    // own stepEnvironments.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: Off, enableIf: false, variables: {V: x}}\n\
         steps:\n- name: S\n  stepEnvironments:\n  - {name: Mine, enableIf: false, variables: {V: x}}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let env = write_file(
        &dir,
        "att.yaml",
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Att, enableIf: false, variables: {V: x}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["check", &job, "--environment", &env]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!(
            "Step 'S': enableIf: false disables Environments 'Att' ({env}), 'Off' \
             (jobEnvironments), 'Mine' (stepEnvironments) for its Sessions.\n\
             Template at '{job}' passes validation checks.\n"
        )
    );
}

#[test]
fn check_does_not_list_an_on_request_environment_a_disabled_entry_names() {
    // An entry with enableIf: false requests nothing, so the Step would
    // not enter Req, and its enableIf: false disables nothing for it.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: Req, activation: ON_REQUEST, enableIf: false, variables: {V: x}}\n\
         steps:\n- name: S\n  dependencies: [{environment: Req, enableIf: false}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["check", &job]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!("Template at '{job}' passes validation checks.\n")
    );
}

// ── Plan 4: disabled Environments and RANGE_EXPR parameters ─────────

#[test]
fn a_disabled_environment_needs_no_values() {
    // Template Schemas §3.2.2: an Environment disabled for a Step is not
    // held to constraint 7, and its values are not checked. Lic's Product
    // has no default and no Step supplies it; S supplies an out-of-range
    // Seats.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: Use, type: STRING, default: 'no'}]\n\
         jobEnvironments:\n- name: Lic\n  enableIf: \"Param.Use == 'yes'\"\n  \
         envParameterDefinitions:\n  - {name: Product, type: STRING}\n  \
         - {name: Seats, type: INT, minValue: 1, default: 1}\n  \
         script: {actions: {onEnter: {command: echo, args: [ENTER, Lic]}}}\n\
         steps:\n- name: S\n  dependencies: [{environment: Lic, parameters: {Seats: 0}}]\n  \
         script: {actions: {onRun: {command: echo, args: [TASK]}}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["check", &job]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!("Template at '{job}' passes validation checks.\n")
    );
    let (code, stdout, stderr) = run_cli(&["run", &job]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("TASK"), "{stdout}");
    assert!(!stdout.contains("ENTER Lic"), "{stdout}");
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--step", "S"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains(
            "Session environments:\n  (none)\nDisabled environments:\n  - Lic (JobTemplate)\n"
        ),
        "{stdout}"
    );
    // Enabled, both checks apply.
    let (code, _stdout, stderr) = run_cli(&["run", &job, "-p", "Use=yes"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("Model validation error: 2 validation errors for JobTemplate\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("steps[0] -> dependencies[0] -> parameters -> Seats:\n\tParameter 'Seats': value 0 is less than minimum 1"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without \
             a default of each Environment its Sessions enter (every jobEnvironments entry with \
             activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; \
             Template Schemas §3.2 constraint 7); missing: 'Product' of Environment 'Lic'.\n"
        ),
        "{stderr}"
    );
}

#[test]
fn range_expr_values_that_denote_the_same_integers_keep_the_environment_entered() {
    // §3.2.2: two RANGE_EXPR values are equal when they have the same
    // canonical form, so A and B (`1-3`), C (`1,2,3`), and E (` 1-3 `) run
    // with Cache entered once, and D, which takes the default `1-10`, only
    // after the Session exits Cache and enters it again. Each value is its
    // canonical form in the Session, in `run`, and in `summary`.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: Cache\n  envParameterDefinitions: [{name: F, type: RANGE_EXPR, \
         default: '1-10'}]\n  script: {actions: {onEnter: {command: echo, args: [ENTER, \
         \"{{ len(Env.Param.F) }}\", \"{{ Env.Param.F }}\"]}}}\n\
         steps:\n\
         - name: A\n  dependencies: [{environment: Cache, parameters: {F: '1-3'}}]\n  \
         script: {actions: {onRun: {command: echo, args: [TASK, A]}}}\n\
         - name: B\n  dependencies: [{environment: Cache, parameters: {F: '1-3'}}]\n  \
         script: {actions: {onRun: {command: echo, args: [TASK, B]}}}\n\
         - name: C\n  dependencies: [{environment: Cache, parameters: {F: '1,2,3'}}]\n  \
         script: {actions: {onRun: {command: echo, args: [TASK, C]}}}\n\
         - name: E\n  dependencies: [{environment: Cache, parameters: {F: ' 1-3 '}}]\n  \
         script: {actions: {onRun: {command: echo, args: [TASK, E]}}}\n\
         - name: D\n  script: {actions: {onRun: {command: echo, args: [TASK, D]}}}\n",
    );
    let (code, stdout, stderr) = run_cli(&["run", &job]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(count(&stdout, "Entering Environment: Cache"), 2, "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "F(RANGE_EXPR) = 1-3",
            "ENTER 3 1-3",
            "TASK A",
            "TASK B",
            "TASK C",
            "TASK E",
            "Exiting Environment: Cache",
            "F(RANGE_EXPR) = 1-10",
            "ENTER 10 1-10",
            "TASK D",
        ],
    );
    let (code, stdout, stderr) = run_cli(&["summary", &job]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "F (RANGE_EXPR): 1-3\n"), 4, "{stdout}");
    let (code, stdout, stderr) = run_cli(&["summary", &job, "--output", "json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let value = |step: usize| json["steps"][step]["environment_parameters"][0]["values"][0].clone();
    for step in 0..4 {
        assert_eq!(
            value(step),
            serde_json::json!({"name": "F", "type": "RANGE_EXPR", "value": "1-3"}),
            "step {step}"
        );
    }
}

#[test]
fn only_the_environments_above_the_first_difference_are_exited() {
    // Job Environments First, Middle, and Last, entered in that order. A
    // requests the ON_REQUEST Middle and B does not, so between them the
    // Session exits Last and Middle (the reverse of entry order) and enters
    // Last again; First, the longest entered prefix B also needs, stays
    // entered. Between B and C only Last's value differs, so only Last is
    // exited and entered again (Template Schemas §3.2.2).
    let dir = tempfile::tempdir().unwrap();
    let echo = |what: &str| format!("{{command: python, args: [\"-c\", \"print('{what}')\"]}}");
    let job = write_file(
        &dir,
        "job.yaml",
        &format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n\
             - name: First\n  script: {{actions: {{onEnter: {fe}, onExit: {fx}}}}}\n\
             - name: Middle\n  activation: ON_REQUEST\n  \
             script: {{actions: {{onEnter: {me}, onExit: {mx}}}}}\n\
             - name: Last\n  envParameterDefinitions: [{{name: V, type: INT, default: 1}}]\n  \
             script: {{actions: {{onEnter: {{command: python, args: [\"-c\", \"import sys; \
             print('ENTER Last ' + sys.argv[1])\", \"{{{{Env.Param.V}}}}\"]}}, \
             onExit: {{command: python, args: [\"-c\", \"import sys; print('EXIT Last ' + \
             sys.argv[1])\", \"{{{{Env.Param.V}}}}\"]}}}}}}\n\
             steps:\n\
             - name: A\n  dependencies: [{{environment: Middle}}]\n  \
             script: {{actions: {{onRun: {ta}}}}}\n\
             - name: B\n  dependencies: [{{dependsOn: A}}]\n  script: {{actions: {{onRun: {tb}}}}}\n\
             - name: C\n  dependencies: [{{dependsOn: B}}, {{environment: Last, parameters: \
             {{V: 2}}}}]\n  script: {{actions: {{onRun: {tc}}}}}\n",
            fe = echo("ENTER First"),
            fx = echo("EXIT First"),
            me = echo("ENTER Middle"),
            mx = echo("EXIT Middle"),
            ta = echo("TASK A"),
            tb = echo("TASK B"),
            tc = echo("TASK C"),
        ),
    );
    let (code, stdout, stderr) = run_cli(&["run", &job]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    let markers: Vec<&str> = stdout
        .lines()
        .filter_map(|line| {
            ["ENTER ", "EXIT ", "TASK "]
                .iter()
                .find_map(|m| line.find(m).map(|at| &line[at..]))
        })
        .collect();
    assert_eq!(
        markers,
        [
            "ENTER First",
            "ENTER Middle",
            "ENTER Last 1",
            "TASK A",
            "EXIT Last 1",
            "EXIT Middle",
            "ENTER Last 1",
            "TASK B",
            "EXIT Last 1",
            "ENTER Last 2",
            "TASK C",
            "EXIT Last 2",
            "EXIT First",
        ],
        "{stdout}"
    );
}

#[test]
fn an_environment_reentered_under_a_wrapper_exits_through_the_wrap_hooks() {
    // RFC 0008: Wrap wraps the actions of every Environment entered after
    // it. The Session keeps Wrap entered and exits and enters Seats again
    // between B and C, each time through Wrap's onWrapEnvExit and
    // onWrapEnvEnter. The hooks run the wrapped action, so Seats' own
    // onEnter and onExit show the values each entry used.
    let dir = tempfile::tempdir().unwrap();
    let job = write_file(
        &dir,
        "job.yaml",
        r#"specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: WrapReenter
jobEnvironments:
- name: Wrap
  script:
    actions:
      onEnter: {command: python, args: ["-c", "print('Wrap own onEnter')"]}
      onWrapEnvEnter:
        command: python
        args: ["-c", "import subprocess, sys; print('WrapEnvEnter ' + sys.argv[1], flush=True); sys.exit(subprocess.run(sys.argv[2:]).returncode)", "{{WrappedEnv.Name}}", "{{WrappedAction.Command}}", "{{WrappedAction.Args}}"]
      onWrapTaskRun: {command: python, args: ["-c", "print('WrapTaskRun')"]}
      onWrapEnvExit:
        command: python
        args: ["-c", "import subprocess, sys; print('WrapEnvExit ' + sys.argv[1], flush=True); sys.exit(subprocess.run(sys.argv[2:]).returncode)", "{{WrappedEnv.Name}}", "{{WrappedAction.Command}}", "{{WrappedAction.Args}}"]
      onExit: {command: python, args: ["-c", "print('Wrap own onExit')"]}
- name: Seats
  envParameterDefinitions: [{name: Count, type: INT, default: 1}]
  script:
    actions:
      onEnter: {command: python, args: ["-c", "import sys; print('Seats onEnter Count=' + sys.argv[1])", "{{Env.Param.Count}}"]}
      onExit: {command: python, args: ["-c", "import sys; print('Seats onExit Count=' + sys.argv[1])", "{{Env.Param.Count}}"]}
steps:
- name: B
  dependencies: [{environment: Seats, parameters: {Count: 2}}]
  script: {actions: {onRun: {command: python, args: ["-c", "print('Step B ran')"]}}}
- name: C
  dependencies: [{dependsOn: B}]
  script: {actions: {onRun: {command: python, args: ["-c", "print('Step C ran')"]}}}
"#,
    );
    let (code, stdout, stderr) = run_cli(&["run", &job]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(count(&stdout, "Wrap own onEnter"), 1, "{stdout}");
    assert_eq!(count(&stdout, "WrapEnvEnter Seats"), 2, "{stdout}");
    assert_eq!(count(&stdout, "WrapEnvExit Seats"), 2, "{stdout}");
    assert_in_order(
        &stdout,
        &[
            "Wrap own onEnter",
            "Entering Environment: Seats",
            "\tCount(INT) = 2",
            "WrapEnvEnter Seats",
            "Seats onEnter Count=2",
            "Running step 'B'",
            "WrapTaskRun",
            "Exiting Environment: Seats",
            "WrapEnvExit Seats",
            "Seats onExit Count=2",
            "Entering Environment: Seats",
            "\tCount(INT) = 1",
            "WrapEnvEnter Seats",
            "Seats onEnter Count=1",
            "Running step 'C'",
            "WrapTaskRun",
            "Exiting Environment: Seats",
            "WrapEnvExit Seats",
            "Seats onExit Count=1",
            "Exiting Environment: Wrap",
            "Wrap own onExit",
            "All actions completed successfully!",
        ],
    );
}

/// The `ENTER ...`, `EXIT ...`, `TASK ...`, and `Running step ...` lines of
/// a run's output, from the marker on, in order.
fn lifecycle_markers(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter_map(|line| {
            ["ENTER ", "EXIT ", "TASK ", "Running step "]
                .iter()
                .find_map(|m| line.find(m).map(|at| &line[at..]))
        })
        .collect()
}

/// A Job Template with Job Environments `First`, `Seats` (INT `Count`,
/// default 1), and `Third`, each of whose actions print a marker, and Steps
/// A (`Count: 2`) and B (the default). `seats_on_enter` is the Python body of
/// Seats' `onEnter`, with `sys.argv[1]` its Count.
fn three_environment_job(dir: &tempfile::TempDir, seats_on_enter: &str) -> String {
    let echo = |what: &str| format!("{{command: python, args: [\"-c\", \"print('{what}')\"]}}");
    write_file(
        dir,
        "job.yaml",
        &format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n\
             - name: First\n  script: {{actions: {{onEnter: {fe}, onExit: {fx}}}}}\n\
             - name: Seats\n  envParameterDefinitions: [{{name: Count, type: INT, default: 1}}]\n  \
             script: {{actions: {{onEnter: {{command: python, args: [\"-c\", \"import sys; \
             {seats_on_enter}\", \"{{{{Env.Param.Count}}}}\"]}}, \
             onExit: {{command: python, args: [\"-c\", \"import sys; print('EXIT Seats ' + \
             sys.argv[1])\", \"{{{{Env.Param.Count}}}}\"]}}}}}}\n\
             - name: Third\n  script: {{actions: {{onEnter: {te}, onExit: {tx}}}}}\n\
             steps:\n\
             - name: A\n  dependencies: [{{environment: Seats, parameters: {{Count: 2}}}}]\n  \
             script: {{actions: {{onRun: {ta}}}}}\n\
             - name: B\n  dependencies: [{{dependsOn: A}}]\n  script: {{actions: {{onRun: {tb}}}}}\n",
            fe = echo("ENTER First"),
            fx = echo("EXIT First"),
            te = echo("ENTER Third"),
            tx = echo("EXIT Third"),
            ta = echo("TASK A"),
            tb = echo("TASK B"),
        ),
    )
}

#[test]
fn an_unchanged_environment_above_a_changed_one_is_exited_and_entered_again() {
    // Seats' value differs between A and B, and Third, entered after it, is
    // unchanged. Exits follow the reverse of entry order (Template Schemas
    // §3.2.2), so the Session exits Third to reach Seats, and enters both
    // again in Job order; First, below the difference, stays entered.
    let dir = tempfile::tempdir().unwrap();
    let job = three_environment_job(&dir, "print('ENTER Seats ' + sys.argv[1])");
    let (code, stdout, stderr) = run_cli(&["run", &job]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(
        lifecycle_markers(&stdout),
        [
            "ENTER First",
            "ENTER Seats 2",
            "ENTER Third",
            "Running step 'A'",
            "TASK A",
            "EXIT Third",
            "EXIT Seats 2",
            "ENTER Seats 1",
            "ENTER Third",
            "Running step 'B'",
            "TASK B",
            "EXIT Third",
            "EXIT Seats 1",
            "EXIT First",
        ],
        "{stdout}"
    );
}

#[test]
fn a_failed_reentry_stops_the_run_and_exits_every_environment() {
    // Seats' onEnter fails for B's value. The Session does not enter Third
    // or run B, exits Seats (whose enter was attempted) and First, and the
    // run fails.
    let dir = tempfile::tempdir().unwrap();
    let job = three_environment_job(
        &dir,
        "print('ENTER Seats ' + sys.argv[1]); sys.exit(0 if sys.argv[1] == '2' else 5)",
    );
    let (code, stdout, stderr) = run_cli(&["run", &job]);
    assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(count(&stdout, "\tSession start"), 1, "{stdout}");
    assert_eq!(
        lifecycle_markers(&stdout),
        [
            "ENTER First",
            "ENTER Seats 2",
            "ENTER Third",
            "Running step 'A'",
            "TASK A",
            "EXIT Third",
            "EXIT Seats 2",
            "ENTER Seats 1",
            "EXIT Seats 1",
            "EXIT First",
        ],
        "{stdout}"
    );
    assert_eq!(count(&stdout, "Entering Environment: Third"), 1, "{stdout}");
    assert!(stdout.contains("Process exited with code: 5"), "{stdout}");
    assert!(
        stderr.contains("ERROR: Environment setup failed: "),
        "{stderr}"
    );
    assert!(stdout.contains("Session ended with errors."), "{stdout}");
}
