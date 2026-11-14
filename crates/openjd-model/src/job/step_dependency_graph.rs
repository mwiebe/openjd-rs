// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Step dependency graph for instantiated jobs.

use std::collections::HashMap;

use crate::error::ModelError;
use crate::job;

type NodeIndex = usize;

/// A dependency edge between two nodes (top-level Steps or loops).
#[derive(Debug)]
pub struct StepDependencyEdge {
    /// Node index of the Step or loop that is depended upon.
    pub origin: NodeIndex,
    /// Node index of the Step or loop that depends on origin.
    pub dependent: NodeIndex,
}

/// Whether a node is a Step or a loop (`CONTROL_FLOW`, RFC 0013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepNodeKind {
    /// A top-level Step: `step_index` indexes `job.steps`.
    Step,
    /// A loop: `step_index` indexes `job.loops`.
    Loop,
}

/// A node in the dependency graph.
#[derive(Debug)]
pub struct StepDependencyNode {
    /// The entry this node represents: an index into `job.steps` for a
    /// Step, into `job.loops` for a loop (see `kind`).
    pub step_index: usize,
    /// Whether the node is a Step or a loop.
    pub kind: StepNodeKind,
    /// The step name.
    pub name: String,
    /// Edges where this step depends on another (this step is the dependent).
    pub in_edges: Vec<usize>,
    /// Edges where another step depends on this one (this step is the origin).
    pub out_edges: Vec<usize>,
}

/// Dependency graph over the top-level entries of an instantiated job: its
/// Steps and its loops, as nodes in template order. For a Job without loops,
/// node `i` is `job.steps[i]`. A loop's body Steps are not nodes: their
/// dependencies stay within the body (RFC 0013 §`<LoopDefinition>`).
#[derive(Debug)]
pub struct StepDependencyGraph {
    nodes: Vec<StepDependencyNode>,
    edges: Vec<StepDependencyEdge>,
    name_to_index: HashMap<String, NodeIndex>,
}

impl StepDependencyGraph {
    /// Build the dependency graph from a Job.
    pub fn new(job: &job::Job) -> Result<Self, ModelError> {
        // Template order: each loop at its entry index, the Steps filling
        // the other places in order.
        let total = job.steps.len() + job.loops.len();
        let mut slots: Vec<Option<(StepNodeKind, usize)>> = vec![None; total];
        for (i, l) in job.loops.iter().enumerate() {
            match slots.get_mut(l.entry_index) {
                Some(slot @ None) => *slot = Some((StepNodeKind::Loop, i)),
                _ => {
                    return Err(ModelError::DecodeValidation(format!(
                        "Loop '{}' has entry index {}, which does not fit the Job's {total} Steps and loops",
                        l.name, l.entry_index
                    )))
                }
            }
        }
        for (next_step, slot) in slots.iter_mut().filter(|s| s.is_none()).enumerate() {
            *slot = Some((StepNodeKind::Step, next_step));
        }
        let entries: Vec<(StepNodeKind, usize, &str, Option<&Vec<job::StepDependency>>)> = slots
            .into_iter()
            .flatten()
            .map(|(kind, i)| match kind {
                StepNodeKind::Step => (
                    kind,
                    i,
                    job.steps[i].name.as_str(),
                    job.steps[i].dependencies.as_ref(),
                ),
                StepNodeKind::Loop => (
                    kind,
                    i,
                    job.loops[i].name.as_str(),
                    job.loops[i].dependencies.as_ref(),
                ),
            })
            .collect();
        let name_to_index: HashMap<String, NodeIndex> = entries
            .iter()
            .enumerate()
            .map(|(n, (_, _, name, _))| (name.to_string(), n))
            .collect();

        let mut nodes: Vec<StepDependencyNode> = entries
            .iter()
            .map(|(kind, i, name, _)| StepDependencyNode {
                step_index: *i,
                kind: *kind,
                name: name.to_string(),
                in_edges: Vec::new(),
                out_edges: Vec::new(),
            })
            .collect();

        let mut edges = Vec::new();

        for (dep_idx, (kind, _, name, deps)) in entries.iter().enumerate() {
            for dep in deps.iter().copied().flatten() {
                let origin_idx = *name_to_index.get(&dep.depends_on).ok_or_else(|| {
                    ModelError::DecodeValidation(format!(
                        "{} '{name}' depends on unknown step '{}'",
                        if matches!(kind, StepNodeKind::Loop) {
                            "Loop"
                        } else {
                            "Step"
                        },
                        dep.depends_on
                    ))
                })?;
                let edge_idx = edges.len();
                edges.push(StepDependencyEdge {
                    origin: origin_idx,
                    dependent: dep_idx,
                });
                nodes[dep_idx].in_edges.push(edge_idx);
                nodes[origin_idx].out_edges.push(edge_idx);
            }
        }

        Ok(Self {
            nodes,
            edges,
            name_to_index,
        })
    }

    /// Get a node by step name.
    pub fn step_node(&self, name: &str) -> Option<&StepDependencyNode> {
        self.name_to_index.get(name).map(|&i| &self.nodes[i])
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get an edge by index.
    pub fn edge(&self, index: usize) -> Option<&StepDependencyEdge> {
        self.edges.get(index)
    }

    /// Get a node by index.
    pub fn node(&self, index: usize) -> Option<&StepDependencyNode> {
        self.nodes.get(index)
    }

    /// Maximum in-degree (max number of dependencies any step has).
    pub fn max_indegree(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| n.in_edges.len())
            .max()
            .unwrap_or(0)
    }

    /// Maximum out-degree (max number of steps that depend on any single step).
    pub fn max_outdegree(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| n.out_edges.len())
            .max()
            .unwrap_or(0)
    }

    /// Stable topological sort matching the Python implementation.
    ///
    /// DFS-based: processes nodes in template order. For each unvisited node,
    /// pushes it onto a stack and explores dependencies (pushed in reverse
    /// template order so the earliest dependency is processed first).
    /// Returns node indices in dependency-respecting, template-stable order;
    /// for a Job without loops these are indices into `job.steps`. Use
    /// [`node`](Self::node) to tell a Step from a loop.
    pub fn topo_sorted(&self) -> Result<Vec<usize>, ModelError> {
        let n = self.nodes.len();
        // 0 = unvisited, 1 = started, 2 = completed
        let mut state = vec![0u8; n];
        let mut result = Vec::with_capacity(n);

        for i in 0..n {
            if state[i] != 0 {
                continue;
            }
            let mut stack: Vec<NodeIndex> = vec![i];
            while let Some(&top) = stack.last() {
                match state[top] {
                    2 => {
                        stack.pop();
                    }
                    1 => {
                        // Second visit — mark completed
                        state[top] = 2;
                        result.push(top);
                        stack.pop();
                    }
                    _ => {
                        // First visit — mark started, push unfinished deps
                        state[top] = 1;
                        // Collect dependency indices, sort by reverse template index
                        let mut dep_indices: Vec<NodeIndex> = self.nodes[top]
                            .in_edges
                            .iter()
                            .map(|&e| self.edges[e].origin)
                            .collect();
                        dep_indices.sort_unstable_by(|a, b| b.cmp(a));
                        for dep in dep_indices {
                            match state[dep] {
                                2 => {} // already done
                                1 => {
                                    // Build cycle path from the stack
                                    let cycle: Vec<&str> = stack
                                        .iter()
                                        .filter(|&&idx| state[idx] == 1)
                                        .map(|&idx| self.nodes[idx].name.as_str())
                                        .collect();
                                    let dep_name = &self.nodes[dep].name;
                                    return Err(ModelError::DecodeValidation(format!(
                                        "A circular dependency was found in the step dependency graph:\n{} -> {}",
                                        cycle.join(" -> "), dep_name
                                    )));
                                }
                                _ => stack.push(dep),
                            }
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    /// Convenience: topological sort returning step names.
    pub fn topo_sorted_names(&self) -> Result<Vec<String>, ModelError> {
        self.topo_sorted().map(|indices| {
            indices
                .into_iter()
                .map(|i| self.nodes[i].name.clone())
                .collect()
        })
    }
}
