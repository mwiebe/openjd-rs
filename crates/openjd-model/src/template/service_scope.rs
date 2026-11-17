// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Service scope (Template Schemas §9.1, RFC 0009 "Service scope"), the
//! dependency graph (§3.2 constraint 3), and which Job Environments each
//! entity's Sessions enter, as known at template validation.
//!
//! A Job Environment is entered by every Session of the Job when its
//! `activation` is `ALWAYS` or `BY_DEFAULT` (except, for `BY_DEFAULT`, the
//! Sessions of a Step or Service that declines it with a disabled
//! `environment:` entry) and by the Sessions of the Steps and Services that
//! request it when `ON_REQUEST`, less any for which its `enableIf` is
//! `false` (which an `ALWAYS` one cannot be) or that an earlier Environment
//! of its `exclusiveGroup` supersedes. A Service's Sessions select
//! Environments exactly as a Step's do, with no exception for an
//! Environment that lists the Service ([`Entrances`]; RFC 0009 "Services run
//! inside Environments").
//!
//! The scope of a Service declared in a Job Template's `services` is the set
//! of Steps whose Tasks depend on it, computed from the template's
//! `dependencies` lists:
//!
//! 1. a Step that lists Service `X` in its `dependencies` is in `X`'s scope;
//! 2. when Service `Y` depends on Service `X` — lists it, or its Sessions
//!    enter a Job Environment that lists it — every Step in `Y`'s scope is
//!    in `X`'s scope, transitively through any chain of Services;
//! 3. when a `jobEnvironments` entry lists Service `X` in its
//!    `dependencies`, every Step whose Sessions enter it is in `X`'s scope:
//!    every Step for an `ALWAYS` Environment, every Step that does not
//!    decline it for a `BY_DEFAULT` one, the requesting Steps for an
//!    `ON_REQUEST` one, less any for which its `enableIf` is `false` or that
//!    an earlier one of its `exclusiveGroup` supersedes;
//! 4. a Service in whose scope no Step falls is unused, and the template is
//!    rejected naming it ([`ComputedServiceScope::is_unused`]).
//!
//! The `dependencies` of a template's Steps, Services, and Job Environments
//! together form one graph — Step-to-Step, Step-to-Service, Service-to-Step
//! and Service-to-Service edges, plus an edge from each Step and each
//! Service whose Sessions enter a Job Environment to each Service that
//! Environment lists — which must be acyclic (§3.2 constraint 3), with no
//! exception. So a Service a Job Environment lists, or any Service it
//! depends on transitively, cannot depend on a Step whose Sessions enter
//! that Environment, and a Service whose Sessions enter an Environment that
//! lists it is the cycle `X -> X`. [`compute_service_scopes`] applies the
//! rules to a decoded Job Template with what is known at template
//! validation (RFC 0010 §7.6 rule 5) — an edge or a scope member that
//! depends on an `enableIf` that is not known, or on an `exclusiveGroup`,
//! is left to job creation — reporting the first cycle found as a
//! [`ServiceDependencyCycle`], which names the Environment whose entering
//! closed it; [`environment_template_service_cycle`] does the same for an
//! Environment Template's Services, which have no Steps to depend on.
//!
//! `Service.*` values are available exactly to the entities that list the
//! Service — a Step, a Service, or a Job Environment — so a reference is
//! never an implicit edge, and never an implicit dependency. The reference
//! extraction helpers at the end of this module ([`step_references`],
//! [`environment_references`], [`service_references`]) serve the
//! diagnostic that names the missing `service: <name>` entry when an
//! entity references a Service it does not list. The scheduler side of the
//! rules (start before any Task of a Step in scope, stop once none
//! remains) is the runtime's.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::actions::{Action, CancelationMode};
use super::environment::{apply_exclusive_groups, EmbeddedFile, Environment, Known, KnownEntry};
use super::environment_template::EnvironmentTemplate;
use super::job_template::JobTemplate;
use super::service::{Service, ServiceRequirement};
use super::step::{listed_service_names, listed_step_names, StepDependency, StepTemplate};
use crate::format_string::FormatString;

/// The Steps whose Tasks depend on a Service (§9.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServiceScope {
    /// Every Step of the Job: a Service listed in the `dependencies` of a
    /// Job Environment that every Step's Sessions enter, or one a Job-wide
    /// Service depends on. The Service
    /// lives as long as the Job.
    AllSteps,
    /// Exactly these Steps, by name. The Service is started before the first
    /// Task of any of them and stopped once none has a Task left to run. An
    /// empty set is a Service nothing lists: validation rejects an inline
    /// Service or an `externalServices` entry in that state, and an attached
    /// Service in that state is never started (§1.2.2).
    Steps {
        /// The Step names, sorted.
        steps: BTreeSet<String>,
    },
}

impl ServiceScope {
    /// A scope of exactly the named Steps.
    #[must_use]
    pub fn steps<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Steps {
            steps: names.into_iter().map(Into::into).collect(),
        }
    }

    /// `AllSteps` — the serde default for a serialized Service that omits
    /// its scope (one written before scopes were recorded).
    #[must_use]
    pub fn all_steps_default() -> Self {
        Self::AllSteps
    }

    /// True when every Step is in the scope.
    #[must_use]
    pub fn is_all_steps(&self) -> bool {
        matches!(self, Self::AllSteps)
    }

    /// True when the Step named `step` is in the scope.
    #[must_use]
    pub fn contains(&self, step: &str) -> bool {
        match self {
            Self::AllSteps => true,
            Self::Steps { steps } => steps.contains(step),
        }
    }

    /// The Step names of a [`ServiceScope::Steps`] scope; `None` for
    /// [`ServiceScope::AllSteps`].
    #[must_use]
    pub fn step_names(&self) -> Option<&BTreeSet<String>> {
        match self {
            Self::AllSteps => None,
            Self::Steps { steps } => Some(steps),
        }
    }
}

/// `every Step`, `Step A`, `Steps A, B`, or `no Step` — the form the run
/// log uses.
impl fmt::Display for ServiceScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllSteps => f.write_str("every Step"),
            Self::Steps { steps } => {
                if steps.is_empty() {
                    f.write_str("no Step")
                } else if steps.len() == 1 {
                    write!(f, "Step {}", steps.iter().next().expect("one step"))
                } else {
                    let names: Vec<&str> = steps.iter().map(String::as_str).collect();
                    write!(f, "Steps {}", names.join(", "))
                }
            }
        }
    }
}

/// Whether an entity's Sessions enter a Job Environment, as far as it is
/// known at template validation (RFC 0009 "Services run inside
/// Environments"; RFC 0010 §4, after item 10, and §7.6 rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entrance {
    /// Known to enter: every `enableIf` the answer depends on is known (an
    /// `ALWAYS` Environment is entered whatever its own `enableIf` or the
    /// entity's entry for it says, each of which is an error of its own
    /// when `false`), and no earlier Environment of its `exclusiveGroup`
    /// may supersede it.
    Enters,
    /// Known not to enter.
    DoesNotEnter,
    /// Decided at job creation: an `enableIf` the answer depends on is not
    /// known, or the Environment has an `exclusiveGroup`.
    Undetermined,
}

impl Entrance {
    /// True unless known not to enter.
    #[must_use]
    pub fn may_enter(self) -> bool {
        self != Self::DoesNotEnter
    }
}

/// For every Step and every Service of `jt`, and every `jobEnvironments`
/// entry, index for index, whether the entity's Sessions enter it as known
/// at template validation.
///
/// A Service's Sessions enter Environments exactly as a Step's do (RFC
/// 0009 "Services run inside Environments"), with no exception for an
/// Environment that lists the Service: entering one is the cycle `X -> X`.
/// Each entrance is RFC 0010's known state (Template Schemas §4, after item
/// 10): from the Environment's `activation`, what its own `enableIf` is
/// known to be, and what the entity's `environment:` entry for it is known
/// to be (§7.6 rule 5), then the `exclusiveGroup` rule. A grouped
/// Environment is never known to be entered here, since an attached
/// Environment of its group may come first and supersede it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entrances {
    /// `steps[i][e]`: Step `i` and Job Environment `e`.
    pub steps: Vec<Vec<Entrance>>,
    /// `services[k][e]`: Service `k` of `jt.services` and Job Environment
    /// `e`.
    pub services: Vec<Vec<Entrance>>,
}

impl Entrances {
    /// Compute every entrance of `jt`, with each `enableIf` known as
    /// template validation knows it in the document's own context
    /// ([`JobTemplate::default_validation_context`]).
    #[must_use]
    pub fn of(jt: &JobTemplate) -> Self {
        Self::from_known(
            jt,
            &crate::template::validate_v2023_09::format_strings::known_enable_ifs(
                jt,
                &jt.default_validation_context(),
            ),
        )
    }

    /// Compute every entrance of `jt` from what each `enableIf` is known to
    /// be.
    pub(crate) fn from_known(
        jt: &JobTemplate,
        known: &crate::template::validate_v2023_09::format_strings::KnownEnableIfs,
    ) -> Self {
        let envs = jt.job_environments.as_deref().unwrap_or(&[]);
        let row = |entry: &dyn Fn(&str) -> KnownEntry| -> Vec<Entrance> {
            let states = envs.iter().enumerate().map(|(k, env)| {
                (
                    env.group(),
                    env.known_state(known.job_environment(k), entry(&env.name)),
                )
            });
            apply_exclusive_groups(states)
                .into_iter()
                .zip(envs)
                .map(|(state, env)| match state {
                    Known::Entered if env.group().is_some() => Entrance::Undetermined,
                    Known::Entered => Entrance::Enters,
                    Known::NotEntered => Entrance::DoesNotEnter,
                    Known::Undetermined => Entrance::Undetermined,
                })
                .collect()
        };
        Self {
            steps: jt
                .steps
                .iter()
                .enumerate()
                .map(|(i, step)| row(&|name| known.entry_in_step(i, step, name)))
                .collect(),
            services: jt
                .services()
                .iter()
                .enumerate()
                .map(|(k, svc)| row(&|name| known.entry_in_service(k, svc, name)))
                .collect(),
        }
    }
}

/// What the scope rules found for one inline Service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedServiceScope {
    /// The Service's `name`.
    pub name: String,
    /// The Steps known at template validation to depend on the Service:
    /// those whose dependence follows from entries and Environments whose
    /// `enableIf` is known (§7.6 rule 5). Where an `enableIf` that is not
    /// known decides more, job creation computes the final scope.
    pub scope: ServiceScope,
    /// False when no Step can be in the scope, whatever every `enableIf`
    /// expression resolves to (rule 4, [`is_unused`](Self::is_unused)).
    pub possibly_used: bool,
    /// The other inline Services this Service lists with the `service` key
    /// (§9 item 4): it starts after each is READY and stops before any of
    /// them. Sorted; never contains the Service itself, a required external
    /// Service, or a name the template does not declare.
    pub depends_on_services: BTreeSet<String>,
    /// The Steps this Service lists in its `dependencies` (§9 item 4): it
    /// starts only after each has completed. In list order; names the
    /// template does not declare are kept (validation reports them).
    pub depends_on_steps: Vec<String>,
    /// The Steps that list the Service directly (rule 1), in template
    /// order.
    pub dependent_steps: Vec<String>,
    /// The Services that list the Service (rule 2), in template order.
    pub dependent_services: Vec<String>,
    /// The `jobEnvironments` entries that list the Service in their
    /// `dependencies` (rule 3), in template order.
    pub listing_environments: Vec<String>,
    /// True when a `jobEnvironments` entry that every Step is known to
    /// enter lists the Service (rule 3): every Step is in the scope.
    pub listed_by_job_environment: bool,
}

impl ComputedServiceScope {
    /// True when no Step can be in the scope (rule 4): the template must be
    /// rejected, naming the Service.
    #[must_use]
    pub fn is_unused(&self) -> bool {
        !self.possibly_used
    }
}

/// The scopes of every Service of a Job Template, by name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceScopes {
    by_name: BTreeMap<String, ComputedServiceScope>,
    /// For each `externalServices` entry, by name: the Steps known to be in
    /// the scope it confers, and whether any Step can be.
    required: BTreeMap<String, (ServiceScope, bool)>,
}

impl ServiceScopes {
    /// The computed scope of the Service named `name`, if the template
    /// declares one.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ComputedServiceScope> {
        self.by_name.get(name)
    }

    /// Every Service's result, in name order.
    pub fn iter(&self) -> impl Iterator<Item = &ComputedServiceScope> {
        self.by_name.values()
    }

    /// The scope of `name`, or an empty scope for a name the template does
    /// not declare.
    #[must_use]
    pub fn scope_of(&self, name: &str) -> ServiceScope {
        self.by_name.get(name).map_or_else(
            || ServiceScope::steps::<[&str; 0], &str>([]),
            |c| c.scope.clone(),
        )
    }

    /// The scope each `externalServices` entry confers on the attached
    /// Service it is matched to at submission, by the same rules as an
    /// inline Service's (§9.1): the Steps that list it (rule 1), the scopes
    /// of the inline Services that depend on it (rule 2), and the Steps
    /// whose Sessions enter a Job Environment that lists it (rule 3), as
    /// known at template validation. An entry nothing lists has an empty
    /// scope, which validation rejects (rule 4).
    #[must_use]
    pub fn required_service_scopes(&self) -> BTreeMap<String, ServiceScope> {
        self.required
            .iter()
            .map(|(name, (scope, _))| (name.clone(), scope.clone()))
            .collect()
    }

    /// True when some Step can be in the scope of the `externalServices`
    /// entry `name` (rule 4); false for a name no entry has.
    #[must_use]
    pub fn requirement_possibly_used(&self, name: &str) -> bool {
        self.required.get(name).is_some_and(|(_, used)| *used)
    }
}

/// How entering an Environment closed a dependency cycle (§3.2 constraint
/// 3): the Sessions of `from` enter `environment`, which lists `service`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleViaEnvironment {
    /// The node whose Sessions enter the Environment: `Step X` or
    /// `Service X`.
    pub from: String,
    /// The Environment, as messages name it: `Job Environment 'E'`, or
    /// `Environment 'E'` in an Environment Template, with the document
    /// for an attached one.
    pub environment: String,
    /// The Service the Environment lists.
    pub service: String,
}

/// A cycle in the graph the `dependencies` of a template's Steps, Services,
/// and Job Environments form (§3.2 constraint 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDependencyCycle {
    /// The nodes along the cycle — `Step X` or `Service X` — starting and
    /// ending with the same node: `["Step Use", "Service Indexer", "Step
    /// Use"]`.
    pub path: Vec<String>,
    /// When an edge of the cycle is one entering an Environment adds (each
    /// Step and Service whose Sessions enter a Job Environment depends on
    /// each Service it lists): the first such edge, for the message.
    pub via_environment: Option<CycleViaEnvironment>,
}

impl fmt::Display for ServiceDependencyCycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "dependencies contain a cycle: {}",
            self.path.join(" -> ")
        )?;
        match &self.via_environment {
            // The edge `X -> X`: the Service's Sessions enter an
            // Environment that lists it, by its `activation` or through the
            // Service's own `environment:` entry. There is no exception for
            // it (RFC 0009 "Services run inside Environments").
            Some(via) if self.path.len() == 2 => write!(
                f,
                " (the Sessions of {} enter {}, which lists Service '{}', so it would wait for \
                 itself to be READY. There is no exception for an Environment that lists the \
                 Service: keep the Service's Sessions out of it: drop any 'environment:' \
                 dependency of the Service that requests it, and decline a BY_DEFAULT one with a \
                 disabled 'environment:' dependency of the Service, or make it ON_REQUEST and \
                 request it only from the Steps that need it; Template Schemas §3.2 constraint \
                 3).",
                via.from, via.environment, via.service
            ),
            Some(via) => write!(
                f,
                " (the Sessions of {} enter {}, which lists Service '{}', so {} depends on it; \
                 Template Schemas §3.2 constraint 3).",
                via.from, via.environment, via.service, via.from
            ),
            None => f.write_str("."),
        }
    }
}

/// One out-edge of a dependency graph, by node index, with the Environment
/// it comes from when entering an Environment added it rather than the
/// entity's own `dependencies`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphEdge {
    pub(crate) to: usize,
    pub(crate) via_environment: Option<String>,
}

/// A dependency graph (§3.2 constraint 3): node labels (`Step X`,
/// `Service X`) and each node's out-edges, index for index.
#[derive(Debug, Clone, Default)]
pub(crate) struct DependencyGraph {
    pub(crate) labels: Vec<String>,
    pub(crate) edges: Vec<Vec<GraphEdge>>,
}

impl DependencyGraph {
    /// Add a node and return its index.
    pub(crate) fn node(&mut self, label: String) -> usize {
        self.labels.push(label);
        self.edges.push(Vec::new());
        self.labels.len() - 1
    }

    /// Add the edge `from -> to` unless it is already there; an edge the
    /// entity's own `dependencies` wrote is kept in preference to one that
    /// entering an Environment adds.
    pub(crate) fn edge(&mut self, from: usize, to: usize, via_environment: Option<String>) {
        let edges = &mut self.edges[from];
        if let Some(existing) = edges.iter_mut().find(|e| e.to == to) {
            if via_environment.is_none() {
                existing.via_environment = None;
            }
            return;
        }
        edges.push(GraphEdge {
            to,
            via_environment,
        });
    }

    /// DFS in node order; the first back edge found yields the cycle,
    /// spelled from the revisited node back to itself (a self-edge is the
    /// cycle `X -> X`). When an edge along the cycle is one entering an
    /// Environment added, the cycle names the first such edge.
    pub(crate) fn find_cycle(&self) -> Option<ServiceDependencyCycle> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            New,
            Open,
            Done,
        }
        let n = self.labels.len();
        let mut marks = vec![Mark::New; n];
        // The DFS stack, one frame per open node: the node, the edge that
        // reached it (`None` for a root), and the index of its next
        // out-edge. An explicit stack rather than recursion, since a long
        // `dependsOn` chain is a long path and recursion per edge would
        // overflow the thread's stack.
        let mut stack: Vec<(usize, Option<&GraphEdge>, usize)> = Vec::new();
        for root in 0..n {
            if marks[root] != Mark::New {
                continue;
            }
            marks[root] = Mark::Open;
            stack.push((root, None, 0));
            while let Some(&(at, _, next_edge)) = stack.last() {
                let Some(edge) = self.edges[at].get(next_edge) else {
                    marks[at] = Mark::Done;
                    stack.pop();
                    continue;
                };
                stack.last_mut().expect("frame is on the stack").2 += 1;
                match marks[edge.to] {
                    Mark::Done => {}
                    Mark::Open => {
                        let start = stack
                            .iter()
                            .position(|(i, _, _)| *i == edge.to)
                            .expect("open nodes are on the stack");
                        let mut path: Vec<String> = stack[start..]
                            .iter()
                            .map(|(i, _, _)| self.labels[*i].clone())
                            .collect();
                        path.push(self.labels[edge.to].clone());
                        // The edges along the cycle with the node each
                        // leaves: those that reached each node after the
                        // first, plus the closing edge.
                        let via_environment = stack[start + 1..]
                            .iter()
                            .enumerate()
                            .filter_map(|(k, (_, e, _))| e.map(|e| (stack[start + k].0, e)))
                            .chain(std::iter::once((at, edge)))
                            .find_map(|(from, e)| {
                                e.via_environment.as_ref().map(|env| CycleViaEnvironment {
                                    from: self.labels[from].clone(),
                                    environment: env.clone(),
                                    // The Service's name alone: a label
                                    // may add its document, `Service X
                                    // (EnvironmentTemplate[0])`, and a
                                    // name never holds a space.
                                    service: self.labels[e.to]
                                        .strip_prefix("Service ")
                                        .unwrap_or(&self.labels[e.to])
                                        .split(' ')
                                        .next()
                                        .unwrap_or_default()
                                        .to_string(),
                                })
                            });
                        return Some(ServiceDependencyCycle {
                            path,
                            via_environment,
                        });
                    }
                    Mark::New => {
                        marks[edge.to] = Mark::Open;
                        stack.push((edge.to, Some(edge), 0));
                    }
                }
            }
        }
        None
    }
}

/// The out-edges of `deps` among the declared `steps` and `services`, by
/// node index; unknown names, malformed entries, `environment:` entries and
/// the entity itself are not edges (validation reports them separately).
fn static_edges(
    deps: Option<&[StepDependency]>,
    this: usize,
    step_index: &HashMap<&str, usize>,
    service_index: &HashMap<&str, usize>,
) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for dep in deps.into_iter().flatten() {
        let node = match dep.target() {
            Some(super::step::DependencyTarget::Step(name, _)) => step_index.get(name),
            Some(super::step::DependencyTarget::Service(name)) => service_index.get(name),
            // An `ENV_PARAMS` `environment:` entry is not an edge of its
            // own (§3.2 constraint 3).
            Some(super::step::DependencyTarget::Environment(_)) | None => None,
        };
        if let Some(&node) = node {
            if node != this && !out.contains(&node) {
                out.push(node);
            }
        }
    }
    out
}

/// Compute the scope of every Service of `jt` (§9.1), or report the first
/// cycle in the combined dependency graph of its Steps, Services, and Job
/// Environments, with what is known at template validation.
///
/// The graph's edges are each entity's `dependsOn` / `step` / `service`
/// entries, and an edge from each Step and Service whose Sessions are known
/// to enter a Job Environment ([`Entrances::of`]) to each inline Service that
/// Environment lists (§3.2 constraint 3). A cycle is reported only when all
/// its edges are known; an edge that depends on an `enableIf` that is not
/// known, or on an `exclusiveGroup`, is left to job creation. A Service
/// whose Sessions enter an Environment that lists it, whether by the
/// Environment's `activation` or through its own `environment:` entry, is
/// the edge `X -> X`: there is no exception for it.
///
/// The scope of a Service is the Steps that list it (rule 1), the Steps
/// known to enter a Job Environment that lists it (rule 3), and the scope
/// of every Service that depends on it, by listing it or by entering a Job
/// Environment that lists it (rule 2). [`ComputedServiceScope::possibly_used`]
/// is the same closure over the edges that may exist.
///
/// The names in `jt.external_services` are external Services: a `service`
/// entry naming one is not an edge of the graph and does not place anything
/// in an inline Service's scope; the scopes the entries confer are
/// [`ServiceScopes::required_service_scopes`].
pub fn compute_service_scopes(jt: &JobTemplate) -> Result<ServiceScopes, ServiceDependencyCycle> {
    compute_service_scopes_with(jt, &Entrances::of(jt))
}

/// [`compute_service_scopes`] from the [`Entrances`] already computed.
pub(crate) fn compute_service_scopes_with(
    jt: &JobTemplate,
    entrances: &Entrances,
) -> Result<ServiceScopes, ServiceDependencyCycle> {
    let services = jt.services();
    let envs = jt.job_environments.as_deref().unwrap_or(&[]);
    let n_steps = jt.steps.len();

    let mut graph = DependencyGraph::default();
    let mut step_index: HashMap<&str, usize> = HashMap::new();
    for step in &jt.steps {
        let i = graph.node(format!("Step {}", step.name));
        step_index.entry(step.name.as_str()).or_insert(i);
    }
    let mut service_index: HashMap<&str, usize> = HashMap::new();
    for svc in services {
        let i = graph.node(format!("Service {}", svc.name));
        service_index.entry(svc.name.as_str()).or_insert(i);
    }
    let deps_of = |i: usize| {
        if i < n_steps {
            jt.steps[i].dependencies.as_deref()
        } else {
            services[i - n_steps].dependencies.as_deref()
        }
    };
    let entrance_of = |i: usize, e: usize| {
        if i < n_steps {
            entrances.steps[i][e]
        } else {
            entrances.services[i - n_steps][e]
        }
    };
    // The inline Services each Job Environment lists, by node index.
    let env_lists: Vec<Vec<usize>> = envs
        .iter()
        .map(|env| {
            let mut v: Vec<usize> = Vec::new();
            for name in env.listed_services() {
                if let Some(&k) = service_index.get(name) {
                    if !v.contains(&k) {
                        v.push(k);
                    }
                }
            }
            v
        })
        .collect();
    // Edges that may exist but are not known: (from, to).
    let mut possible: Vec<(usize, usize)> = Vec::new();
    for i in 0..graph.labels.len() {
        for to in static_edges(deps_of(i), i, &step_index, &service_index) {
            graph.edge(i, to, None);
        }
        for (e, listed) in env_lists.iter().enumerate() {
            for &to in listed {
                match entrance_of(i, e) {
                    Entrance::Enters => {
                        graph.edge(i, to, Some(format!("Job Environment '{}'", envs[e].name)));
                    }
                    Entrance::Undetermined => possible.push((i, to)),
                    Entrance::DoesNotEnter => {}
                }
            }
        }
    }
    if let Some(cycle) = graph.find_cycle() {
        return Err(cycle);
    }
    if services.is_empty() && jt.external_services().is_empty() {
        return Ok(ServiceScopes::default());
    }

    // Reverse edges into each Service: the Steps that list it (rule 1),
    // the Services that list it (rule 2), as written.
    let mut dependent_steps: HashMap<usize, Vec<String>> = HashMap::new();
    let mut dependent_services: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, edges) in graph.edges.iter().enumerate() {
        for edge in edges.iter().filter(|e| e.via_environment.is_none()) {
            if edge.to < n_steps {
                continue;
            }
            let name = if i < n_steps {
                jt.steps[i].name.clone()
            } else {
                services[i - n_steps].name.clone()
            };
            if i < n_steps {
                dependent_steps.entry(edge.to).or_default().push(name);
            } else {
                dependent_services.entry(edge.to).or_default().push(name);
            }
        }
    }

    // The known and the possible closures, over the reverse graph: a
    // Service's scope holds each Step with an edge to it and the scope of
    // each Service with one. The known graph is acyclic; the possible one
    // need not be, so both are computed by propagation to a fixed point
    // rather than by recursion.
    let mut reverse_known: Vec<Vec<usize>> = vec![Vec::new(); graph.labels.len()];
    for (i, edges) in graph.edges.iter().enumerate() {
        for e in edges {
            reverse_known[e.to].push(i);
        }
    }
    let mut reverse_possible = reverse_known.clone();
    for (from, to) in &possible {
        reverse_possible[*to].push(*from);
    }
    // A Job Environment every Step is known to enter that lists a Service
    // makes it a Service of every Step (the old rule 3's `AllSteps`).
    let all_step_envs: Vec<usize> = (0..envs.len())
        .filter(|e| n_steps > 0 && (0..n_steps).all(|s| entrances.steps[s][*e] == Entrance::Enters))
        .collect();
    let mut all_steps: Vec<bool> = vec![false; graph.labels.len()];
    for &e in &all_step_envs {
        for &k in &env_lists[e] {
            all_steps[k] = true;
        }
    }
    let known_steps = |reverse: &[Vec<usize>]| -> Vec<BTreeSet<usize>> {
        // For each node, the Steps that reach it.
        let mut reach: Vec<BTreeSet<usize>> = (0..graph.labels.len())
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
            for to in n_steps..graph.labels.len() {
                let mut add: Vec<usize> = Vec::new();
                for &from in &reverse[to] {
                    add.extend(reach[from].iter().copied());
                }
                for s in add {
                    changed |= reach[to].insert(s);
                }
            }
            if !changed {
                break;
            }
        }
        reach
    };
    let known_reach = known_steps(&reverse_known);
    let possible_reach = if possible.is_empty() {
        known_reach.clone()
    } else {
        known_steps(&reverse_possible)
    };
    // `AllSteps` propagates along known edges from a Service to the
    // Services it depends on.
    loop {
        let mut changed = false;
        for from in n_steps..graph.labels.len() {
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
    let scope_of_node = |k: usize| {
        if all_steps[k] {
            ServiceScope::AllSteps
        } else {
            ServiceScope::steps(known_reach[k].iter().map(|s| jt.steps[*s].name.clone()))
        }
    };

    let mut by_name = BTreeMap::new();
    for (k, svc) in services.iter().enumerate() {
        let node = n_steps + k;
        if service_index.get(svc.name.as_str()) != Some(&node) {
            // A duplicated name (reported by pass 12) keeps the first.
            continue;
        }
        let name = svc.name.as_str();
        let depends_on_services = listed_service_names(svc.dependencies.as_deref())
            .filter(|n| *n != name && service_index.contains_key(n))
            .map(str::to_string)
            .collect();
        let depends_on_steps = listed_step_names(svc.dependencies.as_deref())
            .map(str::to_string)
            .collect();
        by_name.insert(
            svc.name.clone(),
            ComputedServiceScope {
                name: svc.name.clone(),
                scope: scope_of_node(node),
                possibly_used: all_steps[node] || !possible_reach[node].is_empty(),
                depends_on_services,
                depends_on_steps,
                dependent_steps: dependent_steps.get(&node).cloned().unwrap_or_default(),
                dependent_services: dependent_services.get(&node).cloned().unwrap_or_default(),
                listing_environments: envs
                    .iter()
                    .zip(&env_lists)
                    .filter(|(_, l)| l.contains(&node))
                    .map(|(env, _)| env.name.clone())
                    .collect(),
                listed_by_job_environment: all_step_envs
                    .iter()
                    .any(|e| env_lists[*e].contains(&node)),
            },
        );
    }

    // The scope each `externalServices` entry confers: its listers' reach,
    // as for an inline Service, without being a node of the graph (nothing
    // in the Job Template is downstream of it).
    let mut required = BTreeMap::new();
    for req in jt.external_services() {
        let name = req.name.as_str();
        let mut known: BTreeSet<usize> = BTreeSet::new();
        let mut maybe: BTreeSet<usize> = BTreeSet::new();
        let mut all = false;
        for i in 0..graph.labels.len() {
            let lists = super::step::lists_service(deps_of(i), name);
            let via_env = envs
                .iter()
                .enumerate()
                .fold(Entrance::DoesNotEnter, |acc, (e, env)| {
                    if !env.listed_services().any(|n| n == name) {
                        return acc;
                    }
                    match (acc, entrance_of(i, e)) {
                        (Entrance::Enters, _) | (_, Entrance::Enters) => Entrance::Enters,
                        (Entrance::Undetermined, _) | (_, Entrance::Undetermined) => {
                            Entrance::Undetermined
                        }
                        _ => Entrance::DoesNotEnter,
                    }
                });
            let k = if lists { Entrance::Enters } else { via_env };
            if k == Entrance::DoesNotEnter {
                continue;
            }
            if k == Entrance::Enters {
                known.extend(known_reach[i].iter().copied());
                all |= i >= n_steps && all_steps[i];
            }
            maybe.extend(possible_reach[i].iter().copied());
        }
        // Every Step known to enter one Job Environment that lists it.
        if all_step_envs
            .iter()
            .any(|e| envs[*e].listed_services().any(|n| n == name))
        {
            all = true;
        }
        let scope = if all {
            ServiceScope::AllSteps
        } else {
            ServiceScope::steps(known.iter().map(|s| jt.steps[*s].name.clone()))
        };
        required.insert(name.to_string(), (scope, all || !maybe.is_empty()));
    }
    Ok(ServiceScopes { by_name, required })
}

/// Find a cycle among the Service dependencies of an Environment Template's
/// Services, which have no Steps to depend on (§1.2 *services* constraint
/// 4): their own `service` entries, and an edge from each Service whose
/// Sessions are known to enter the document's `environment` to each
/// Service it lists. There is no exception for an Environment that lists
/// the Service. A Service here may have only disabled `environment:`
/// entries (constraint 5), so its Sessions enter the Environment by its
/// `activation` unless such an entry declines it; one whose own `enableIf`
/// is not known (§7.6 rule 5, in the document's own context), or that has
/// an `exclusiveGroup`, is decided at job creation, and adds no edge here.
#[must_use]
pub fn environment_template_service_cycle(
    et: &EnvironmentTemplate,
) -> Option<ServiceDependencyCycle> {
    let ctx = et.default_validation_context();
    let services = et.services();
    let mut graph = DependencyGraph::default();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for svc in services {
        let i = graph.node(format!("Service {}", svc.name));
        index.entry(svc.name.as_str()).or_insert(i);
    }
    for (i, svc) in services.iter().enumerate() {
        for to in static_edges(svc.dependencies.as_deref(), i, &HashMap::new(), &index) {
            graph.edge(i, to, None);
        }
    }
    // An Environment in an `exclusiveGroup` is never known to be entered
    // here: an Environment Template attached before this one may hold an
    // earlier member of its group and supersede it (as in
    // [`Entrances::of`]), so its edges are decided at job creation.
    if let Some(env) = et.environment().filter(|env| env.group().is_none()) {
        let listed: Vec<usize> = env
            .listed_services()
            .filter_map(|n| index.get(n).copied())
            .collect();
        let enabled =
            crate::template::validate_v2023_09::format_strings::known_environment_template_enable_if(
                et, &ctx,
            );
        let lists_env_params = et
            .profile()
            .has_extension(crate::types::ModelExtension::EnvParams);
        for (i, svc) in services.iter().enumerate() {
            // Only a disabled entry declines: any other is a constraint 5
            // error of its own and is read as absent, so the Service still
            // enters by `activation`, and a listing Environment's cycle is
            // reported beside that error.
            let declines = svc.dependencies.iter().flatten().any(|d| {
                d.environment() == Some(env.name.as_str()) && d.literal_enabled() == Some(false)
            });
            let entry = if declines {
                KnownEntry::Disabled
            } else {
                KnownEntry::Absent
            };
            if env.known_state_in(lists_env_params, enabled, entry) != Known::Entered {
                continue;
            }
            for &to in &listed {
                graph.edge(i, to, Some(format!("Environment '{}'", env.name)));
            }
        }
    }
    graph.find_cycle()
}

/// The Services of `services` that `dependencies` lists with the `service`
/// key — those whose `Service.<name>.<port>.*` the listing Step, Service, or
/// Environment may reference (§9 scope rules 2–5). Declaration order; a
/// name the list does not declare (a required Service, or a typo) yields
/// nothing here.
pub fn listed_services<'a>(
    dependencies: Option<&'a [StepDependency]>,
    services: &'a [Service],
) -> impl Iterator<Item = &'a Service> + Clone + 'a {
    services
        .iter()
        .filter(move |svc| super::step::lists_service(dependencies, &svc.name))
}

/// The `externalServices` entries of `requirements` that `dependencies` lists
/// with the `service` key — the required Services whose
/// `Service.<name>.<port>.port` / `.connectAddress` the listing Step,
/// Service, or Job Environment may reference (§9 scope rules 2–4, §9.8 item
/// 2). Declaration
/// order; a name no requirement declares (an inline Service, or a typo)
/// yields nothing here. Listing a required Service makes its values visible
/// and puts the lister in the matched Service's scope, as for an inline
/// Service ([`ServiceScopes::required_service_scopes`]).
pub fn listed_requirements<'a>(
    dependencies: Option<&'a [StepDependency]>,
    requirements: &'a [ServiceRequirement],
) -> impl Iterator<Item = &'a ServiceRequirement> + Clone + 'a {
    requirements
        .iter()
        .filter(move |req| super::step::lists_service(dependencies, &req.name))
}

// ── Reference extraction ─────────────────────────────────────────────

/// The Service names a Step references from its `script` (actions,
/// embedded files, script `let`) and `stepEnvironments`. A reference is not
/// a dependency: the Step must list each with the `service` key (§9 scope
/// rule 3), and this is how the diagnostic finds the ones it does not.
#[must_use]
pub fn step_references(step: &StepTemplate) -> BTreeSet<String> {
    let mut symbols = HashSet::new();
    let script = step.resolve_syntax_sugar();
    if let Some(script) = &script {
        collect_action(&script.actions.on_run, &mut symbols);
        collect_files(script.embedded_files.as_deref(), &mut symbols);
        collect_lets(script.let_bindings.as_deref(), &mut symbols);
    }
    for env in step.step_environments.iter().flatten() {
        collect_environment(env, &mut symbols);
    }
    service_names(&symbols)
}

/// The Service names an Environment references from its `variables`,
/// actions, embedded files and script `let`. A reference is not a
/// dependency: a Job Environment must list each with the `service` key (§9
/// scope rule 4), and this is how the diagnostic finds the ones it does
/// not.
#[must_use]
pub fn environment_references(env: &Environment) -> BTreeSet<String> {
    let mut symbols = HashSet::new();
    collect_environment(env, &mut symbols);
    service_names(&symbols)
}

/// True when any format string of `env` references a `Service.*` value.
#[must_use]
pub fn environment_references_service(env: &Environment) -> bool {
    let mut symbols = HashSet::new();
    collect_environment(env, &mut symbols);
    symbols.iter().any(|s| s.starts_with("Service."))
}

/// The Service names a Service references from its `variables`, actions,
/// embedded files and `<ServiceScript>.let`, including its own name when it
/// references itself; `Service.File.*` is not a Service. A reference is not
/// a dependency (§9 scope rule 2): the Service must list each other Service
/// with the `service` key.
#[must_use]
pub fn service_references(svc: &Service) -> BTreeSet<String> {
    let mut symbols = HashSet::new();
    for fs in svc.variables.iter().flat_map(|v| v.values()) {
        symbols.extend(fs.accessed_symbols());
    }
    for action in svc.script.actions.iter_actions() {
        collect_action(action, &mut symbols);
    }
    collect_files(svc.script.embedded_files.as_deref(), &mut symbols);
    collect_lets(svc.script.let_bindings.as_deref(), &mut symbols);
    service_names(&symbols)
}

fn collect_environment(env: &Environment, out: &mut HashSet<String>) {
    for fs in env.variables.iter().flat_map(|v| v.values()) {
        out.extend(fs.accessed_symbols());
    }
    if let Some(script) = &env.script {
        for action in script.actions.iter_actions() {
            collect_action(action, out);
        }
        collect_files(script.embedded_files.as_deref(), out);
        collect_lets(script.let_bindings.as_deref(), out);
    }
}

fn collect_action(action: &Action, out: &mut HashSet<String>) {
    out.extend(action.command.accessed_symbols());
    for arg in action.args.iter().flatten() {
        out.extend(arg.accessed_symbols());
    }
    if let Some(timeout) = &action.timeout {
        out.extend(timeout.accessed_symbols());
    }
    match &action.cancelation {
        Some(CancelationMode::NotifyThenTerminate {
            notify_period_in_seconds: Some(n),
        }) => out.extend(n.accessed_symbols()),
        Some(CancelationMode::DeferredMode {
            mode,
            notify_period_in_seconds,
        }) => {
            out.extend(mode.accessed_symbols());
            if let Some(n) = notify_period_in_seconds {
                out.extend(n.accessed_symbols());
            }
        }
        _ => {}
    }
}

fn collect_files(files: Option<&[EmbeddedFile]>, out: &mut HashSet<String>) {
    for file in files.into_iter().flatten() {
        if let Some(data) = &file.data {
            out.extend(data.accessed_symbols());
        }
        if let Some(filename) = &file.filename {
            if let Ok(fs) = FormatString::new(filename) {
                out.extend(fs.accessed_symbols());
            }
        }
    }
}

fn collect_lets(bindings: Option<&[String]>, out: &mut HashSet<String>) {
    for binding in bindings.into_iter().flatten() {
        if let Some(eq) = binding.find('=') {
            let expr = binding[eq + 1..].trim();
            if let Ok(parsed) = openjd_expr::eval::ParsedExpression::new(expr) {
                out.extend(parsed.accessed_symbols().iter().cloned());
            }
        }
    }
}

/// The `<name>` of every `Service.<name>.…` symbol in `symbols`, excluding
/// `Service.File.*`.
fn service_names(symbols: &HashSet<String>) -> BTreeSet<String> {
    symbols
        .iter()
        .filter_map(|s| s.strip_prefix("Service."))
        .filter_map(|rest| rest.split('.').next())
        .filter(|name| *name != "File" && !name.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(yaml: &str) -> JobTemplate {
        serde_saphyr::from_str(yaml).unwrap()
    }

    /// `service:X` in a test's dependency list stands for `{service: X}`,
    /// `env:X` for `{environment: X}`, `off:X` for `{environment: X,
    /// enableIf: false}` (which declines a `BY_DEFAULT` X); anything else
    /// for `{dependsOn: X}`.
    fn dep_entry(d: &str) -> String {
        if let Some(env) = d.strip_prefix("env:") {
            return format!("{{environment: {env}}}");
        }
        if let Some(env) = d.strip_prefix("off:") {
            return format!("{{environment: {env}, enableIf: false}}");
        }
        match d.strip_prefix("service:") {
            Some(svc) => format!("{{service: {svc}}}"),
            None => format!("{{dependsOn: {d}}}"),
        }
    }

    fn svc(name: &str, args: &[&str], deps: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
        let deps = if deps.is_empty() {
            String::new()
        } else {
            let deps: Vec<String> = deps.iter().map(|d| dep_entry(d)).collect();
            format!("  dependencies: [{}]\n", deps.join(", "))
        };
        format!(
            "- name: {name}\n{deps}  ports: [{{name: main}}]\n  script:\n    actions:\n      onRun: {{command: x, args: [{}]}}\n",
            args.join(", ")
        )
    }

    fn step(name: &str, args: &[&str], deps: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
        let deps = if deps.is_empty() {
            String::new()
        } else {
            let deps: Vec<String> = deps.iter().map(|d| dep_entry(d)).collect();
            format!("  dependencies: [{}]\n", deps.join(", "))
        };
        format!(
            "- name: {name}\n{deps}  script:\n    actions:\n      onRun: {{command: x, args: [{}]}}\n",
            args.join(", ")
        )
    }

    #[test]
    fn display_forms() {
        assert_eq!(ServiceScope::AllSteps.to_string(), "every Step");
        assert_eq!(ServiceScope::steps(["B"]).to_string(), "Step B");
        assert_eq!(ServiceScope::steps(["B", "A"]).to_string(), "Steps A, B");
        assert_eq!(
            ServiceScope::steps::<[&str; 0], &str>([]).to_string(),
            "no Step"
        );
        assert!(ServiceScope::AllSteps.contains("anything"));
        assert!(ServiceScope::steps(["A"]).contains("A"));
        assert!(!ServiceScope::steps(["A"]).contains("B"));
        assert!(ServiceScope::AllSteps.step_names().is_none());
        assert_eq!(ServiceScope::steps(["A"]).step_names().unwrap().len(), 1);
    }

    #[test]
    fn scope_serializes_with_a_kind_tag() {
        assert_eq!(
            serde_json::to_string(&ServiceScope::AllSteps).unwrap(),
            r#"{"kind":"allSteps"}"#
        );
        assert_eq!(
            serde_json::to_string(&ServiceScope::steps(["B", "A"])).unwrap(),
            r#"{"kind":"steps","steps":["A","B"]}"#
        );
        let back: ServiceScope = serde_json::from_str(r#"{"kind":"steps","steps":["A"]}"#).unwrap();
        assert_eq!(back, ServiceScope::steps(["A"]));
    }

    #[test]
    fn four_rules_and_transitivity() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  dependencies: [{{service: Wide}}]\n  \
             variables: {{ADDR: \"{{{{ Service.Wide.main.connectAddress }}}}\"}}\n\
             services:\n{}{}{}{}steps:\n{}{}{}",
            // Wide declines E, which lists it: there is no exception for it
            // (RFC 0009 "Services run inside Environments").
            svc("Wide", &[], &["off:E"]),
            svc(
                "Front",
                &["{{ Service.Back.main.port }}"],
                &["service:Back"]
            ),
            svc("Back", &[], &[]),
            svc("Lonely", &[], &[]),
            step("S1", &["{{ Service.Front.main.port }}"], &["service:Front"]),
            step(
                "S2",
                &["{{ Service.Back.main.port }}"],
                &["S1", "service:Back"]
            ),
            step("S3", &[], &["S2"]),
        );
        let jt = template(&yaml);
        let scopes = compute_service_scopes(&jt).unwrap();
        // Rule 3.
        assert_eq!(scopes.scope_of("Wide"), ServiceScope::AllSteps);
        assert!(scopes.get("Wide").unwrap().listed_by_job_environment);
        assert!(!scopes.get("Wide").unwrap().is_unused());
        // Rule 4: nothing lists Lonely.
        let lonely = scopes.get("Lonely").unwrap();
        assert_eq!(lonely.scope, ServiceScope::steps::<[&str; 0], &str>([]));
        assert!(lonely.is_unused());
        // Rule 1.
        assert_eq!(scopes.scope_of("Front"), ServiceScope::steps(["S1"]));
        // Rule 2: Back has S2 directly and S1 through Front.
        assert_eq!(scopes.scope_of("Back"), ServiceScope::steps(["S1", "S2"]));
        let front = scopes.get("Front").unwrap();
        assert_eq!(
            front.depends_on_services.iter().collect::<Vec<_>>(),
            vec![&"Back".to_string()]
        );
        assert_eq!(front.dependent_steps, vec!["S1".to_string()]);
        let back = scopes.get("Back").unwrap();
        assert!(back.depends_on_services.is_empty());
        assert_eq!(back.dependent_steps, vec!["S2".to_string()]);
        assert_eq!(back.dependent_services, vec!["Front".to_string()]);
        // An undeclared name has no scope here.
        assert_eq!(
            scopes.scope_of("Ext"),
            ServiceScope::steps::<[&str; 0], &str>([])
        );
        assert_eq!(scopes.iter().count(), 4);
    }

    /// `Front` lists `Back`, and a Job Environment `E` with the default
    /// `activation` `BY_DEFAULT` (or `ON_REQUEST`, requested by both Steps)
    /// lists `Front`.
    fn job_wide_dependent(on_request: bool) -> String {
        let (activation, deps): (&str, &[&str]) = if on_request {
            ("  activation: ON_REQUEST\n", &["env:E"])
        } else {
            ("", &[])
        };
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n{activation}  dependencies: [{{service: Front}}]\n  \
             variables: {{ADDR: \"{{{{ Service.Front.main.connectAddress }}}}\"}}\n\
             services:\n{}{}steps:\n{}{}",
            svc(
                "Front",
                &["{{ Service.Back.main.port }}"],
                &["service:Back"]
            ),
            svc("Back", &[], &[]),
            step("S1", &[], deps),
            step("S2", &[], deps),
        )
    }

    #[test]
    fn a_service_entering_a_by_default_client_environment_of_its_dependent_is_a_cycle() {
        // RFC 0009 `<StepDependency>` constraint 3: Back's Sessions enter the
        // BY_DEFAULT Environment E, which lists Front, so Back depends on Front,
        // which lists Back.
        let cycle = compute_service_scopes(&template(&job_wide_dependent(false))).unwrap_err();
        assert_eq!(
            cycle.to_string(),
            "dependencies contain a cycle: Service Front -> Service Back -> Service Front (the \
             Sessions of Service Back enter Job Environment 'E', which lists Service 'Front', so \
             Service Back depends on it; Template Schemas §3.2 constraint 3)."
        );
    }

    #[test]
    fn a_job_wide_dependent_makes_the_depended_on_service_job_wide() {
        // The remedy: E is ON_REQUEST and every Step requests it, so no
        // Service's Sessions enter it, and both Services serve every Step.
        let scopes = compute_service_scopes(&template(&job_wide_dependent(true))).unwrap();
        assert_eq!(scopes.scope_of("Front"), ServiceScope::AllSteps);
        assert_eq!(scopes.scope_of("Back"), ServiceScope::AllSteps);
    }

    #[test]
    fn a_job_environment_reference_without_a_dependency_is_not_rule_3() {
        // Listing without referencing puts every Step in scope; referencing
        // without listing (a validation error) is not an edge, and a
        // required Service listed by a Job Environment is not one either.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             externalServices: [{{name: Ext, ports: [{{name: main}}]}}]\n\
             jobEnvironments:\n\
             - name: Opaque\n  dependencies: [{{service: Listed}}, {{service: Ext}}]\n  \
             variables: {{A: x}}\n\
             - name: Stray\n  variables: {{ADDR: \"{{{{ Service.Referenced.main.connectAddress }}}}\"}}\n\
             services:\n{}{}steps:\n{}",
            svc("Listed", &[], &["off:Opaque"]),
            svc("Referenced", &[], &[]),
            step("S1", &[], &[]),
        );
        let scopes = compute_service_scopes(&template(&yaml)).unwrap();
        let listed = scopes.get("Listed").unwrap();
        assert_eq!(listed.scope, ServiceScope::AllSteps);
        assert!(listed.listed_by_job_environment);
        let referenced = scopes.get("Referenced").unwrap();
        assert!(referenced.is_unused());
        assert!(!referenced.listed_by_job_environment);
        assert!(scopes.get("Ext").is_none());
    }

    #[test]
    fn references_are_not_edges_and_requirement_names_are_not_dependencies() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             externalServices: [{{name: Ext, ports: [{{name: main}}]}}]\n\
             services:\n{}{}steps:\n{}{}",
            svc("Own", &["{{ Service.Ext.main.port }}"], &["service:Ext"]),
            svc("Other", &["{{ Service.Own.main.port }}"], &[]),
            step("S1", &["{{ Service.Ext.main.port }}"], &["service:Own"]),
            step("S2", &["{{ Service.Other.main.port }}"], &[]),
        );
        let scopes = compute_service_scopes(&template(&yaml)).unwrap();
        let own = scopes.get("Own").unwrap();
        assert!(own.depends_on_services.is_empty());
        assert_eq!(own.scope, ServiceScope::steps(["S1"]));
        // Other references Own and S2 references Other: neither is an edge.
        assert!(scopes.get("Other").unwrap().is_unused());
        assert!(scopes.get("Ext").is_none());
    }

    #[test]
    fn service_step_dependencies_are_recorded() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             services:\n{}steps:\n{}{}",
            svc("Indexer", &[], &["Prepare", "service:Nope"]),
            step("Prepare", &[], &[]),
            step("Use", &[], &["Prepare", "service:Indexer"]),
        );
        let scopes = compute_service_scopes(&template(&yaml)).unwrap();
        let indexer = scopes.get("Indexer").unwrap();
        assert_eq!(indexer.depends_on_steps, vec!["Prepare".to_string()]);
        // An unknown Service name is not an edge.
        assert!(indexer.depends_on_services.is_empty());
        assert_eq!(indexer.scope, ServiceScope::steps(["Use"]));
    }

    #[test]
    fn cycles_are_reported_with_their_path() {
        // Service -> Service -> Service.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             services:\n{}{}{}steps:\n{}",
            svc("A", &[], &["service:B"]),
            svc("B", &[], &["service:C"]),
            svc("C", &[], &["service:A"]),
            step("S1", &[], &["service:A"]),
        );
        let jt = template(&yaml);
        let cycle = compute_service_scopes(&jt).unwrap_err();
        assert_eq!(
            cycle.path,
            vec!["Service A", "Service B", "Service C", "Service A"]
        );
        assert_eq!(cycle.via_environment, None);
        assert_eq!(
            cycle.to_string(),
            "dependencies contain a cycle: Service A -> Service B -> Service C -> Service A."
        );
        // Step -> Service -> Step.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             services:\n{}steps:\n{}",
            svc("X", &[], &["Use"]),
            step("Use", &[], &["service:X"]),
        );
        let cycle = compute_service_scopes(&template(&yaml)).unwrap_err();
        assert_eq!(cycle.path, vec!["Step Use", "Service X", "Step Use"]);
        // Step -> Step.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             steps:\n{}{}",
            step("A", &[], &["B"]),
            step("B", &[], &["A"]),
        );
        let cycle = compute_service_scopes(&template(&yaml)).unwrap_err();
        assert_eq!(
            cycle.to_string(),
            "dependencies contain a cycle: Step A -> Step B -> Step A."
        );
        // Self-dependencies are not edges here (validation reports them).
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             services:\n{}steps:\n{}",
            svc("A", &[], &["service:A"]),
            step("S1", &[], &["service:A"]),
        );
        let jt = template(&yaml);
        assert!(compute_service_scopes(&jt).is_ok());
    }

    #[test]
    fn a_job_environment_closes_a_cycle_through_its_scope() {
        // Job Environment E lists X; X depends on Step Prepare. Every Step
        // depends on X through E, so Prepare -> X -> Prepare is a cycle.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  dependencies: [{{service: X}}]\n  \
             variables: {{ADDR: \"{{{{ Service.X.main.connectAddress }}}}\"}}\n\
             services:\n{}steps:\n{}{}",
            svc("X", &[], &["Prepare"]),
            step("Prepare", &[], &[]),
            step("Use", &[], &[]),
        );
        let cycle = compute_service_scopes(&template(&yaml)).unwrap_err();
        assert_eq!(
            cycle.path,
            vec!["Step Prepare", "Service X", "Step Prepare"]
        );
        assert_eq!(
            cycle.via_environment,
            Some(CycleViaEnvironment {
                from: "Step Prepare".to_string(),
                environment: "Job Environment 'E'".to_string(),
                service: "X".to_string(),
            })
        );
        assert_eq!(
            cycle.to_string(),
            "dependencies contain a cycle: Step Prepare -> Service X -> Step Prepare (the \
             Sessions of Step Prepare enter Job Environment 'E', which lists Service 'X', so \
             Step Prepare depends on it; Template Schemas §3.2 constraint 3)."
        );
        // The transitive variant: E lists Front, Front lists X, X depends
        // on Prepare.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  dependencies: [{{service: Front}}]\n  \
             variables: {{ADDR: \"{{{{ Service.Front.main.connectAddress }}}}\"}}\n\
             services:\n{}{}steps:\n{}{}",
            // Both decline E, which lists Front, so the cycle through
            // Prepare's Sessions is the only one.
            svc("Front", &["{{ Service.X.main.port }}"], &["service:X", "off:E"]),
            svc("X", &[], &["Prepare", "off:E"]),
            step("Prepare", &[], &[]),
            step("Use", &[], &[]),
        );
        let cycle = compute_service_scopes(&template(&yaml)).unwrap_err();
        assert_eq!(
            cycle.path,
            vec!["Step Prepare", "Service Front", "Service X", "Step Prepare"]
        );
        assert_eq!(
            cycle.via_environment.map(|v| (v.from, v.service)),
            Some(("Step Prepare".to_string(), "Front".to_string()))
        );
        // A Job Environment that lists a Service no Service depends on a
        // Step through is not a cycle, and the scope edges are not counted
        // as Steps that list the Service.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  dependencies: [{{service: X}}]\n  \
             variables: {{ADDR: \"{{{{ Service.X.main.connectAddress }}}}\"}}\n\
             services:\n{}steps:\n{}{}",
            svc("X", &[], &["off:E"]),
            step("Prepare", &[], &[]),
            step("Use", &[], &["Prepare"]),
        );
        let scopes = compute_service_scopes(&template(&yaml)).unwrap();
        let x = scopes.get("X").unwrap();
        assert_eq!(x.scope, ServiceScope::AllSteps);
        assert!(x.dependent_steps.is_empty());
        assert!(x.listed_by_job_environment);
    }

    #[test]
    fn listed_services_follow_the_dependencies_list() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             services:\n{}{}{}steps:\n{}",
            svc("A", &[], &[]),
            svc("B", &[], &[]),
            svc("C", &[], &[]),
            step("S1", &[], &["service:C", "service:A", "service:Nope"]),
        );
        let jt = template(&yaml);
        let names: Vec<&str> = listed_services(jt.steps[0].dependencies.as_deref(), jt.services())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["A", "C"]);
        assert_eq!(listed_services(None, jt.services()).count(), 0);
    }

    #[test]
    fn listed_requirements_follow_the_dependencies_list() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             externalServices:\n- name: R1\n  ports: [{{name: main}}]\n- name: R2\n  \
             ports: [{{name: main}}]\n- name: R3\n  ports: [{{name: main}}]\nservices:\n{}steps:\n{}",
            svc("A", &[], &[]),
            step("S1", &[], &["service:R3", "service:A", "service:R1", "service:Nope"]),
        );
        let jt = template(&yaml);
        let names: Vec<&str> =
            listed_requirements(jt.steps[0].dependencies.as_deref(), jt.external_services())
                .map(|r| r.name.as_str())
                .collect();
        assert_eq!(names, vec!["R1", "R3"]);
        assert_eq!(listed_requirements(None, jt.external_services()).count(), 0);
    }

    #[test]
    fn environment_reference_detection() {
        let env: Environment = serde_saphyr::from_str(
            "name: E\nscript:\n  actions:\n    onEnter: {command: x, args: [\"{{ Service.C.p.port }}\"]}\n",
        )
        .unwrap();
        assert!(environment_references_service(&env));
        assert_eq!(
            environment_references(&env).into_iter().collect::<Vec<_>>(),
            vec!["C".to_string()]
        );
        let plain: Environment =
            serde_saphyr::from_str("name: E\nvariables: {A: \"{{ Param.X }}\"}\n").unwrap();
        assert!(!environment_references_service(&plain));
        // Service.File.* in a let does not name a Service.
        let lets: Environment = serde_saphyr::from_str(
            "name: E\nscript:\n  let: [\"p = Service.D.main.port + 1\"]\n  actions:\n    onEnter: {command: x}\n",
        )
        .unwrap();
        assert_eq!(
            environment_references(&lets)
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["D".to_string()]
        );
    }

    #[test]
    fn step_references_cover_sugar_and_step_environments() {
        let step: StepTemplate = serde_saphyr::from_str(
            "name: S\nbash:\n  script: \"echo {{ Service.A.p.port }}\"\nstepEnvironments:\n- name: E\n  variables: {X: \"{{ Service.B.p.connectAddress }}\"}\n",
        )
        .unwrap();
        assert_eq!(
            step_references(&step).into_iter().collect::<Vec<_>>(),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    /// A reverse-declared `dependsOn` chain of 6,000 Steps has recursion
    /// depth 6,000 in a recursive DFS, which overflows a test thread's
    /// stack. The cycle check must not recurse per edge (Q6).
    #[test]
    fn cycle_check_handles_a_deep_dependency_chain() {
        const N: usize = 6_000;
        let mut yaml = String::from(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nname: J\n\
             services:\n",
        );
        yaml.push_str(&svc("Svc", &[], &[]));
        yaml.push_str("steps:\n");
        for i in 0..N {
            let deps: Vec<String> = if i + 1 < N {
                vec![format!("S{}", i + 1)]
            } else {
                vec!["service:Svc".to_string()]
            };
            let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
            yaml.push_str(&step(&format!("S{i}"), &[], &deps));
        }
        let jt = template(&yaml);
        let scopes = compute_service_scopes(&jt).unwrap();
        assert_eq!(
            scopes.scope_of("Svc"),
            ServiceScope::steps([format!("S{}", N - 1)])
        );
    }
}
