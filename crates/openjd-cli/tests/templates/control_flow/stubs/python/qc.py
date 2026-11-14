# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Deterministic quality-check stub for the render/QC and RFC 0013
render-until-clean samples.

Reads each rendered frame's `noise: X` line. A frame is bad when its noise is
over `threshold` (default 0.1), and the mean noise is over all frames. The
report has the bad frames as both `bad_frames` (the RFC 0011 render/QC
sample) and `noisy_frames` (the RFC 0013 render-until-clean sample). A frame
that was not rendered exits with a one-line error, so a test's output has no
traceback.
"""
import os
import re
import sys
from types import SimpleNamespace


def check_frames(pattern, frames, threshold=0.1):
    noise = {}
    for frame in frames:
        path = re.sub(r"#+", lambda m: f"{frame:0{len(m.group(0))}d}", pattern)
        if not os.path.exists(path):
            sys.exit(f"qc stub: frame {frame} was not rendered")
        with open(path, encoding="utf-8") as f:
            noise[frame] = float(f.read().split(": ", 1)[1])
    print(f"qc stub: noise {noise}")
    bad = [f for f in frames if noise[f] > threshold]
    return SimpleNamespace(
        bad_frames=bad,
        noisy_frames=bad,
        mean_noise=round(sum(noise.values()) / len(noise), 6),
    )
