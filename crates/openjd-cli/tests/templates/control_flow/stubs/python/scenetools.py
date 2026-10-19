# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Deterministic scene-graph stub: the scene file is JSON, a list of nodes
with `name` and `children`."""
import json
from types import SimpleNamespace


def load_graph(scene):
    with open(scene, encoding="utf-8") as f:
        nodes = json.load(f)
    return SimpleNamespace(nodes=[SimpleNamespace(name=n["name"], child_indices=n["children"]) for n in nodes])
