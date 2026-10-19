# CONTROL_FLOW golden tests

Golden-file tests of `openjd` on the sample templates of RFC 0010 (CONTROL_FLOW), run by
`tests/control_flow_golden/mod.rs` as part of `cli_tests`. Each case runs one `openjd`
command and compares its normalized output with `<case>.expected.txt` in this directory.

```bash
cargo test -p openjd-cli --test cli_tests control_flow_golden            # compare
OPENJD_UPDATE_GOLDEN=1 cargo test -p openjd-cli --test cli_tests control_flow_golden   # rewrite
```

After rewriting, review the diff of the expected files: it is the change in behavior.
The tests run on Linux only, because the samples' `bash` scripts rely on GNU coreutils
(`stat -c %s`, and `wc -l` printing no padding).

## Samples

| Template | Source | Changes from the source |
|---|---|---|
| `video_frame_count.yaml` | RFC Basic Examples, "Video frame count" | none |
| `s3_fan_out.yaml` | "S3 prefix fan-out with a manifest" | none |
| `task_metrics.yaml` | "Task metrics from a parameter sweep" | parameter space cut from 10×3×3×3 to 2×1×1×1 |
| `sweep.yaml` | "Parameter sweep, then run the winner" | `Window`, `Overlap`, `DetConf` ranges cut to 2×1×1; run with `EpisodeCount=2` |
| `scene_graph.yaml` | "Scene-graph DAG" | none |
| `scene_graph_no_subspace.yaml` | "Scene-graph DAG" | no `TASK_DEPENDENCIES`: the `BakeNode` self-dependency with `dependsOnSubspace` is removed |
| `scene_graph_extension.yaml` | "Scene-graph DAG" | the `dependsOnSubspace` self-dependency is removed, but `TASK_DEPENDENCIES` is still listed |
| `render_qc.yaml` | "Render, quality check, and optional steps" | none |
| `chunked_render.yaml` | Specification, "Chunked Steps" | wrapped in a Job Template; range `1-6` and `defaultTaskCount: 3` |
| `chunked_render_omit_frame.yaml` | as `chunked_render.yaml` | frame 5 reports no `RenderTimeS` |
| `progressive.yaml`, `progressive_enable_if.yaml`, `progressive_let.yaml` | not RFC samples: the plan's progressive binding tests | — |

The RFC YAML is from openjd-specifications commit `619d910`. Each template's first comment
names its source.

## Stubs and fixtures

The samples' application commands are deterministic Python stubs, so the samples run as
written. The harness puts `stubs/bin` first on `PATH` and `stubs/python` on `PYTHONPATH`:

* `stubs/bin`: `ffmpeg`, `ffprobe`, `stylize`, `render`, `denoise`, `publish`, `bake-node`,
  `assemble-lightmaps`, and a `date` that answers `date +%s.%N` with a time 1.25 seconds after
  its previous call, so the chunked render's per-frame times are fixed.
* `stubs/python`: `boto3` (S3 backed by a directory), `pipeline`, `handtrack`, `scenetools`,
  and `qc`.

Each stub's docstring says what it does. The stubs whose output the samples don't capture
print a `<name> stub:` line; `ffprobe`, `date`, `pipeline`, `scenetools` and the read calls
of `boto3` print nothing of their own. `stylize` writes its output even when its input
frame is missing, and says `(no input frame)`, so `StylizeFrame` can run alone with
`--step-inputs`, which extracts no frames. Inputs
come from `fixtures/`: a text "video" (`three_frames.video`), scene files for the render
and scene-graph samples, and S3 buckets. A case that uses S3 copies its `fixtures/<dir>` to
the scratch directory, where the `boto3` stub reads and writes it.

## Cases and normalization

Each case runs in a fresh scratch directory, which is also its working directory and
`TMPDIR`. In arguments, `{T}` stands for this directory and `{W}` for the scratch
directory. The expected file starts with the command line, any stub environment variables
and the S3 fixture, then the exit code, stdout and stderr. In the output, the log
timestamps are removed, both directories are written `{T}` and `{W}`, and the session
duration is `{DURATION}`.

The harness removes `BASH_ENV`, `ENV`, `CDPATH` and the stubs' own `OPENJD_GOLDEN_*`
variables from the environment it inherits, because they change what the samples print.

To add a case, add an entry to the `golden!` list in `tests/control_flow_golden/mod.rs` and
run with `OPENJD_UPDATE_GOLDEN=1`. A test fails if an expected file here has no case.
