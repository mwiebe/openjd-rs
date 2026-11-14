# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Deterministic relaxation solver stub for the RFC 0013 iterative solver sample.

`prepare(mesh, state)` reads the mesh file's `residual: X` line, writes the
state file with a step count of 0, and returns X. `relax_step(state, damping,
best)` counts the step in the state file (from 0 when there is none) and returns `best * (1 - damping)`,
except that step 1 overshoots and returns `best * 1.5`, so the sample halves
its damping once. Residuals are rounded to 6 digits. When
`$OPENJD_GOLDEN_SOLVER_FAIL_STEP` is set, that step exits with an error instead, so a
test can fail a body Step in a later iteration.
"""
import os
import sys


def prepare(mesh, state):
    with open(mesh, encoding="utf-8") as f:
        residual = float(f.read().split(": ", 1)[1])
    os.makedirs(os.path.dirname(state), exist_ok=True)
    with open(state, "w", encoding="utf-8") as f:
        f.write("0\n")
    print(f"solver stub: prepared, residual {residual}")
    return residual


def relax_step(state, damping, best):
    # A run of the loop alone (`--step Relax`) has no prepared state.
    step = 0
    if os.path.exists(state):
        with open(state, encoding="utf-8") as f:
            step = int(f.read())
    os.makedirs(os.path.dirname(state), exist_ok=True)
    with open(state, "w", encoding="utf-8") as f:
        f.write(f"{step + 1}\n")
    if os.environ.get("OPENJD_GOLDEN_SOLVER_FAIL_STEP") == str(step):
        sys.exit(f"solver stub: step {step} diverged")
    residual = round(best * 1.5 if step == 1 else best * (1 - damping), 6)
    print(f"solver stub: step {step}, damping {damping}, residual {residual}")
    return residual
