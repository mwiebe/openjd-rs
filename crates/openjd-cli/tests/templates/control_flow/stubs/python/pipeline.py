# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Deterministic hand-tracking pipeline stub for the task metrics sample.

The scores are a fixed function of the Task parameters.

If `OPENJD_GOLDEN_DUPLICATE_MPJPE` is set, `extract_hand_actions` prints its
own `mpjpe_mm` directive, as a library that logs its results would, so the
sample's `ONCE` output is reported twice. The score's `coverage` then waits
for 60 seconds before it returns. The sample reads it just after printing the
duplicate, so the runtime must cancel the Action while it waits: the output
ends at the duplicate, and a runtime that doesn't cancel at once is slow and
prints the later directives.
"""
import os
import time


class _Trajectory:
    def __init__(self, video, frames):
        self.video = video
        self.frames = frames


class _Score:
    def __init__(self, video, frames):
        n = int(video[1:5])
        base = 40.0 + n
        self.chunk_errors_mm = [base + 2.5, base - 1.25, base + 0.5]
        self.mpjpe_mm = sum(self.chunk_errors_mm) / len(self.chunk_errors_mm)
        self.fps = float(frames) / 80
        self._coverage = 0.75 + 0.05 * n

    @property
    def coverage(self):
        if os.environ.get("OPENJD_GOLDEN_DUPLICATE_MPJPE"):
            time.sleep(60)
        return self._coverage


def extract_hand_actions(video, window, overlap, det_conf, on_progress):
    frames = 400 * (1 + int(video[1:5]))
    on_progress(frames)
    if os.environ.get("OPENJD_GOLDEN_DUPLICATE_MPJPE"):
        print("openjd_output_var: mpjpe_mm=1.0", flush=True)
    return _Trajectory(video, frames)


def score_against_hot3d(trajectory):
    return _Score(trajectory.video, trajectory.frames)
