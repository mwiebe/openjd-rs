//! Service scope (Template Schemas §9.1, RFC 0009 "Service scope").
//!
//! The scope of a Service declared in a Job Template's `services` is the set
//! of Steps whose Tasks depend on it, computed from the template's
//! `dependencies` lists:
//!
//! 1. a Step that lists `service:X` in its `dependencies` is in `X`'s scope;
//! 2. when Service `Y` lists `service:X`, every Step in `Y`'s scope is in
//!    `X`'s scope, transitively through any chain of Services;
//! 3. when any `jobEnvironments` entry lists `service:X` in its
//!    `dependencies`, every Step is in `X`'s scope — a Job Environment is
//!    entered by every Step's Session, so a Service it depends on is one
//!    every Step depends on;
//! 4. a Service in whose scope no Step falls — one that no Step, Service, or
//!    Job Environment lists — is unused, and the template is rejected naming
//!    it ([`ComputedServiceScope::is_unused`]).
//!
//! The `dependencies` of a template's Steps and Services together form one
//! graph — Step-to-Step, Step-to-Service, Service-to-Step and
//! Service-to-Service edges — which must be acyclic (§3.2 constraint 3, §9.9
//! item 10). A Job Environment's entries add Environment-to-Service edges
//! that, since nothing depends on an Environment, can never close a cycle,
//! so they take no part in cycle detection. [`compute_service_scopes`]
//! applies the rules to a decoded Job Template, reporting the first cycle
//! found as a [`ServiceDependencyCycle`]; [`service_dependency_cycle`] finds
//! a cycle among the `service:` dependencies of an Environment Template's
//! Services, which have no Steps to depend on.
//!
//! `Service.*` values are available exactly to the entities that list the
//! Service — a Step, a Service, or a Job Environment — so a reference is
//! never an implicit edge, and never an implicit dependency. The reference
//! extraction helpers at the end of this module ([`step_references`],
//! [`environment_references`], [`service_references`]) remain for the two
//! places a reference does matter: the diagnostic that names the missing
//! `service:<name>` entry when an entity references a Service it does not
//! list, and the `runScope` default of a Step Environment (§4 item 4),
//! which has no list of its own. The scheduler side of the rules (start
//! before any Task of a Step in scope, stop once none remains) is the
//! runtime's.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::actions::{Action, CancelationMode};
use super::environment::{EmbeddedFile, Environment};
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
    /// Every Step of the Job: a Service a Job Environment lists in its
    /// `dependencies`, one a Job-wide Service depends on, or an external
    /// Service. The Service lives
    /// as long as the Job.
    AllSteps,
    /// Exactly these Steps, by name. The Service is started before the first
    /// Task of any of them and stopped once none has a Task left to run. An
    /// empty set is an unused Service, which validation rejects.
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

/// What the scope rules found for one inline Service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedServiceScope {
    /// The Service's `name`.
    pub name: String,
    /// The Steps whose Tasks depend on the Service.
    pub scope: ServiceScope,
    /// The other inline Services this Service lists as `service:<name>`
    /// (§9 item 4): it starts after each is READY and stops before any of
    /// them. Sorted; never contains the Service itself, a required external
    /// Service, or a name the template does not declare.
    pub depends_on_services: BTreeSet<String>,
    /// The Steps this Service lists in its `dependencies` (§9 item 4): it
    /// starts only after each has completed. In list order; names the
    /// template does not declare are kept (validation reports them).
    pub depends_on_steps: Vec<String>,
    /// The Steps that list `service:<name>` directly (rule 1), in template
    /// order.
    pub dependent_steps: Vec<String>,
    /// The Services that list `service:<name>` (rule 2), in template order.
    pub dependent_services: Vec<String>,
    /// True when a `jobEnvironments` entry lists `service:<name>` in its
    /// `dependencies` (rule 3): every Step is in the scope.
    pub listed_by_job_environment: bool,
}

impl ComputedServiceScope {
    /// True when no Step is in the scope (rule 4): the template must be
    /// rejected, naming the Service.
    #[must_use]
    pub fn is_unused(&self) -> bool {
        matches!(&self.scope, ServiceScope::Steps { steps } if steps.is_empty())
    }
}

/// The scopes of every Service of a Job Template, by name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceScopes {
    by_name: BTreeMap<String, ComputedServiceScope>,
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

    /// The scope of `name`, or `AllSteps` for a name the template does not
    /// declare (an external Service).
    #[must_use]
    pub fn scope_of(&self, name: &str) -> ServiceScope {
        self.by_name
            .get(name)
            .map_or(ServiceScope::AllSteps, |c| c.scope.clone())
    }
}

/// A cycle in the graph the `dependencies` of a template's Steps and
/// Services form (§3.2 constraint 3, §9.9 item 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDependencyCycle {
    /// The nodes along the cycle as they are written in a `dependsOn`
    /// value — a Step name, or `service:<name>` — starting and ending with
    /// the same node: `["Use", "service:Indexer", "Use"]`.
    pub path: Vec<String>,
}

impl fmt::Display for ServiceDependencyCycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "dependencies contain a cycle: {}.",
            self.path.join(" -> ")
        )
    }
}

/// A node of the combined dependency graph, keyed as `dependsOn` writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Node<'a> {
    Step(&'a str),
    Service(&'a str),
}

impl fmt::Display for Node<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Step(name) => f.write_str(name),
            Self::Service(name) => write!(f, "{}{name}", super::step::SERVICE_DEPENDENCY_PREFIX),
        }
    }
}

/// The out-edges of `deps` among the declared `steps` and `services`;
/// unknown names and the entity itself are not edges (validation reports
/// them separately).
fn edges_of<'a>(
    deps: Option<&'a [StepDependency]>,
    this: Node<'a>,
    steps: &HashSet<&'a str>,
    services: &HashSet<&'a str>,
) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    for dep in deps.into_iter().flatten() {
        let node = match dep.target(true) {
            super::step::DependencyTarget::Step(name) => steps.get(name).map(|n| Node::Step(n)),
            super::step::DependencyTarget::Service(name) => {
                services.get(name).map(|n| Node::Service(n))
            }
        };
        if let Some(node) = node {
            if node != this && !out.contains(&node) {
                out.push(node);
            }
        }
    }
    out
}

/// Compute the scope of every Service of `jt` (§9.1), or report the first
/// cycle in the combined dependency graph of its Steps and Services.
///
/// The names in `jt.requires_services` are external Services: a
/// `service:<name>` entry naming one is not an edge of the graph and does
/// not place anything in an inline Service's scope, whether a Step, a
/// Service, or a Job Environment lists it.
pub fn compute_service_scopes(jt: &JobTemplate) -> Result<ServiceScopes, ServiceDependencyCycle> {
    let services = jt.services();
    let step_names: HashSet<&str> = jt.steps.iter().map(|s| s.name.as_str()).collect();
    let service_names: HashSet<&str> = services.iter().map(|s| s.name.as_str()).collect();

    // The combined graph, in template order (Steps, then Services).
    let mut graph: Vec<(Node<'_>, Vec<Node<'_>>)> = Vec::new();
    for step in &jt.steps {
        let node = Node::Step(&step.name);
        graph.push((
            node,
            edges_of(
                step.dependencies.as_deref(),
                node,
                &step_names,
                &service_names,
            ),
        ));
    }
    for svc in services {
        let node = Node::Service(&svc.name);
        graph.push((
            node,
            edges_of(
                svc.dependencies.as_deref(),
                node,
                &step_names,
                &service_names,
            ),
        ));
    }
    if let Some(cycle) = find_cycle(&graph) {
        return Err(cycle);
    }
    if services.is_empty() {
        return Ok(ServiceScopes::default());
    }

    // Reverse edges into each Service: the Steps (rule 1) and Services
    // (rule 2) that list it.
    let mut dependent_steps: HashMap<&str, Vec<String>> = HashMap::new();
    let mut dependent_services: HashMap<&str, Vec<&str>> = HashMap::new();
    for (node, edges) in &graph {
        for edge in edges {
            let Node::Service(target) = edge else {
                continue;
            };
            match node {
                Node::Step(step) => dependent_steps
                    .entry(target)
                    .or_default()
                    .push(step.to_string()),
                Node::Service(svc) => dependent_services.entry(target).or_default().push(svc),
            }
        }
    }
    // Rule 3: the Services the Job Environments list.
    let mut job_wide: HashSet<&str> = HashSet::new();
    for env in jt.job_environments.iter().flatten() {
        for name in env.listed_services() {
            if let Some(key) = service_names.get(name) {
                job_wide.insert(key);
            }
        }
    }

    // Rules 1–3 with the transitive closure of rule 2, memoized over the
    // (acyclic) dependent-services graph.
    let mut memo: HashMap<&str, ServiceScope> = HashMap::new();
    fn scope_of<'a>(
        name: &'a str,
        job_wide: &HashSet<&str>,
        dependent_steps: &HashMap<&str, Vec<String>>,
        dependent_services: &HashMap<&str, Vec<&'a str>>,
        memo: &mut HashMap<&'a str, ServiceScope>,
    ) -> ServiceScope {
        if let Some(s) = memo.get(name) {
            return s.clone();
        }
        let scope = if job_wide.contains(name) {
            ServiceScope::AllSteps
        } else {
            let mut steps: BTreeSet<String> = dependent_steps
                .get(name)
                .map(|v| v.iter().cloned().collect())
                .unwrap_or_default();
            let mut all = false;
            for dependent in dependent_services.get(name).into_iter().flatten() {
                match scope_of(
                    dependent,
                    job_wide,
                    dependent_steps,
                    dependent_services,
                    memo,
                ) {
                    ServiceScope::AllSteps => {
                        all = true;
                        break;
                    }
                    ServiceScope::Steps { steps: s } => steps.extend(s),
                }
            }
            if all {
                ServiceScope::AllSteps
            } else {
                ServiceScope::Steps { steps }
            }
        };
        memo.insert(name, scope.clone());
        scope
    }

    let mut by_name = BTreeMap::new();
    for svc in services {
        let name = svc.name.as_str();
        let scope = scope_of(
            name,
            &job_wide,
            &dependent_steps,
            &dependent_services,
            &mut memo,
        );
        let depends_on_services = listed_service_names(svc.dependencies.as_deref())
            .filter(|n| *n != name && service_names.contains(n))
            .map(str::to_string)
            .collect();
        let depends_on_steps = listed_step_names(svc.dependencies.as_deref(), true)
            .map(str::to_string)
            .collect();
        by_name.insert(
            svc.name.clone(),
            ComputedServiceScope {
                name: svc.name.clone(),
                scope,
                depends_on_services,
                depends_on_steps,
                dependent_steps: dependent_steps.get(name).cloned().unwrap_or_default(),
                dependent_services: dependent_services
                    .get(name)
                    .map(|v| v.iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default(),
                listed_by_job_environment: job_wide.contains(name),
            },
        );
    }
    Ok(ServiceScopes { by_name })
}

/// Find a cycle among the `service:` dependencies of `services` (an
/// Environment Template's list, which has no Steps), or `None` when the
/// graph is acyclic.
pub fn service_dependency_cycle(services: &[Service]) -> Option<ServiceDependencyCycle> {
    let service_names: HashSet<&str> = services.iter().map(|s| s.name.as_str()).collect();
    let graph: Vec<(Node<'_>, Vec<Node<'_>>)> = services
        .iter()
        .map(|svc| {
            let node = Node::Service(&svc.name);
            (
                node,
                edges_of(
                    svc.dependencies.as_deref(),
                    node,
                    &HashSet::new(),
                    &service_names,
                ),
            )
        })
        .collect();
    find_cycle(&graph)
}

/// DFS over `graph` in declaration order; the first back edge found yields
/// the cycle, spelled from the revisited node back to itself.
fn find_cycle<'a>(graph: &[(Node<'a>, Vec<Node<'a>>)]) -> Option<ServiceDependencyCycle> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    let index: HashMap<Node<'a>, usize> = graph
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (*n, i))
        .collect();
    fn visit<'a>(
        at: usize,
        graph: &[(Node<'a>, Vec<Node<'a>>)],
        index: &HashMap<Node<'a>, usize>,
        marks: &mut [Mark],
        stack: &mut Vec<usize>,
    ) -> Option<ServiceDependencyCycle> {
        marks[at] = Mark::Open;
        stack.push(at);
        for next in &graph[at].1 {
            let Some(&next_idx) = index.get(next) else {
                continue;
            };
            match marks[next_idx] {
                Mark::Done => {}
                Mark::Open => {
                    let start = stack
                        .iter()
                        .position(|n| *n == next_idx)
                        .expect("open nodes are on the stack");
                    let mut path: Vec<String> = stack[start..]
                        .iter()
                        .map(|i| graph[*i].0.to_string())
                        .collect();
                    path.push(graph[next_idx].0.to_string());
                    return Some(ServiceDependencyCycle { path });
                }
                Mark::New => {
                    if let Some(c) = visit(next_idx, graph, index, marks, stack) {
                        return Some(c);
                    }
                }
            }
        }
        stack.pop();
        marks[at] = Mark::Done;
        None
    }
    let mut marks = vec![Mark::New; graph.len()];
    let mut stack = Vec::new();
    for i in 0..graph.len() {
        if marks[i] == Mark::New {
            if let Some(c) = visit(i, graph, &index, &mut marks, &mut stack) {
                return Some(c);
            }
        }
    }
    None
}

/// The Services of `services` that `dependencies` lists as `service:<name>`
/// — those whose `Service.<name>.<port>.*` the listing Step, Service, or
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

/// The `requiresServices` entries of `requirements` that `dependencies` lists
/// as `service:<name>` — the required Services whose
/// `Service.<name>.<port>.port` / `.connectAddress` the listing Step,
/// Service, or Job Environment may reference (§9 scope rules 2–4, §9.8 item
/// 2). Declaration
/// order; a name no requirement declares (an inline Service, or a typo)
/// yields nothing here. Listing a required Service grants access to its
/// values and nothing more: its scope is every Step whether or not any
/// entity lists it ([`ServiceScope::AllSteps`]).
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
/// a dependency: the Step must list each as `service:<name>` (§9 scope
/// rule 3), and this is how the diagnostic finds the ones it does not.
#[must_use]
pub fn step_references(step: &StepTemplate) -> BTreeSet<String> {
    let mut symbols = HashSet::new();
    let script = step
        .resolve_syntax_sugar()
        .ok()
        .flatten()
        .or_else(|| step.script.clone());
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
/// dependency: a Job Environment must list each as `service:<name>` (§9
/// scope rule 4), and this is how the diagnostic finds the ones it does
/// not.
#[must_use]
pub fn environment_references(env: &Environment) -> BTreeSet<String> {
    let mut symbols = HashSet::new();
    collect_environment(env, &mut symbols);
    service_names(&symbols)
}

/// True when any format string of `env` references a `Service.*` value —
/// for a Step Environment, which has no `dependencies` of its own, the
/// condition under which an absent `runScope` defaults to `[TASK]` (§4
/// item 4).
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
/// as `service:<name>`.
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

    fn svc(name: &str, args: &[&str], deps: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
        let deps = if deps.is_empty() {
            String::new()
        } else {
            let deps: Vec<String> = deps.iter().map(|d| format!("{{dependsOn: {d}}}")).collect();
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
            let deps: Vec<String> = deps.iter().map(|d| format!("{{dependsOn: {d}}}")).collect();
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
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  dependencies: [{{dependsOn: service:Wide}}]\n  \
             variables: {{ADDR: \"{{{{ Service.Wide.main.connectAddress }}}}\"}}\n\
             services:\n{}{}{}{}steps:\n{}{}{}",
            svc("Wide", &[], &[]),
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
        // An undeclared name is treated as an external Service.
        assert_eq!(scopes.scope_of("Ext"), ServiceScope::AllSteps);
        assert_eq!(scopes.iter().count(), 4);
    }

    #[test]
    fn a_job_wide_dependent_makes_the_depended_on_service_job_wide() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  dependencies: [{{dependsOn: service:Front}}]\n  \
             variables: {{ADDR: \"{{{{ Service.Front.main.connectAddress }}}}\"}}\n\
             services:\n{}{}steps:\n{}{}",
            svc(
                "Front",
                &["{{ Service.Back.main.port }}"],
                &["service:Back"]
            ),
            svc("Back", &[], &[]),
            step("S1", &[], &[]),
            step("S2", &[], &[]),
        );
        let scopes = compute_service_scopes(&template(&yaml)).unwrap();
        assert_eq!(scopes.scope_of("Front"), ServiceScope::AllSteps);
        assert_eq!(scopes.scope_of("Back"), ServiceScope::AllSteps);
    }

    #[test]
    fn a_job_environment_reference_without_a_dependency_is_not_rule_3() {
        // Listing without referencing puts every Step in scope; referencing
        // without listing (a validation error) is not an edge, and a
        // required Service listed by a Job Environment is not one either.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             requiresServices: [{{name: Ext, ports: [{{name: main}}]}}]\n\
             jobEnvironments:\n\
             - name: Opaque\n  dependencies: [{{dependsOn: service:Listed}}, {{dependsOn: service:Ext}}]\n  \
             variables: {{A: x}}\n\
             - name: Stray\n  variables: {{ADDR: \"{{{{ Service.Referenced.main.connectAddress }}}}\"}}\n\
             services:\n{}{}steps:\n{}",
            svc("Listed", &[], &[]),
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
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             requiresServices: [{{name: Ext, ports: [{{name: main}}]}}]\n\
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
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
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
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
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
            vec!["service:A", "service:B", "service:C", "service:A"]
        );
        assert_eq!(
            cycle.to_string(),
            "dependencies contain a cycle: service:A -> service:B -> service:C -> service:A."
        );
        assert_eq!(
            service_dependency_cycle(jt.services()).unwrap().path,
            vec!["service:A", "service:B", "service:C", "service:A"]
        );
        // Step -> Service -> Step.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             services:\n{}steps:\n{}",
            svc("X", &[], &["Use"]),
            step("Use", &[], &["service:X"]),
        );
        let cycle = compute_service_scopes(&template(&yaml)).unwrap_err();
        assert_eq!(cycle.path, vec!["Use", "service:X", "Use"]);
        // Step -> Step.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             steps:\n{}{}",
            step("A", &[], &["B"]),
            step("B", &[], &["A"]),
        );
        let cycle = compute_service_scopes(&template(&yaml)).unwrap_err();
        assert_eq!(
            cycle.to_string(),
            "dependencies contain a cycle: A -> B -> A."
        );
        // Self-dependencies are not edges here (validation reports them).
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             services:\n{}steps:\n{}",
            svc("A", &[], &["service:A"]),
            step("S1", &[], &["service:A"]),
        );
        let jt = template(&yaml);
        assert!(service_dependency_cycle(jt.services()).is_none());
        assert!(compute_service_scopes(&jt).is_ok());
    }

    #[test]
    fn listed_services_follow_the_dependencies_list() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
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
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             requiresServices:\n- name: R1\n  ports: [{{name: main}}]\n- name: R2\n  \
             ports: [{{name: main}}]\n- name: R3\n  ports: [{{name: main}}]\nservices:\n{}steps:\n{}",
            svc("A", &[], &[]),
            step("S1", &[], &["service:R3", "service:A", "service:R1", "service:Nope"]),
        );
        let jt = template(&yaml);
        let names: Vec<&str> =
            listed_requirements(jt.steps[0].dependencies.as_deref(), jt.requires_services())
                .map(|r| r.name.as_str())
                .collect();
        assert_eq!(names, vec!["R1", "R3"]);
        assert_eq!(listed_requirements(None, jt.requires_services()).count(), 0);
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
}
