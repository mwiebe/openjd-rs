# Step Dependency Graph

The `step_dependency_graph` module builds and queries a directed acyclic graph of step-to-step
dependencies from an instantiated `job::Job`. With `CONTROL_FLOW` loops (RFC 0013), a loop is
a node like a Step; its body Steps are not nodes, since their dependencies stay within the
body.

## Public API

```rust
impl StepDependencyGraph {
    pub fn new(job: &Job) -> Result<Self, ModelError>
    pub fn node_count(&self) -> usize
    pub fn step_node(&self, name: &str) -> Option<&StepDependencyNode>
    pub fn node(&self, index: usize) -> Option<&StepDependencyNode>
    pub fn edge(&self, index: usize) -> Option<&StepDependencyEdge>
    pub fn max_indegree(&self) -> usize
    pub fn max_outdegree(&self) -> usize
    pub fn topo_sorted(&self) -> Result<Vec<usize>, ModelError>
    pub fn topo_sorted_names(&self) -> Result<Vec<String>, ModelError>
}
```

## Types

```rust
pub struct StepDependencyGraph {
    nodes: Vec<StepDependencyNode>,
    edges: Vec<StepDependencyEdge>,
    name_to_index: HashMap<String, usize>,
}

pub enum StepNodeKind { Step, Loop }

pub struct StepDependencyNode {
    pub step_index: usize,        // into job.steps for a Step, job.loops for a loop
    pub kind: StepNodeKind,
    pub name: String,
    pub in_edges: Vec<usize>,     // Indices into edges vec
    pub out_edges: Vec<usize>,    // Indices into edges vec
}

pub struct StepDependencyEdge {
    pub origin: usize,            // Node index of the dependency (upstream Step or loop)
    pub dependent: usize,         // Node index of the dependent (downstream Step or loop)
}
```

## Construction

`StepDependencyGraph::new(job)` builds the graph from an instantiated job:

1. Creates a node for each top-level Step and loop, in template order: each loop at its
   `entry_index`, and the Steps filling the other places in `job.steps` order. For a Job
   without loops, node `i` is `job.steps[i]`. A loop whose `entry_index` does not fit is
   an error.
2. Builds `name_to_index` lookup map
3. For each node's dependencies, creates edges from the dependency target to the node
4. Errors on unknown dependency targets (`Step '<S>' depends on unknown step '<D>'`, or
   `Loop '<L>' ...` for a loop)

`topo_sorted()` returns node indices; `node(i).kind` tells a Step from a loop.

## Topological Sort

`topo_sorted()` returns node indices (for a Job without loops, indices into `job.steps`)
in dependency-respecting order using iterative
DFS-based topological sort with three-state marking.

**Algorithm:**
- Three states: Unvisited, Started (on stack), Completed
- Processes nodes in template order (index 0, 1, 2, ...)
- For each unvisited node, pushes dependencies in reverse template order before processing
- Completed nodes are prepended to the result (reverse post-order)
- Detects cycles via the Started state (back edge = cycle)

**Stability guarantee:** The sort is deterministic and matches the Python implementation's
behavior. Steps appear in template order unless dependency constraints force reordering.
This is important for reproducible scheduling.

`topo_sorted_names()` is a convenience wrapper that maps indices to step names.

## Design Decisions

### Edge-List Representation

The graph uses an edge-list representation with index-based references rather than
pointer-based adjacency. This avoids reference cycles (which the Python implementation
needs a manual `__del__` to handle) and is more cache-friendly.

### Stable Topological Sort

The iterative DFS-based sort processes nodes in template order and pushes dependencies in
reverse template order. This produces a stable, deterministic ordering that preserves
template order where dependency constraints allow.

### Cycle Detection at Two Levels

Cycles are detected in two places:
1. During template validation (Pass 6, `structure.rs`, `detect_dependency_cycles`) with
   Kahn's algorithm over Step names: on the top-level graph of Steps and loops, and on
   each loop body's graph (RFC 0013)
2. During `topo_sorted()` using DFS back-edge detection

`create_job` binds `CONTROL_FLOW` Steps in the order `step_dependency_order`
(`structure.rs`) gives: a separate Kahn's-algorithm pass over the indices of the
template's `steps` entries (Steps and loops), every Step
after its dependencies, ties broken by template index. It is kept apart from the cycle
check, whose name-keyed graph decides which errors a template with a repeated Step name
gets. When several orders are valid, `topo_sorted()` follows its DFS and
`step_dependency_order` the template index, so the error `create_job` reports first under
`CONTROL_FLOW` is the first in the latter.

The validation check catches cycles in templates before job creation. The `topo_sorted()`
check catches cycles in programmatically-constructed jobs that bypass template validation.

### Skipped and zero-Task dependencies (CONTROL_FLOW)

The step graph is fixed: `enableIf` and computed ranges never add or remove edges, so
`StepDependencyGraph` is the same for every Job of a template. A `Skipped` Step
(`job::StepBinding::Skipped`) and a bound Step with an empty parameter space both have
zero Tasks, and both satisfy the `dependsOn` of the Steps that depend on them once their
own dependencies are satisfied (RFC 0011 §Conditional execution, §Empty parameter
spaces). A skipped producer's `Steps.*` symbols are `null`; a zero-Task producer's
`Output` and `TaskParam` lists are `[]`. Scheduling by these rules is the caller's; the
graph only orders the Steps.
