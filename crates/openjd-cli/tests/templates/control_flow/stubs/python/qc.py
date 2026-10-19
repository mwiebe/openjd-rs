# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Deterministic quality-check stub for the render/QC sample.

Reads each rendered frame's `noise: X` line. A frame is bad when its noise is
over 0.1, and the mean noise is over all frames.
"""
import re
from types import SimpleNamespace


def check_frames(pattern, frames):
    noise = {}
    for frame in frames:
        path = re.sub(r"#+", lambda m: f"{frame:0{len(m.group(0))}d}", pattern)
        with open(path, encoding="utf-8") as f:
            noise[frame] = float(f.read().split(": ", 1)[1])
    print(f"qc stub: noise {noise}")
    return SimpleNamespace(
        bad_frames=[f for f in frames if noise[f] > 0.1],
        mean_noise=round(sum(noise.values()) / len(noise), 6),
    )
