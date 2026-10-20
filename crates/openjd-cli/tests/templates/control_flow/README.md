# CONTROL_FLOW golden tests

Golden-file tests of `openjd` on the sample templates of RFC 0011, RFC 0012, and RFC 0013
(CONTROL_FLOW) and on the plan's progressive binding templates, run by
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
| `video_frame_count.yaml` | RFC 0011 Basic Examples, "Video frame count" | none |
| `s3_fan_out.yaml` | RFC 0011 Basic Examples, "S3 prefix fan-out with a manifest" | none |
| `task_metrics.yaml` | RFC 0012 Basic Examples, "Task metrics from a parameter sweep" | parameter space cut from 10×3×3×3 to 2×1×1×1 |
| `sweep.yaml` | RFC 0011 Basic Examples, "Parameter sweep, then run the winner" | `Window`, `Overlap`, `DetConf` ranges cut to 2×1×1; run with `EpisodeCount=2` |
| `scene_graph.yaml` | RFC 0011 Basic Examples, "Scene-graph DAG" | none |
| `scene_graph_no_subspace.yaml` | "Scene-graph DAG" | no `TASK_DEPENDENCIES`: the `BakeNode` self-dependency with `dependsOnSubspace` is removed |
| `scene_graph_extension.yaml` | "Scene-graph DAG" | the `dependsOnSubspace` self-dependency is removed, but `TASK_DEPENDENCIES` is still listed |
| `render_qc.yaml` | RFC 0011 Basic Examples, "Render, quality check, and optional steps" | none |
| `chunked_render.yaml` | RFC 0012 Specification, "Chunked Steps" | wrapped in a Job Template; range `1-6` and `defaultTaskCount: 3` |
| `chunked_render_omit_frame.yaml` | as `chunked_render.yaml` | frame 5 reports no `RenderTimeS` |
| `progressive.yaml`, `progressive_enable_if.yaml`, `progressive_let.yaml` | not RFC samples: the plan's progressive binding tests | — |
| `refine.yaml` | RFC 0013 Basic Examples, "Render, check, and re-render until clean" | none; run with `Frames=1-4`, also with `MaxPasses=2` or `--maximum-tasks 2`, and alone with `--step` |
| `solver.yaml` | RFC 0013 Basic Examples, "Iterative solver" | none; run with `Tolerance=0.01` or `0.1`, or with `StartDamping=0.0015` so that `Damping`'s `next` breaks its `minValue` |
| `s3_pages.yaml` | RFC 0013 Basic Examples, "Paginated listing, then fan out" | none; listed in pages of two keys with `OPENJD_GOLDEN_S3_PAGE_SIZE=2`, or over an empty prefix |
| `s3_pages_zero.yaml` | "Paginated listing, then fan out" | `Loop.Iteration == 0 or` removed from `while`, so the loop has zero iterations |
| `solver_optional.yaml` | "Iterative solver" | an `enableIf` on `Relax` that skips it when `Prepare`'s residual is under the tolerance; the job outputs handle `null` loop outputs; run with `Tolerance=2` to skip it |

The RFC 0011 and RFC 0012 YAML is from openjd-specifications commit `c3f583b`; it has
not changed since `619d910`. The RFC 0013 YAML is from `c3f583b`. Each template's first
comment names its source.

## Stubs and fixtures

The samples' application commands are deterministic Python stubs, so the samples run as
written. The harness puts `stubs/bin` first on `PATH` and `stubs/python` on `PYTHONPATH`:

* `stubs/bin`: `ffmpeg`, `ffprobe`, `stylize`, `render`, `denoise`, `publish`, `bake-node`,
  `assemble-lightmaps`, and a `date` that answers `date +%s.%N` with a time 1.25 seconds after
  its previous call, so the chunked render's per-frame times are fixed.
* `stubs/python`: `boto3` (S3 backed by a directory), `pipeline`, `handtrack`, `scenetools`,
  `qc`, and `solver`.

`OPENJD_GOLDEN_SOLVER_FAIL_STEP=N` makes the solver's step N exit with an error, so a case
can fail a body Step in a later iteration. `qc` exits with a one-line error when a frame it
checks was not rendered, as in `refine.maximum_tasks`.

Each stub's docstring says what it does. The stubs whose output the samples don't capture
print a `<name> stub:` line. `ffprobe`, `date`, `scenetools`, and `pipeline` (except its
`process_key`) print nothing of their own, and neither do `boto3`'s `list_objects_v2` and `head_object`; its
`download_file`, `upload_file` and `put_object` print a marker. `stylize` writes its output
even when its input frame is missing, and says `(no input frame)`, because `StylizeFrame`
run alone with `--step` has no extracted frames.

Inputs come from `fixtures/`: a text "video" (`three_frames.video`), scene files for the
render and scene-graph samples (`refine.scene` has frames that need 128 and 256 samples to
be clean, as `render --samples N` divides the noise by N / 64), a mesh for the solver
(`mesh.txt`), and S3 buckets. A case that uses S3 copies its
`fixtures/<dir>` to the scratch directory, where the `boto3` stub reads and writes it.

## Cases and normalization

Each case runs in a fresh scratch directory, which is also its working directory and
`TMPDIR`. In arguments, `{T}` stands for this directory and `{W}` for the scratch
directory. The expected file starts with the command line, any stub environment variables
and the S3 fixture, then the exit code, stdout and stderr. In the output, the log
timestamps are removed, both directories are written `{T}` and `{W}`, and the session
duration, also in `--output json`, is `{DURATION}`.

The harness removes `BASH_ENV`, `ENV`, `CDPATH`, `SHELLOPTS`, `BASHOPTS`, every `PYTHON*`
variable and the stubs' own `OPENJD_GOLDEN_*` variables from the environment it inherits,
because they change what the samples print, and then sets the `PYTHON*` variables it needs.

The flag rules for a loop that is the Job's only top-level entry (`--tasks` and
`--task-param` without `--step`) need a template with no other Step, which none of the RFC 0013
samples is, so `cli_tests`' `control_flow_loops` module tests them instead of a golden case.

To add a case, add an entry to the `golden!` list in `tests/control_flow_golden/mod.rs` and
run with `OPENJD_UPDATE_GOLDEN=1`. A test fails if an expected file here has no case.
