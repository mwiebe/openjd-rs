// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Service scope (Template Schemas §9.1, RFC 0009 "Service scope").
//!
//! The scope of a Service declared in a Job Template's `services` is the set
//! of Steps whose Tasks depend on it, computed from the template's
//! `Service.<name>.*` references:
//!
//! 1. a Step whose `script` or `stepEnvironments` references `Service.X.*`
//!    is in `X`'s scope;
//! 2. when any `jobEnvironments` entry references `Service.X.*`, every Step
//!    is in `X`'s scope;
//! 3. when Service `Y` references `Service.X.*`, `X`'s scope includes `Y`'s
//!    (transitively), and `Y` starts only after `X` is READY — so the
//!    references among a document's Services are their dependency graph,
//!    which must be acyclic;
//! 4. a Service that nothing references has every Step in its scope.
//!
//! [`compute_service_scopes`] applies the rules to a decoded Job Template
//! and [`service_reference_cycle`] finds a reference cycle in either kind of
//! template. Both read the fields that may legally reference `Service.*`
//! (actions, `variables`, embedded files, script-level `let`) — a reference
//! in a job-creation-time field is rejected by the format-string pass and
//! does not place anything in a scope. The scheduler side of the rules
//! (start before any Task of a Step in scope, stop once none remains) is the
//! runtime's.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::actions::{Action, CancelationMode};
use super::environment::{EmbeddedFile, Environment};
use super::job_template::JobTemplate;
use super::service::Service;
use super::step::StepTemplate;
use crate::format_string::FormatString;

/// The Steps whose Tasks depend on a Service (§9.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServiceScope {
    /// Every Step of the Job: a Service a Job Environment references, one
    /// that nothing references, one a Job-wide Service references, or an
    /// external Service. The Service lives as long as the Job.
    AllSteps,
    /// Exactly these Steps, by name. The Service is started before the first
    /// Task of any of them and stopped once none has a Task left to run.
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

/// `every Step`, or `Steps A, B` — the form the run log uses.
impl fmt::Display for ServiceScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllSteps => f.write_str("every Step"),
            Self::Steps { steps } => {
                if steps.len() == 1 {
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
    /// The other inline Services this Service references through
    /// `Service.<name>.*` (rule 3): it starts after each is READY and stops
    /// before any of them. Sorted; never contains the Service itself or a
    /// required external Service.
    pub references: BTreeSet<String>,
    /// The Steps that reference the Service directly from their `script` or
    /// `stepEnvironments` (rule 1), in template order.
    pub referencing_steps: Vec<String>,
    /// True when a `jobEnvironments` entry references the Service (rule 2).
    pub referenced_by_job_environment: bool,
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

/// A cycle among the `Service.*` references of a template's Services
/// (§9.1 rule 3, §9.9 item 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceReferenceCycle {
    /// The Service names along the cycle, starting and ending with the same
    /// name: `["A", "B", "A"]`.
    pub path: Vec<String>,
}

impl fmt::Display for ServiceReferenceCycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the Service.* references among the Services form a cycle: {}; a Service may not \
             reference a Service that (transitively) references it.",
            self.path.join(" -> ")
        )
    }
}

/// Compute the scope of every Service of `jt` (§9.1), or report the first
/// reference cycle found.
///
/// The names in `jt.requires_services` are external Services: references to
/// them are not reference edges and do not place anything in an inline
/// Service's scope. Only the fields that may legally reference `Service.*`
/// are read (see the module docs).
pub fn compute_service_scopes(jt: &JobTemplate) -> Result<ServiceScopes, ServiceReferenceCycle> {
    let services = jt.services();
    let names: HashSet<&str> = services.iter().map(|s| s.name.as_str()).collect();
    if names.is_empty() {
        return Ok(ServiceScopes::default());
    }

    // Direct references (rule 3) and the cycle check.
    let mut references: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for svc in services {
        let refs: BTreeSet<String> = service_references(svc)
            .into_iter()
            .filter(|n| n != &svc.name && names.contains(n.as_str()))
            .collect();
        references.insert(&svc.name, refs);
    }
    if let Some(cycle) = find_cycle(services.iter().map(|s| s.name.as_str()), &references) {
        return Err(cycle);
    }
    // Reverse edges: who references X.
    let mut referenced_by: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, refs) in &references {
        for to in refs {
            referenced_by
                .entry(names_key(&names, to))
                .or_default()
                .push(from);
        }
    }

    // Rules 1 and 2.
    let mut direct_steps: HashMap<&str, Vec<String>> = HashMap::new();
    for step in &jt.steps {
        for name in step_references(step) {
            if names.contains(name.as_str()) {
                let list = direct_steps.entry(names_key(&names, &name)).or_default();
                if !list.contains(&step.name) {
                    list.push(step.name.clone());
                }
            }
        }
    }
    let mut job_wide: HashSet<&str> = HashSet::new();
    for env in jt.job_environments.iter().flatten() {
        for name in environment_references(env) {
            if names.contains(name.as_str()) {
                job_wide.insert(names_key(&names, &name));
            }
        }
    }

    // Rule 4 and the transitive closure of rule 3, memoized over the
    // (acyclic) referenced-by graph.
    let mut memo: HashMap<&str, ServiceScope> = HashMap::new();
    fn scope_of<'a>(
        name: &'a str,
        job_wide: &HashSet<&str>,
        direct_steps: &HashMap<&str, Vec<String>>,
        referenced_by: &HashMap<&str, Vec<&'a str>>,
        memo: &mut HashMap<&'a str, ServiceScope>,
    ) -> ServiceScope {
        if let Some(s) = memo.get(name) {
            return s.clone();
        }
        let referrers = referenced_by.get(name).map(Vec::as_slice).unwrap_or(&[]);
        let direct = direct_steps.get(name).map(Vec::as_slice).unwrap_or(&[]);
        let scope = if job_wide.contains(name) || (referrers.is_empty() && direct.is_empty()) {
            ServiceScope::AllSteps
        } else {
            let mut steps: BTreeSet<String> = direct.iter().cloned().collect();
            let mut all = false;
            for r in referrers {
                match scope_of(r, job_wide, direct_steps, referenced_by, memo) {
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
        let scope = scope_of(name, &job_wide, &direct_steps, &referenced_by, &mut memo);
        by_name.insert(
            svc.name.clone(),
            ComputedServiceScope {
                name: svc.name.clone(),
                scope,
                references: references.get(name).cloned().unwrap_or_default(),
                referencing_steps: direct_steps.get(name).cloned().unwrap_or_default(),
                referenced_by_job_environment: job_wide.contains(name),
            },
        );
    }
    Ok(ServiceScopes { by_name })
}

/// The `&str` key of `names` equal to `name` (so borrowed keys outlive the
/// owned strings they were looked up with).
fn names_key<'a>(names: &HashSet<&'a str>, name: &str) -> &'a str {
    names.get(name).copied().expect("caller checked membership")
}

/// Find a cycle among the `Service.*` references of `services` (any
/// template's list), or `None` when the reference graph is acyclic.
pub fn service_reference_cycle(services: &[Service]) -> Option<ServiceReferenceCycle> {
    let names: HashSet<&str> = services.iter().map(|s| s.name.as_str()).collect();
    let references: BTreeMap<&str, BTreeSet<String>> = services
        .iter()
        .map(|svc| {
            let refs = service_references(svc)
                .into_iter()
                .filter(|n| n != &svc.name && names.contains(n.as_str()))
                .collect();
            (svc.name.as_str(), refs)
        })
        .collect();
    find_cycle(services.iter().map(|s| s.name.as_str()), &references)
}

/// DFS over `references` in declaration order; the first back edge found
/// yields the cycle, spelled from the revisited node back to itself.
fn find_cycle<'a>(
    order: impl Iterator<Item = &'a str>,
    references: &BTreeMap<&'a str, BTreeSet<String>>,
) -> Option<ServiceReferenceCycle> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    fn visit<'a>(
        node: &'a str,
        references: &BTreeMap<&'a str, BTreeSet<String>>,
        marks: &mut HashMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
    ) -> Option<ServiceReferenceCycle> {
        marks.insert(node, Mark::Open);
        stack.push(node);
        if let Some(refs) = references.get(node) {
            for next in refs {
                let next_key = references
                    .keys()
                    .copied()
                    .find(|k| *k == next.as_str())
                    .expect("references are filtered to declared names");
                match marks.get(next_key).copied().unwrap_or(Mark::New) {
                    Mark::Done => {}
                    Mark::Open => {
                        let start = stack
                            .iter()
                            .position(|n| *n == next_key)
                            .expect("open nodes are on the stack");
                        let mut path: Vec<String> =
                            stack[start..].iter().map(|s| s.to_string()).collect();
                        path.push(next_key.to_string());
                        return Some(ServiceReferenceCycle { path });
                    }
                    Mark::New => {
                        if let Some(c) = visit(next_key, references, marks, stack) {
                            return Some(c);
                        }
                    }
                }
            }
        }
        stack.pop();
        marks.insert(node, Mark::Done);
        None
    }
    let mut marks = HashMap::new();
    let mut stack = Vec::new();
    for name in order {
        if marks.get(name).copied().unwrap_or(Mark::New) == Mark::New {
            if let Some(c) = visit(name, references, &mut marks, &mut stack) {
                return Some(c);
            }
        }
    }
    None
}

// ── Reference extraction ─────────────────────────────────────────────

/// The Service names a Step references from its `script` (actions,
/// embedded files, script `let`) and `stepEnvironments` (§9.1 rule 1).
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
/// actions, embedded files and script `let` (§9.1 rule 2 for a Job
/// Environment; the `runScope` default of §4 item 3).
#[must_use]
pub fn environment_references(env: &Environment) -> BTreeSet<String> {
    let mut symbols = HashSet::new();
    collect_environment(env, &mut symbols);
    service_names(&symbols)
}

/// True when any format string of `env` references a `Service.*` value —
/// the condition under which an Environment without `runScope` defaults to
/// `[TASK]` (§4 item 3).
#[must_use]
pub fn environment_references_service(env: &Environment) -> bool {
    let mut symbols = HashSet::new();
    collect_environment(env, &mut symbols);
    symbols.iter().any(|s| s.starts_with("Service."))
}

/// The Service names a Service references from its `variables`, actions,
/// embedded files and `<ServiceScript>.let` (§9.1 rule 3), including its
/// own name when it references itself; `Service.File.*` is not a Service.
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

    fn svc(name: &str, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
        format!(
            "- name: {name}\n  ports: [{{name: main}}]\n  script:\n    actions:\n      onRun: {{command: x, args: [{}]}}\n",
            args.join(", ")
        )
    }

    fn step(name: &str, args: &[&str], deps: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
        let deps: Vec<String> = deps.iter().map(|d| format!("{{dependsOn: {d}}}")).collect();
        format!(
            "- name: {name}\n  dependencies: [{}]\n  script:\n    actions:\n      onRun: {{command: x, args: [{}]}}\n",
            deps.join(", "),
            args.join(", ")
        )
    }

    #[test]
    fn display_forms() {
        assert_eq!(ServiceScope::AllSteps.to_string(), "every Step");
        assert_eq!(ServiceScope::steps(["B"]).to_string(), "Step B");
        assert_eq!(ServiceScope::steps(["B", "A"]).to_string(), "Steps A, B");
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
             jobEnvironments:\n- name: E\n  variables: {{ADDR: \"{{{{ Service.Wide.main.connectAddress }}}}\"}}\n\
             services:\n{}{}{}{}steps:\n{}{}{}",
            svc("Wide", &[]),
            svc("Front", &["{{ Service.Back.main.port }}"]),
            svc("Back", &[]),
            svc("Lonely", &[]),
            step("S1", &["{{ Service.Front.main.port }}"], &[]),
            step("S2", &["{{ Service.Back.main.port }}"], &["S1"]),
            step("S3", &[], &["S2"]),
        );
        let jt = template(&yaml);
        let scopes = compute_service_scopes(&jt).unwrap();
        assert_eq!(scopes.scope_of("Wide"), ServiceScope::AllSteps);
        assert!(scopes.get("Wide").unwrap().referenced_by_job_environment);
        assert_eq!(scopes.scope_of("Lonely"), ServiceScope::AllSteps);
        assert_eq!(scopes.scope_of("Front"), ServiceScope::steps(["S1"]));
        // Back: S2 directly, S1 through Front.
        assert_eq!(scopes.scope_of("Back"), ServiceScope::steps(["S1", "S2"]));
        let front = scopes.get("Front").unwrap();
        assert_eq!(
            front.references.iter().collect::<Vec<_>>(),
            vec![&"Back".to_string()]
        );
        assert_eq!(front.referencing_steps, vec!["S1".to_string()]);
        assert!(scopes.get("Back").unwrap().references.is_empty());
        // An undeclared name is treated as an external Service.
        assert_eq!(scopes.scope_of("Ext"), ServiceScope::AllSteps);
        assert_eq!(scopes.iter().count(), 4);
    }

    #[test]
    fn a_job_wide_referrer_makes_the_referenced_service_job_wide() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             services:\n{}{}steps:\n{}{}",
            svc("Front", &["{{ Service.Back.main.port }}"]),
            svc("Back", &[]),
            step("S1", &[], &[]),
            step("S2", &[], &[]),
        );
        let scopes = compute_service_scopes(&template(&yaml)).unwrap();
        assert_eq!(scopes.scope_of("Front"), ServiceScope::AllSteps);
        assert_eq!(scopes.scope_of("Back"), ServiceScope::AllSteps);
    }

    #[test]
    fn requirement_names_are_not_reference_edges() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             requiresServices: [{{name: Ext, ports: [{{name: main}}]}}]\n\
             services:\n{}steps:\n{}",
            svc("Own", &["{{ Service.Ext.main.port }}"]),
            step("S1", &["{{ Service.Ext.main.port }}"], &[]),
        );
        let scopes = compute_service_scopes(&template(&yaml)).unwrap();
        assert!(scopes.get("Own").unwrap().references.is_empty());
        assert_eq!(scopes.scope_of("Own"), ServiceScope::AllSteps);
        assert!(scopes.get("Ext").is_none());
    }

    #[test]
    fn cycles_are_reported_with_their_path() {
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             services:\n{}{}{}steps:\n{}",
            svc("A", &["{{ Service.B.main.port }}"]),
            svc("B", &["{{ Service.C.main.port }}"]),
            svc("C", &["{{ Service.A.main.port }}"]),
            step("S1", &[], &[]),
        );
        let jt = template(&yaml);
        let cycle = compute_service_scopes(&jt).unwrap_err();
        assert_eq!(cycle.path, vec!["A", "B", "C", "A"]);
        assert_eq!(
            cycle.to_string(),
            "the Service.* references among the Services form a cycle: A -> B -> C -> A; a \
             Service may not reference a Service that (transitively) references it."
        );
        assert_eq!(
            service_reference_cycle(jt.services()).unwrap().path,
            vec!["A", "B", "C", "A"]
        );
        // Self-references are not edges.
        let yaml = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [SERVICE, EXPR]\nname: J\n\
             services:\n{}steps:\n{}",
            svc("A", &["{{ Service.A.main.bindAddress }}"]),
            step("S1", &[], &[]),
        );
        assert!(service_reference_cycle(template(&yaml).services()).is_none());
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
