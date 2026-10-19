# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Deterministic hand-tracking stub for the sweep sample.

`benchmark` scores are a fixed function of the configuration: a larger
window is more accurate and slower. `track_s3_video` creates one result
directory per video under `out_dir` and returns it.
"""
import os
from types import SimpleNamespace


def benchmark(benchmark_dir, episode, window, overlap, det_conf):
    print(f"handtrack stub: benchmark episode {episode} window {window}")
    return SimpleNamespace(mpjpe_mm=80.0 - window * 2 + episode * 0.5, fps=40.0 - window)


def track_s3_video(bucket, video, window, overlap, det_conf, out_dir):
    result_dir = os.path.join(out_dir, os.path.splitext(os.path.basename(video))[0])
    os.makedirs(result_dir, exist_ok=True)
    print(f"handtrack stub: tracked s3://{bucket}/{video} with window {window}, overlap {overlap}, det_conf {det_conf}")
    return result_dir
