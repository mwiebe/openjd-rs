// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Scope-specific diagnostics for out-of-scope `Service.*` and `Task.*`
//! references (RFC 0009 "The `Service.*` scope", Template Schemas §9 scope
//! list, §9.8, §9.9 items 1–2, §4 item 3.2, §7.3.1).
//!
//! Pass 8 validates every format string against a symbol table seeded with
//! exactly the `Service.*` values in scope at that site, so a reference that
//! breaks a scope rule surfaces as the expression language's generic
//! `Undefined variable: 'Service.X.p.port'.` — sometimes with a `Did you
//! mean` pointing at a *different* Service whose name is one edit away. The
//! author then has to work out which scope rule they hit.
//!
//! This module runs after pass 8 has walked a document and rewrites those
//! messages when the referenced Service **is declared or required somewhere
//! in the document**: it classifies the reference site from the error's
//! path, finds the declaration, and states the rule that keeps it out of
//! scope. A name that is declared nowhere keeps the generic message (with
//! its suggestion), since a typo is then the likeliest cause. Only the
//! sentence after `Failed to parse interpolation expression at [s, e]. ` (or
//! after `Invalid expression in let binding 'x': `) changes; the path, the
//! expression source and caret lines that follow it are untouched.
//!
//! The rules, in the order they are tried for a declared or required
//! `Service.<svc>`:
//!
//! 1. `Task.*` or `Step.*` inside a Service (its actions, `variables`,
//!    `let`, embedded files): *Task.\* is not available within a Service.*
//!    / *Step.\* is not available within a Service.* (§9 item 3.)
//! 2. The site is a job-creation-time field (§9.9 item 2: `hostRequirements`,
//!    a `<StepTemplate>`'s or `<Service>`'s `let` list — not a script's —, a
//!    `parameterSpace` range, an `<Action>`'s `timeout`, a cancelation
//!    method's `mode` / `notifyPeriodInSeconds`, a `<ServicePort>`'s
//!    `port`, a `<ServiceRestartPolicy>`'s `maxAttempts`, the four
//!    `<ServiceHealthCheck>` numeric fields): *Service.\* is not available
//!    in `<field>`: it is resolved at job creation, before any Service has
//!    an endpoint.* (§3.6.2, §9.9 items 1–2.) Pass 8 never seeds `Service.*`
//!    into the symbol table of these fields, so the reference is undefined
//!    there whatever the scope; this rule only names the field.
//! 3. The site is a Job Environment (or an Environment Template's
//!    `environment`) whose explicit `runScope` includes `SERVICE`: the
//!    error is **dropped**. Pass 11 reports the rule once, on the `runScope`
//!    list — *Environment 'E' is entered in Service Sessions (its runScope
//!    includes SERVICE) and may not reference Service.\*; declare runScope:
//!    [TASK] if it configures Tasks.* (§4 item 4 constraint 2) — and each
//!    reference is a consequence of it. A Step Environment gives no
//!    `runScope` (pass 11 rejects one) and is always entered in Task
//!    Sessions, so this rule never applies to it.
//! 4. A Service — declared or required — referenced from a Step's `script`,
//!    one of its `stepEnvironments`, another Service, a Job Environment, or
//!    an Environment Template's `environment` that does not list
//!    `service: <name>` in its `dependencies` (§9 scope rules 2–5, §9.8
//!    item 2, §9.9 item 1): *Step 'Render' references Service.Cache.main.port
//!    but does not list service: Cache in dependencies.* — *Step 'Render'
//!    references Service.Cache.main.port in stepEnvironments 'Tools' but
//!    does not list service: Cache in dependencies.* — *Service 'Front'
//!    references Service.Back.main.port but does not list service: Back in
//!    dependencies.* — *Environment 'CacheClient' references
//!    Service.Cache.main.port but does not list service: Cache in
//!    dependencies.* The dependency is the author's statement that the
//!    entity needs the Service; the reference alone is not taken as one. A
//!    required Service follows the same rule: listing it is what grants
//!    access to its values, though it changes nothing about scheduling. A
//!    Step Environment has no list of its own and follows its Step's.
//! 5. The port is not declared: *Service 'S' has no port 'mian'; declared
//!    ports: main.* — or, for a required Service, *required Service 'R'
//!    has no port 'x'; declared ports: main.* (§9.8.)
//! 6. `bindAddress` of a required Service: *bindAddress of required
//!    Service 'R' is not available; use connectAddress to reach it.*
//!    (§9.8 item 2.)
//! 7. `bindAddress` outside the declaring Service: *Service.P.main.bindAddress
//!    is available only within the Service 'P' itself; use connectAddress to
//!    reach it from elsewhere.* (§7.3.1.)
//!
//! When the document does not declare `SERVICE` at all, every `Undefined
//! variable: 'Service.<svc>…'` whose `<svc>` the document's `services` or
//! `requiresServices` names is **dropped**: the gating pass has reported
//! the list itself (*services requires the SERVICE extension.*), and the
//! reference is a consequence of that.
//!
//! The visibility rule has no exceptions: an entity sees a Service's values
//! iff it lists the Service. Anything else — an unknown value name after a
//! declared port, a reference with too few components, `Service.File.*` —
//! keeps the generic message.

use crate::error::{PathElement, ValidationError, ValidationErrors};
use crate::template::{
    lists_service, Environment, EnvironmentTemplate, JobTemplate, RunScope, Service, StepTemplate,
};
use crate::types::{ModelExtension, ValidationContext};

/// One Service the document knows: declared in its `services`, or required
/// by a Job Template's `requiresServices`.
enum Known<'a> {
    Declared(&'a Service),
    Required { port_names: Vec<&'a str> },
}

impl Known<'_> {
    fn port_names(&self) -> Vec<&str> {
        match self {
            Self::Declared(svc) => svc.ports.iter().map(|p| p.name.as_str()).collect(),
            Self::Required { port_names } => port_names.clone(),
        }
    }
}

/// The kind of entity a format string belongs to, read off the error path.
enum Site<'a> {
    /// A Step's `script` (its Tasks' scope).
    Task { step: &'a StepTemplate },
    /// A Service's own session-scope fields: `variables` and `script`.
    Service { service: &'a Service },
    /// A Job or Step Environment, or an Environment Template's
    /// `environment`; `step` is the owning Step of a Step Environment.
    Environment {
        env: &'a Environment,
        step: Option<&'a StepTemplate>,
    },
    /// A field resolved at job creation, named for the message.
    JobCreation(&'static str),
}

/// The document's declarations and lookups, over either template kind.
struct Document<'a> {
    job_template: Option<&'a JobTemplate>,
    env_template: Option<&'a EnvironmentTemplate>,
}

impl<'a> Document<'a> {
    fn for_job_template(jt: &'a JobTemplate) -> Self {
        Self {
            job_template: Some(jt),
            env_template: None,
        }
    }

    fn for_environment_template(et: &'a EnvironmentTemplate) -> Self {
        Self {
            job_template: None,
            env_template: Some(et),
        }
    }

    fn services(&self) -> &'a [Service] {
        match (self.job_template, self.env_template) {
            (Some(jt), _) => jt.services(),
            (None, Some(et)) => et.services(),
            (None, None) => &[],
        }
    }

    /// What the document knows about the Service named `name`.
    fn known(&self, name: &str) -> Option<Known<'a>> {
        if let Some(svc) = self.services().iter().find(|s| s.name == name) {
            return Some(Known::Declared(svc));
        }
        let req = self
            .job_template?
            .requires_services()
            .iter()
            .find(|r| r.name == name)?;
        Some(Known::Required {
            port_names: req.port_names().collect(),
        })
    }

    /// Classify the entity the error at `path` belongs to; `None` when the
    /// path is not one this module reasons about.
    fn site(&self, path: &[PathElement]) -> Option<Site<'a>> {
        use PathElement::{Field, Index};
        // Job-creation fields anywhere under the site take precedence: a
        // Service.* value there is undefined whatever the scope.
        if let Some(name) = job_creation_field(path) {
            return Some(Site::JobCreation(name));
        }
        match path {
            [Field(f), Index(i), rest @ ..] if f == "steps" => {
                let step = self.job_template?.steps.get(*i)?;
                match rest {
                    [Field(g), Index(j), ..] if g == "stepEnvironments" => {
                        let env = step.step_environments.as_ref()?.get(*j)?;
                        Some(Site::Environment {
                            env,
                            step: Some(step),
                        })
                    }
                    [Field(g), ..] if g == "script" => Some(Site::Task { step }),
                    _ => None,
                }
            }
            [Field(f), Index(k), ..] if f == "services" => {
                let service = self.services().get(*k)?;
                Some(Site::Service { service })
            }
            [Field(f), Index(i), ..] if f == "jobEnvironments" => {
                let env = self.job_template?.job_environments.as_ref()?.get(*i)?;
                Some(Site::Environment { env, step: None })
            }
            [Field(f), ..] if f == "environment" => Some(Site::Environment {
                env: self.env_template?.environment.as_ref()?,
                step: None,
            }),
            _ => None,
        }
    }
}

/// The job-creation-time field `path` points into, if any, as the message
/// names it.
fn job_creation_field(path: &[PathElement]) -> Option<&'static str> {
    let fields: Vec<&str> = path
        .iter()
        .filter_map(|e| match e {
            PathElement::Field(f) => Some(f.as_str()),
            PathElement::Index(_) => None,
        })
        .collect();
    // The leaf decides for the `@fmtstring` scalars; `hostRequirements` and
    // `parameterSpace` anywhere in the path; `let` only outside a `script`
    // (a `<StepTemplate>`'s or a `<Service>`'s own list, resolved at job
    // creation — a `<StepScript>`, `<ServiceScript>` or
    // `<EnvironmentScript>` `let` is a session-scope site).
    if fields.contains(&"hostRequirements") {
        return Some("hostRequirements");
    }
    if fields.contains(&"parameterSpace") {
        return Some("a parameterSpace range");
    }
    if fields.contains(&"let") && !fields.contains(&"script") {
        return Some("a let binding");
    }
    match fields.last().copied() {
        Some("timeout") => Some("timeout"),
        // A cancelation method's `mode` and `notifyPeriodInSeconds` are both
        // reported on the `cancelation` path.
        Some("cancelation") => Some("a cancelation method's mode or notifyPeriodInSeconds"),
        Some("notifyPeriodInSeconds") => Some("notifyPeriodInSeconds"),
        Some("port") if fields.contains(&"ports") => Some("port"),
        Some("readinessIntervalSeconds") => Some("readinessIntervalSeconds"),
        Some("readinessTimeoutSeconds") => Some("readinessTimeoutSeconds"),
        Some("healthIntervalSeconds") => Some("healthIntervalSeconds"),
        Some("failureThreshold") => Some("failureThreshold"),
        Some("maxAttempts") => Some("maxAttempts"),
        _ => None,
    }
}

/// `Undefined variable: '<name>'.` plus any ` Did you mean…` suggestion, as
/// one span of a message line: the start of the sentence and the end of the
/// line it is on.
fn undefined_variable_span(message: &str) -> Option<(usize, usize, &str)> {
    const PREFIX: &str = "Undefined variable: '";
    let start = message.find(PREFIX)?;
    let name_start = start + PREFIX.len();
    let name_end = name_start + message[name_start..].find("'.")?;
    let name = &message[name_start..name_end];
    let line_end = message[start..]
        .find('\n')
        .map_or(message.len(), |n| start + n);
    Some((start, line_end, name))
}

/// Rewrite the errors of `errors` from index `from` on that report an
/// out-of-scope `Service.*` / `Task.*` reference whose Service (or Task
/// scope) the document declares, per the module rules.
fn refine(doc: &Document<'_>, service_active: bool, errors: &mut ValidationErrors, from: usize) {
    let mut index = 0;
    errors.errors.retain_mut(|err| {
        let i = index;
        index += 1;
        if i < from {
            return true;
        }
        let Some((start, end, name)) = undefined_variable_span(&err.message) else {
            return true;
        };
        // Without the extension no Service.* value exists anywhere, and the
        // gating pass has already reported the `services` /
        // `requiresServices` list the name came from: a reference to one
        // of its Services is a consequence of that, not a finding.
        if !service_active {
            let declared = name
                .strip_prefix("Service.")
                .and_then(|rest| rest.split('.').next())
                .is_some_and(|svc| doc.known(svc).is_some());
            return !declared;
        }
        let Some(site) = doc.site(&err.path) else {
            return true;
        };
        let within_service =
            matches!(err.path.first(), Some(PathElement::Field(f)) if f == "services");
        match reason(doc, &site, name, within_service) {
            Some(Refinement::Rewrite(reason)) => rewrite(err, start, end, &reason),
            Some(Refinement::Drop) => return false,
            None => {}
        }
        true
    });
}

/// What [`refine`] does with one `Undefined variable` error.
enum Refinement {
    /// Replace the generic sentence with the scope rule it breaks.
    Rewrite(String),
    /// Remove the error: pass 11 reports the rule once, on the field that
    /// breaks it, and this error is a consequence.
    Drop,
}

/// The scope rule `name` breaks at `site`, if this module states one.
/// `within_service` is true when the error's path is under `services`, so
/// that a `<Service>`'s job-creation fields (`let`, `hostRequirements`)
/// count as inside the Service for rule 1.
fn reason(
    doc: &Document<'_>,
    site: &Site<'_>,
    name: &str,
    within_service: bool,
) -> Option<Refinement> {
    // Rule 1: Task.* and Step.* are never available within a Service (§9
    // item 3): there is no Task, and no Step, in a Service Session.
    if let Some(root) = name
        .split_once('.')
        .map(|(root, _)| root)
        .filter(|root| ["Task", "Step"].contains(root))
    {
        return match site {
            Site::Service { .. } | Site::JobCreation(_) if within_service => Some(
                Refinement::Rewrite(format!("{root}.* is not available within a Service.")),
            ),
            _ => None,
        };
    }
    let mut parts = name.strip_prefix("Service.")?.splitn(3, '.');
    let svc = parts.next()?;
    let port = parts.next();
    let value = parts.next();
    let known = doc.known(svc)?;

    // Rule 2: job-creation fields never see Service.*.
    if let Site::JobCreation(field) = site {
        return Some(Refinement::Rewrite(format!(
            "Service.* is not available in {field}: it is resolved at job creation, before any \
             Service has an endpoint."
        )));
    }

    // Rule 3: a Job Environment entered in Service Sessions (its explicit
    // runScope includes SERVICE). Pass 11 reports that on the runScope
    // list, once, because it references Service.*; each reference is a
    // consequence. A Step Environment never has a runScope (pass 11 rejects
    // one, §4 item 4 constraint 4) and is always a Task-Session site.
    if let Site::Environment { env, step: None } = site {
        if env.run_scope.is_some() && env.runs_in(RunScope::Service) {
            return Some(Refinement::Drop);
        }
    }

    // Rule 4: a declared or required Service the Step, Service, or
    // Environment does not list in its dependencies.
    let missing = |who: String, where_: String| {
        format!("{who} references {name}{where_} but does not list service: {svc} in dependencies.")
    };
    match site {
        Site::Task { step } if !lists_service(step.dependencies.as_deref(), svc) => {
            return Some(Refinement::Rewrite(missing(
                format!("Step '{}'", step.name),
                String::new(),
            )));
        }
        Site::Environment {
            env,
            step: Some(step),
        } if !lists_service(step.dependencies.as_deref(), svc) => {
            return Some(Refinement::Rewrite(missing(
                format!("Step '{}'", step.name),
                format!(" in stepEnvironments '{}'", env.name),
            )));
        }
        Site::Environment { env, step: None }
            if !lists_service(env.dependencies.as_deref(), svc) =>
        {
            return Some(Refinement::Rewrite(missing(
                format!("Environment '{}'", env.name),
                String::new(),
            )));
        }
        Site::Service { service }
            if service.name != svc && !lists_service(service.dependencies.as_deref(), svc) =>
        {
            return Some(Refinement::Rewrite(missing(
                format!("Service '{}'", service.name),
                String::new(),
            )));
        }
        _ => {}
    }

    // Rule 5: the port.
    let port = port?;
    let declared_ports = known.port_names();
    if !declared_ports.contains(&port) {
        let what = match known {
            Known::Declared(_) => "Service",
            Known::Required { .. } => "required Service",
        };
        return Some(Refinement::Rewrite(format!(
            "{what} '{svc}' has no port '{port}'; declared ports: {}.",
            declared_ports.join(", ")
        )));
    }

    if value == Some("bindAddress") {
        match known {
            // Rule 6: a required Service's bindAddress is never in scope.
            Known::Required { .. } => {
                return Some(Refinement::Rewrite(format!(
                    "bindAddress of required Service '{svc}' is not available; use connectAddress \
                     to reach it."
                )));
            }
            // Rule 7: bindAddress outside the Service itself.
            Known::Declared(_) => {
                let within = matches!(site, Site::Service { service } if service.name == svc);
                if !within {
                    return Some(Refinement::Rewrite(format!(
                        "Service.{svc}.{port}.bindAddress is available only within the Service \
                         '{svc}' itself; use connectAddress to reach it from elsewhere."
                    )));
                }
            }
        }
    }
    None
}

/// Replace `[start, end)` of the error's message — the `Undefined variable`
/// sentence and its suggestion — with `reason`, and mirror the change into
/// the structured detail.
fn rewrite(err: &mut ValidationError, start: usize, end: usize, reason: &str) {
    let old_sentence = err.message[start..end].to_string();
    err.message.replace_range(start..end, reason);
    if let Some(detail) = err.detail.as_mut() {
        if detail.summary.trim_end() == old_sentence {
            detail.summary = reason.to_string();
        }
        for span in &mut detail.spans {
            if span.summary.trim_end() == old_sentence {
                span.summary = reason.to_string();
            }
        }
    }
}

/// Refine the out-of-scope `Service.*` / `Task.*` errors pass 8 added to
/// `errors` (from index `from`) for job template `jt`.
pub(crate) fn refine_job_template(
    jt: &JobTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
    from: usize,
) {
    refine(
        &Document::for_job_template(jt),
        ctx.profile.has_extension(ModelExtension::Service),
        errors,
        from,
    );
}

/// As [`refine_job_template`], for an Environment Template.
pub(crate) fn refine_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
    from: usize,
) {
    refine(
        &Document::for_environment_template(et),
        ctx.profile.has_extension(ModelExtension::Service),
        errors,
        from,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undefined_variable_span_finds_the_sentence_and_name() {
        let msg = "Failed to parse interpolation expression at [0, 29]. Undefined variable: \
                   'Service.Later.main.port'. Did you mean: Service.Cache.main.port\n  \
                   Service.Later.main.port\n  ~~~~~~^~~~";
        let (s, e, name) = undefined_variable_span(msg).unwrap();
        assert_eq!(name, "Service.Later.main.port");
        assert_eq!(
            &msg[s..e],
            "Undefined variable: 'Service.Later.main.port'. Did you mean: Service.Cache.main.port"
        );
        assert!(undefined_variable_span("Unknown function: 'f'").is_none());
    }

    #[test]
    fn job_creation_fields_are_recognized_by_path() {
        use PathElement::{Field, Index};
        let p = |fields: &[&str]| -> Vec<PathElement> {
            fields.iter().map(|f| Field(f.to_string())).collect()
        };
        assert_eq!(
            job_creation_field(&p(&["steps", "hostRequirements", "attributes", "anyOf"])),
            Some("hostRequirements")
        );
        assert_eq!(
            job_creation_field(&p(&["services", "let"])),
            Some("a let binding")
        );
        assert_eq!(
            job_creation_field(&p(&["steps", "let"])),
            Some("a let binding")
        );
        // A script's `let` is a session-scope site, whatever the script.
        assert_eq!(job_creation_field(&p(&["steps", "script", "let"])), None);
        assert_eq!(job_creation_field(&p(&["services", "script", "let"])), None);
        assert_eq!(
            job_creation_field(&p(&["jobEnvironments", "script", "let"])),
            None
        );
        assert_eq!(
            job_creation_field(&p(&["steps", "stepEnvironments", "script", "let"])),
            None
        );
        assert_eq!(
            job_creation_field(&p(&["steps", "script", "actions", "onRun", "timeout"])),
            Some("timeout")
        );
        assert_eq!(
            job_creation_field(&[
                Field("services".into()),
                Index(0),
                Field("ports".into()),
                Index(0),
                Field("port".into())
            ]),
            Some("port")
        );
        assert_eq!(
            job_creation_field(&p(&["steps", "script", "actions", "onRun", "args"])),
            None
        );
        assert_eq!(
            job_creation_field(&p(&["steps", "script", "actions", "onRun", "cancelation"])),
            Some("a cancelation method's mode or notifyPeriodInSeconds")
        );
        assert_eq!(
            job_creation_field(&p(&["services", "restartPolicy", "maxAttempts"])),
            Some("maxAttempts")
        );
        assert_eq!(
            job_creation_field(&p(&["services", "healthCheck", "healthIntervalSeconds"])),
            Some("healthIntervalSeconds")
        );
    }
}
