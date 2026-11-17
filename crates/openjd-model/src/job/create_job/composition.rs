// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The dependency graph and Service scopes of a combined Job (RFC 0009
//! `<StepDependency>` constraint 3 and "Service scope"), computed at job
//! creation once every Step's and every Service's Job Environments are
//! known.
//!
//! Template validation applies the same rules with what it knows of one
//! Job Template ([`crate::template::compute_service_scopes`]); here every
//! `enableIf` has been decided and the attached Environments and Services
//! are part of the Job, so every edge is known. Nodes are the Job's Steps
//! and Services (external Services first, as in [`job::Job::services`]).
//! Edges run from each entity to each Step and Service its own
//! `dependencies` list, and from each Step and each Service whose Sessions
//! enter a Job Environment to each Service that Environment lists. A
//! `service: <name>` names a Service of the listing entity's document or,
//! in the Job Template, the attached Service a requirement was bound to
//! (§1.2.2 items 2–3).
//!
//! A Service's scope is the Steps that list it (rule 1), the Steps whose
//! Sessions enter a Job Environment that lists it (rule 3), and the scope
//! of every Service with an edge to it (rule 2). A Service listed by a Job
//! Environment that every Step enters, or one such a Service depends on,
//! has [`ServiceScope::AllSteps`].

use std::collections::{BTreeSet, HashMap};

use crate::job::{self, ServiceScope};
use crate::template::service_scope::DependencyGraph;
use crate::template::ServiceDependencyCycle;

use super::external::RequirementBinding;

/// Resolves the `service: <name>` entries of a combined Job to indices of
/// [`job::Job::services`].
pub(crate) struct ServiceResolver<'a> {
    services: &'a [job::Service],
    bindings: &'a [RequirementBinding],
}

impl<'a> ServiceResolver<'a> {
    pub(crate) fn new(services: &'a [job::Service], bindings: &'a [RequirementBinding]) -> Self {
        Self { services, bindings }
    }

    /// The Service a `service: <name>` entry written in `document` names:
    /// the Service of that document with that name or, in the Job
    /// Template, the attached Service the requirement `name` was bound to.
    pub(crate) fn resolve(&self, document: &job::Document, name: &str) -> Option<usize> {
        if let Some(k) = self
            .services
            .iter()
            .position(|s| &s.document == document && s.name == name)
        {
            return Some(k);
        }
        if !document.is_job_template() {
            return None;
        }
        let binding = self.bindings.iter().find(|b| b.requirement == name)?;
        self.services
            .iter()
            .position(|s| s.document == binding.document && s.name == binding.service)
    }

    /// The Services Job Environment `env`, declared by `document`, lists in
    /// its `dependencies`, by index, each once.
    pub(crate) fn listed_by(&self, env: &job::Environment, document: &job::Document) -> Vec<usize> {
        let mut out = Vec::new();
        for name in env.depends_on_services() {
            if let Some(k) = self.resolve(document, name) {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        out
    }
}

/// `Service X` for the Job Template's own, `Service X (<document>)` for an
/// external one: how the graph's messages name a Service.
pub(crate) fn service_label(svc: &job::Service) -> String {
    if svc.document.is_job_template() {
        format!("Service {}", svc.name)
    } else {
        format!("Service {} ({})", svc.name, svc.document)
    }
}

/// `Job Environment 'E'`, with its document when attached.
pub(crate) fn environment_label(env: &job::Environment, document: &job::Document) -> String {
    if document.is_job_template() {
        format!("Job Environment '{}'", env.name)
    } else {
        format!("Job Environment '{}' ({document})", env.name)
    }
}

/// The result of [`analyze`].
pub(crate) struct Composition {
    /// The first cycle of the combined graph, if there is one.
    pub(crate) cycle: Option<ServiceDependencyCycle>,
    /// The attached Environment Template every node of `cycle` belongs
    /// to, when they all belong to one: the cycle is then that
    /// document's, and is reported there.
    pub(crate) cycle_document: Option<job::Document>,
    /// Each Service's scope, index for index with `job.services`; empty
    /// when there is a cycle.
    pub(crate) scopes: Vec<ServiceScope>,
}

/// Build the combined graph of `job` and compute every Service's scope.
/// `documents` is the document of each entry of `job.job_environments`,
/// index for index; every Step's and Service's `environments` are set.
pub(crate) fn analyze(
    job: &job::Job,
    documents: &[job::Document],
    bindings: &[RequirementBinding],
) -> Composition {
    let services = job.services.as_deref().unwrap_or(&[]);
    let envs = job.job_environments.as_deref().unwrap_or(&[]);
    let resolver = ServiceResolver::new(services, bindings);
    let n_steps = job.steps.len();

    let mut graph = DependencyGraph::default();
    let mut step_index: HashMap<&str, usize> = HashMap::new();
    for step in &job.steps {
        let i = graph.node(format!("Step {}", step.name));
        step_index.entry(step.name.as_str()).or_insert(i);
    }
    for svc in services {
        graph.node(service_label(svc));
    }
    let env_lists: Vec<Vec<usize>> = envs
        .iter()
        .zip(documents)
        .map(|(env, doc)| resolver.listed_by(env, doc))
        .collect();
    let env_labels: Vec<String> = envs
        .iter()
        .zip(documents)
        .map(|(env, doc)| environment_label(env, doc))
        .collect();
    // entered[i][e]: the Sessions of node `i` enter Job Environment `e`.
    let entered: Vec<Vec<bool>> = job
        .steps
        .iter()
        .map(|s| {
            envs.iter()
                .zip(documents)
                .map(|(env, doc)| s.enters_environment(doc, &env.name))
                .collect()
        })
        .chain(services.iter().map(|svc| {
            envs.iter()
                .zip(documents)
                .map(|(env, doc)| svc.enters_environment(doc, &env.name))
                .collect()
        }))
        .collect();

    let jt = job::Document::JobTemplate;
    for (i, step) in job.steps.iter().enumerate() {
        for dep in step.dependencies.iter().flatten() {
            let to = match (dep.step(), dep.service()) {
                (Some(name), _) => step_index.get(name).copied(),
                (_, Some(name)) => resolver.resolve(&jt, name).map(|k| n_steps + k),
                _ => None,
            };
            if let Some(to) = to.filter(|to| *to != i) {
                graph.edge(i, to, None);
            }
        }
    }
    for (k, svc) in services.iter().enumerate() {
        let i = n_steps + k;
        for dep in svc.dependencies.iter().flatten() {
            let to = match (dep.step(), dep.service()) {
                (Some(name), _) if svc.document.is_job_template() => step_index.get(name).copied(),
                (_, Some(name)) => resolver.resolve(&svc.document, name).map(|k| n_steps + k),
                _ => None,
            };
            if let Some(to) = to.filter(|to| *to != i) {
                graph.edge(i, to, None);
            }
        }
    }
    for (i, row) in entered.iter().enumerate() {
        for (e, enters) in row.iter().enumerate() {
            if !enters {
                continue;
            }
            for &k in &env_lists[e] {
                graph.edge(i, n_steps + k, Some(env_labels[e].clone()));
            }
        }
    }
    if let Some(cycle) = graph.find_cycle() {
        // Each node's document: a Step's is the Job Template.
        let node_document = |label: &String| -> Option<&job::Document> {
            let i = graph.labels.iter().position(|l| l == label)?;
            Some(
                i.checked_sub(n_steps)
                    .map_or(&jt, |k| &services[k].document),
            )
        };
        // An attached Service's edges reach only its own document's
        // Services and earlier attachments', never back, so a cycle through
        // one stays in its document; the check on every node keeps that an
        // invariant this code does not rely on.
        let first = cycle.path.first().and_then(node_document);
        let cycle_document = first
            .filter(|d| !d.is_job_template())
            .filter(|d| cycle.path.iter().all(|l| node_document(l) == Some(*d)))
            .cloned();
        return Composition {
            cycle: Some(cycle),
            cycle_document,
            scopes: Vec::new(),
        };
    }

    // For each node, the Steps that reach it; the graph is acyclic, so
    // propagation reaches a fixed point.
    let n = graph.labels.len();
    let mut reverse: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, edges) in graph.edges.iter().enumerate() {
        for e in edges {
            reverse[e.to].push(i);
        }
    }
    let mut reach: Vec<BTreeSet<usize>> = (0..n)
        .map(|i| {
            if i < n_steps {
                std::iter::once(i).collect()
            } else {
                BTreeSet::new()
            }
        })
        .collect();
    loop {
        let mut changed = false;
        for to in n_steps..n {
            let add: Vec<usize> = reverse[to]
                .iter()
                .flat_map(|from| reach[*from].iter().copied())
                .collect();
            for s in add {
                changed |= reach[to].insert(s);
            }
        }
        if !changed {
            break;
        }
    }
    // `AllSteps`: listed by a Job Environment every Step enters, or
    // depended on by a Service that is.
    let mut all_steps = vec![false; n];
    for (e, listed) in env_lists.iter().enumerate() {
        if n_steps > 0 && (0..n_steps).all(|s| entered[s][e]) {
            for &k in listed {
                all_steps[n_steps + k] = true;
            }
        }
    }
    loop {
        let mut changed = false;
        for from in n_steps..n {
            if !all_steps[from] {
                continue;
            }
            for e in &graph.edges[from] {
                if e.to >= n_steps && !all_steps[e.to] {
                    all_steps[e.to] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let scopes = (0..services.len())
        .map(|k| {
            let node = n_steps + k;
            if all_steps[node] {
                ServiceScope::AllSteps
            } else {
                ServiceScope::steps(reach[node].iter().map(|s| job.steps[*s].name.clone()))
            }
        })
        .collect();
    Composition {
        cycle: None,
        cycle_document: None,
        scopes,
    }
}
