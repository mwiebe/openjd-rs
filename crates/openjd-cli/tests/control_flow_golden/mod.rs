// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Golden-file tests of `openjd` on the RFC 0010 (CONTROL_FLOW) sample
//! templates.
//!
//! Each case runs `openjd` on a template in `tests/templates/control_flow/`,
//! normalizes the output, and compares it with `<case>.expected.txt` beside
//! the template. `OPENJD_UPDATE_GOLDEN=1 cargo test -p openjd-cli` rewrites
//! the expected files instead. See `tests/templates/control_flow/README.md`
//! for the samples, their stubs, and how a case is set up.
//!
//! The samples' `bash` scripts rely on GNU coreutils (`stat -c`, unpadded
//! `wc -l`), so these tests run on Linux only.

use super::{cli_command, templates_dir};
use std::path::{Path, PathBuf};

/// One `openjd` invocation and its expected output.
struct Case<'a> {
    /// The expected file is `control_flow/<name>.expected.txt`.
    name: &'a str,
    /// The `openjd` arguments. `{T}` stands for the `control_flow` template
    /// directory and `{W}` for the case's scratch directory, which is also
    /// the working directory and `TMPDIR`.
    args: &'a [&'a str],
    /// Extra environment variables for the stubs, written to the expected
    /// file's header.
    env: &'a [(&'a str, &'a str)],
    /// A `control_flow/fixtures/` directory copied to `{W}/s3`, which the
    /// boto3 stub serves as its buckets.
    s3: Option<&'a str>,
}

fn control_flow_dir() -> PathBuf {
    templates_dir().join("control_flow")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The output with each log line's relative timestamp removed, the template
/// and scratch directories replaced by `{T}` and `{W}`, and the session
/// duration replaced by `{DURATION}`.
fn normalize(text: &str, dir: &str, scratch: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        // A log line is `H:MM:SS.mmm<TAB>message`.
        let line = match line.split_once('\t') {
            Some((stamp, rest))
                if !stamp.is_empty()
                    && stamp
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == ':' || c == '.') =>
            {
                rest
            }
            _ => line,
        };
        let line = line.replace(scratch, "{W}").replace(dir, "{T}");
        let line = match line.strip_prefix("Duration: ") {
            Some(rest) if rest.ends_with(" seconds") => "Duration: {DURATION}".to_string(),
            _ => line,
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn check(case: Case) {
    let dir = control_flow_dir();
    let scratch = tempfile::tempdir().unwrap();
    let scratch_path = scratch.path().canonicalize().unwrap();
    let (dir_s, scratch_s) = (dir.to_str().unwrap(), scratch_path.to_str().unwrap());
    let args: Vec<String> = case
        .args
        .iter()
        .map(|a| a.replace("{T}", dir_s).replace("{W}", scratch_s))
        .collect();
    let mut cmd = cli_command(&args.iter().map(String::as_str).collect::<Vec<_>>());
    // Put the command stubs first on the PATH that cli_command chose, which
    // may already start with its python shim.
    let base_path = cmd
        .get_envs()
        .find(|(k, _)| *k == "PATH")
        .and_then(|(_, v)| v.map(|v| v.to_os_string()))
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(dir.join("stubs/bin")).chain(std::env::split_paths(&base_path)),
    )
    .unwrap();
    cmd.current_dir(&scratch_path)
        .env("PATH", path)
        .env("PYTHONPATH", dir.join("stubs/python"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        // Unbuffered, so a script's own lines and those of the stub
        // commands it runs appear in the order they were printed.
        .env("PYTHONUNBUFFERED", "1")
        .env("TMPDIR", &scratch_path);
    for (k, v) in case.env {
        cmd.env(k, v);
    }
    if let Some(fixture) = case.s3 {
        let s3 = scratch_path.join("s3");
        copy_dir(&dir.join("fixtures").join(fixture), &s3);
        cmd.env("OPENJD_GOLDEN_S3", &s3);
    }
    let output = cmd.output().expect("failed to execute openjd");

    // Arguments with spaces are quoted, so the command line reads as one.
    let shown: Vec<String> = case
        .args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("'{a}'")
            } else {
                a.to_string()
            }
        })
        .collect();
    let mut actual = format!("$ openjd {}\n", shown.join(" "));
    for (k, v) in case.env {
        actual.push_str(&format!("$ env {k}={v}\n"));
    }
    if let Some(fixture) = case.s3 {
        actual.push_str(&format!("$ s3 fixtures/{fixture}\n"));
    }
    actual.push_str(&format!(
        "exit code: {}\n",
        output.status.code().unwrap_or(-1)
    ));
    for (label, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        actual.push_str(&format!("--- {label}\n"));
        actual.push_str(&normalize(
            &String::from_utf8_lossy(bytes),
            dir_s,
            scratch_s,
        ));
    }

    let expected_path = dir.join(format!("{}.expected.txt", case.name));
    if std::env::var_os("OPENJD_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        std::fs::write(&expected_path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run with OPENJD_UPDATE_GOLDEN=1 to create it",
            expected_path.display()
        )
    });
    if actual != expected {
        let (a, e): (Vec<_>, Vec<_>) = (actual.lines().collect(), expected.lines().collect());
        let line = a
            .iter()
            .zip(&e)
            .position(|(a, e)| a != e)
            .unwrap_or(a.len().min(e.len()));
        panic!(
            "{} differs from the actual output at line {}:\n  expected: {:?}\n  actual:   {:?}\n\
             Run with OPENJD_UPDATE_GOLDEN=1 to update it.\n--- actual output\n{actual}",
            expected_path.display(),
            line + 1,
            e.get(line).copied().unwrap_or("<end of file>"),
            a.get(line).copied().unwrap_or("<end of output>"),
        );
    }
}

/// Expands to one `#[test]` per case, and `CASES`, the expected file names.
macro_rules! golden {
    ($($test:ident: $case:expr;)*) => {
        $(
            #[test]
            fn $test() {
                check($case);
            }
        )*
        const CASES: &[&str] = &[$($case.name),*];
    };
}

/// Every expected file belongs to a case, so a removed or renamed case
/// doesn't leave a stale file behind.
#[test]
fn every_expected_file_has_a_case() {
    let mut stale = Vec::new();
    for entry in std::fs::read_dir(control_flow_dir()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if let Some(case) = name.strip_suffix(".expected.txt") {
            if !CASES.contains(&case) {
                stale.push(name);
            }
        }
    }
    assert!(stale.is_empty(), "expected files with no case: {stale:?}");
}

const NO_ENV: &[(&str, &str)] = &[];

golden! {
    // Video frame count: VideoToFrames reports FrameCount=3, which gives
    // StylizeFrame 3 Tasks, and a scalar FrameRate that FramesToVideo reads
    // in its script, a host-scope field.
    video_frame_count: Case {
        name: "video_frame_count",
        args: &["run", "{T}/video_frame_count.yaml",
            "-p", "InputVideo={T}/fixtures/three_frames.video",
            "-p", "WorkDir={W}/work", "-p", "OutputVideo={W}/out.mp4"],
        env: NO_ENV, s3: None,
    };
    // StylizeFrame run alone, with its producer's output given on the command line.
    video_frame_count_step_inputs: Case {
        name: "video_frame_count.step_inputs",
        args: &["run", "{T}/video_frame_count.yaml", "--step", "StylizeFrame",
            "--step-inputs",
            r#"{"Steps.VideoToFrames.Output.FrameCount": 2, "Steps.VideoToFrames.Output.FrameRate": "30/1"}"#,
            "-p", "InputVideo={T}/fixtures/three_frames.video",
            "-p", "WorkDir={W}/work", "-p", "OutputVideo={W}/out.mp4"],
        env: NO_ENV, s3: None,
    };
    // Chunked render: two chunks of three frames, each reporting RenderTimeS[frame].
    chunked_render: Case {
        name: "chunked_render",
        args: &["run", "{T}/chunked_render.yaml"],
        env: NO_ENV, s3: None,
    };
    // Frame 5 reports no RenderTimeS, so its chunk fails.
    chunked_render_omit_frame: Case {
        name: "chunked_render_omit_frame",
        args: &["run", "{T}/chunked_render_omit_frame.yaml"],
        env: NO_ENV, s3: None,
    };
    // Task metrics: frames_done reported twice (LAST), three chunk_error_mm
    // values (APPEND), and the three ONCE metrics, each shown with its unit.
    task_metrics: Case {
        name: "task_metrics",
        args: &["run", "{T}/task_metrics.yaml"],
        env: NO_ENV, s3: None,
    };
    // mpjpe_mm (ONCE) reported twice fails the Task.
    task_metrics_duplicate_once: Case {
        name: "task_metrics.duplicate_once",
        args: &["run", "{T}/task_metrics.yaml"],
        env: &[("OPENJD_GOLDEN_DUPLICATE_MPJPE", "1")], s3: None,
    };
    // S3 fan-out with no .mov keys: a zero-Task Transcode, and a manifest with totalBytes 0.
    s3_fan_out_no_keys: Case {
        name: "s3_fan_out.no_keys",
        args: &["run", "{T}/s3_fan_out.yaml",
            "-p", "Bucket=media", "-p", "Prefix=shots/", "-p", "OutputPrefix=transcoded"],
        env: NO_ENV, s3: Some("s3_no_keys"),
    };
    // S3 fan-out with two .mov keys.
    s3_fan_out_two_keys: Case {
        name: "s3_fan_out.two_keys",
        args: &["run", "{T}/s3_fan_out.yaml",
            "-p", "Bucket=media", "-p", "Prefix=shots/", "-p", "OutputPrefix=transcoded"],
        env: NO_ENV, s3: Some("s3_two_keys"),
    };
    // --maximum-tasks 1 runs one of Transcode's two Tasks, which blocks Manifest.
    s3_fan_out_maximum_tasks: Case {
        name: "s3_fan_out.maximum_tasks",
        args: &["run", "{T}/s3_fan_out.yaml", "--maximum-tasks", "1",
            "-p", "Bucket=media", "-p", "Prefix=shots/", "-p", "OutputPrefix=transcoded"],
        env: NO_ENV, s3: Some("s3_two_keys"),
    };
    // Sweep with a winner: the job outputs VideoCount, BestSingleTaskErrorMm
    // and ResultDirs appear after Discover, Sweep and RunWinner.
    sweep_winner: Case {
        name: "sweep.winner",
        args: &["run", "{T}/sweep.yaml", "-p", "EpisodeCount=2",
            "-p", "BenchmarkDir={T}/fixtures/benchmark", "-p", "DatasetBucket=datasets",
            "-p", "DatasetPrefix=hot3d/", "-p", "OutputDir={W}/results"],
        env: NO_ENV, s3: Some("s3_videos"),
    };
    // No configuration reaches MinFps, so RunWinner fails at binding with the fail(...) message.
    sweep_no_fast_configuration: Case {
        name: "sweep.no_fast_configuration",
        args: &["run", "{T}/sweep.yaml", "-p", "EpisodeCount=2", "-p", "MinFps=100",
            "-p", "BenchmarkDir={T}/fixtures/benchmark", "-p", "DatasetBucket=datasets",
            "-p", "DatasetPrefix=hot3d/", "-p", "OutputDir={W}/results"],
        env: NO_ENV, s3: Some("s3_videos"),
    };
    // With a path mapping rule, the Tasks write under {W}/mapped, and
    // ResultDirs names the directories in the submission form, /submit/....
    sweep_path_mapping: Case {
        name: "sweep.path_mapping",
        args: &["run", "{T}/sweep.yaml", "-p", "EpisodeCount=2",
            "-p", "BenchmarkDir={T}/fixtures/benchmark", "-p", "DatasetBucket=datasets",
            "-p", "DatasetPrefix=hot3d/", "-p", "OutputDir=/submit/results",
            "--path-mapping-rules",
            r#"{"version": "pathmapping-1.0", "path_mapping_rules": [{"source_path_format": "POSIX", "source_path": "/submit", "destination_path": "{W}/mapped"}]}"#],
        env: NO_ENV, s3: Some("s3_videos"),
    };
    // The scene-graph sample needs TASK_DEPENDENCIES, which is not
    // supported. Template parsing rejects the unknown dependsOnSubspace field
    // before it checks extension names, so the error names the field.
    scene_graph_check: Case {
        name: "scene_graph.check",
        args: &["check", "{T}/scene_graph.yaml"],
        env: NO_ENV, s3: None,
    };
    // Without dependsOnSubspace, BakeNode's Tasks come from the computed node count.
    scene_graph_no_subspace: Case {
        name: "scene_graph_no_subspace",
        args: &["run", "{T}/scene_graph_no_subspace.yaml",
            "-p", "Scene={T}/fixtures/scene_graph.json", "-p", "BakeDir={W}/bakes"],
        env: NO_ENV, s3: None,
    };
    // A clean render: BadFrames=[] gives a zero-Task Rerender, and noise
    // under the threshold skips Denoise.
    render_qc_clean: Case {
        name: "render_qc.clean",
        args: &["run", "{T}/render_qc.yaml", "-p", "Frames=1-3",
            "-p", "SceneFile={T}/fixtures/clean.scene",
            "-p", "OutputPattern={W}/out/beauty_####.exr", "-p", "OutputVideo={W}/out/beauty.mp4"],
        env: NO_ENV, s3: None,
    };
    // A noisy frame: Rerender runs one Task, and noise over the threshold enables Denoise.
    render_qc_noisy: Case {
        name: "render_qc.noisy",
        args: &["run", "{T}/render_qc.yaml", "-p", "Frames=1-3",
            "-p", "SceneFile={T}/fixtures/noisy.scene",
            "-p", "OutputPattern={W}/out/beauty_####.exr", "-p", "OutputVideo={W}/out/beauty.mp4"],
        env: NO_ENV, s3: None,
    };
    // EncodeVideo=false skips the EncodeVideo Step at Job creation, so Publish prints "frames only".
    render_qc_no_encode: Case {
        name: "render_qc.no_encode",
        args: &["run", "{T}/render_qc.yaml", "-p", "Frames=1-3", "-p", "EncodeVideo=false",
            "-p", "SceneFile={T}/fixtures/clean.scene",
            "-p", "OutputPattern={W}/out/beauty_####.exr", "-p", "OutputVideo={W}/out/beauty.mp4"],
        env: NO_ENV, s3: None,
    };
    // C's Tasks are known after P, and C is ready after Q.
    progressive: Case {
        name: "progressive",
        args: &["run", "{T}/progressive.yaml"],
        env: NO_ENV, s3: None,
    };
    // summary shows C's Tasks determined at run time from P, and its host requirements waiting for Q.
    progressive_summary: Case {
        name: "progressive.summary",
        args: &["summary", "{T}/progressive.yaml"],
        env: NO_ENV, s3: None,
    };
    // C's range fails as soon as P succeeds, and Q never runs.
    progressive_range_fails: Case {
        name: "progressive.range_fails",
        args: &["run", "{T}/progressive.yaml", "-p", "N=0"],
        env: NO_ENV, s3: None,
    };
    // C's enableIf reads Q and is false, so its failing range is never evaluated.
    progressive_enable_if: Case {
        name: "progressive_enable_if",
        args: &["run", "{T}/progressive_enable_if.yaml", "-p", "N=0"],
        env: NO_ENV, s3: None,
    };
    // summary shows a known Task count for a Step pending on its step let.
    progressive_let_summary: Case {
        name: "progressive_let.summary",
        args: &["summary", "{T}/progressive_let.yaml"],
        env: NO_ENV, s3: None,
    };
    progressive_let: Case {
        name: "progressive_let",
        args: &["run", "{T}/progressive_let.yaml"],
        env: NO_ENV, s3: None,
    };
}
