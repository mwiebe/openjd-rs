// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Scope-specific diagnostics for out-of-scope `Service.*` and `Task.*`
//! references (RFC 0009 "The `Service.*` scope", Template Schemas §9 scope
//! list, §9.7 items 1–2, §4 item 3.2, §7.3.1).
//!
//! Pass 8 validates every format string against a symbol table seeded with
//! exactly the `Service.*` values in scope at that site, so a reference that
//! breaks a scope rule surfaces as the expression language's generic
//! `Undefined variable: 'Service.X.p.port'.` — sometimes with a `Did you
//! mean` pointing at a *different* Service whose name is one edit away. The
//! author then has to work out which of the half-dozen scope rules they hit.
//!
//! This module runs after pass 8 has walked a document and rewrites those
//! messages when the referenced Service **is declared somewhere in the
//! document**: it classifies the reference site from the error's path, finds
//! the declaration, and states the rule that keeps it out of scope. A name
//! that is declared nowhere keeps the generic message (with its suggestion),
//! since a typo is then the likeliest cause. Only the sentence after
//! `Failed to parse interpolation expression at [s, e]. ` (or after
//! `Invalid expression in let binding 'x': `) changes; the path, the
//! expression source and caret lines that follow it are untouched.
//!
//! The rules, in the order they are tried for a declared `Service.<svc>`:
//!
//! 1. `Task.*` inside a Service (its actions, `variables`, `let`,
//!    embedded files): *Task.\* is not available
//!    within a Service.* (§9 item 3.)
//! 2. The site is a job-creation-time field (`hostRequirements`, a `let`
//!    list, a `parameterSpace` range, an action `timeout` /
//!    `notifyPeriodInSeconds`, a Service's `port` / `maxAttempts` / the
//!    four `<ServiceHealthCheck>` numeric fields): *Service.\* is not
//!    available in
//!    `<field>`: it is resolved at job creation, before any Service has an
//!    endpoint.* (§3.6.2, §9.7 item 1.)
//! 3. The site is an Environment whose `runScope` includes `SERVICE` (the
//!    default): *Environment 'E' is entered in Service Sessions (its
//!    runScope includes SERVICE) and may not reference Service.\*; declare
//!    runScope: [TASK] if it configures Tasks.* (§4 item 3.2.)
//! 4. `svc` is a Step Service of another Step (or of any Step, from a Job
//!    Environment or Job Service): *Service 'C' is a Step Service of step
//!    'S' and is not in scope in step 'T'.* (§9 scope item 4.)
//! 5. `svc` is later in the referencing Service's own list: *Service 'B' is
//!    declared later in jobServices than 'A'; a Service may reference only
//!    itself and earlier Services.* (§9 scope items 1–2.)
//! 6. The port is not declared: *Service 'S' has no port 'mian'; declared
//!    ports: main.*
//! 7. `bindAddress` outside the declaring Service: *Service.P.main.bindAddress
//!    is available only within the Service 'P' itself; use connectAddress to
//!    reach it from elsewhere.* (§7.3.1.)
//!
//! Anything else — an unknown value name after a declared port, a reference
//! with too few components, `Service.File.*` — keeps the generic message.

use crate::error::{PathElement, ValidationError, ValidationErrors};
use crate::template::{Environment, EnvironmentTemplate, JobTemplate, RunScope, Service};

/// Where a Service is declared in the document.
#[derive(Clone, Copy)]
enum DeclSite<'a> {
    /// `jobServices[k]`, or an Environment Template's `services[k]`.
    Job { index: usize },
    /// `steps[step] -> stepServices[k]`.
    Step {
        step: usize,
        step_name: &'a str,
        index: usize,
    },
}

/// One Service declaration of the document.
struct Decl<'a> {
    service: &'a Service,
    site: DeclSite<'a>,
}

/// The kind of entity a format string belongs to, read off the error path.
enum Site<'a> {
    /// A Step's `script` (its Tasks' scope).
    Task { step: usize, step_name: &'a str },
    /// A Service's own session-scope fields: `variables` and `script`.
    Service {
        decl: DeclSite<'a>,
        service_name: &'a str,
    },
    /// A Job Environment (`jobEnvironments[i]`, or an Environment
    /// Template's `environment`).
    JobEnvironment(&'a Environment),
    /// `steps[i] -> stepEnvironments[j]`.
    StepEnvironment {
        step: usize,
        step_name: &'a str,
        env: &'a Environment,
    },
    /// A field resolved at job creation, named for the message.
    JobCreation(&'static str),
}

/// The document's declarations and lookups, over either template kind.
struct Document<'a> {
    decls: Vec<Decl<'a>>,
    job_list: &'static str,
    /// Looks a path up to its reference site.
    job_template: Option<&'a JobTemplate>,
    env_template: Option<&'a EnvironmentTemplate>,
}

impl<'a> Document<'a> {
    fn for_job_template(jt: &'a JobTemplate) -> Self {
        let mut decls = Vec::new();
        for (index, service) in jt.job_services.iter().flatten().enumerate() {
            decls.push(Decl {
                service,
                site: DeclSite::Job { index },
            });
        }
        for (step, st) in jt.steps.iter().enumerate() {
            for (index, service) in st.step_services.iter().flatten().enumerate() {
                decls.push(Decl {
                    service,
                    site: DeclSite::Step {
                        step,
                        step_name: &st.name,
                        index,
                    },
                });
            }
        }
        Self {
            decls,
            job_list: "jobServices",
            job_template: Some(jt),
            env_template: None,
        }
    }

    fn for_environment_template(et: &'a EnvironmentTemplate) -> Self {
        let decls = et
            .services
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, service)| Decl {
                service,
                site: DeclSite::Job { index },
            })
            .collect();
        Self {
            decls,
            job_list: "services",
            job_template: None,
            env_template: Some(et),
        }
    }

    /// The declarations named `name`, in document order.
    fn declared(&self, name: &str) -> Vec<&Decl<'a>> {
        self.decls
            .iter()
            .filter(|d| d.service.name == name)
            .collect()
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
                    [Field(g), Index(k), ..] if g == "stepServices" => {
                        let svc = step.step_services.as_ref()?.get(*k)?;
                        Some(Site::Service {
                            decl: DeclSite::Step {
                                step: *i,
                                step_name: &step.name,
                                index: *k,
                            },
                            service_name: &svc.name,
                        })
                    }
                    [Field(g), Index(j), ..] if g == "stepEnvironments" => {
                        let env = step.step_environments.as_ref()?.get(*j)?;
                        Some(Site::StepEnvironment {
                            step: *i,
                            step_name: &step.name,
                            env,
                        })
                    }
                    [Field(g), ..] if g == "script" => Some(Site::Task {
                        step: *i,
                        step_name: &step.name,
                    }),
                    _ => None,
                }
            }
            [Field(f), Index(k), ..] if f == "jobServices" => {
                let svc = self.job_template?.job_services.as_ref()?.get(*k)?;
                Some(Site::Service {
                    decl: DeclSite::Job { index: *k },
                    service_name: &svc.name,
                })
            }
            [Field(f), Index(k), ..] if f == "services" => {
                let svc = self.env_template?.services.as_ref()?.get(*k)?;
                Some(Site::Service {
                    decl: DeclSite::Job { index: *k },
                    service_name: &svc.name,
                })
            }
            [Field(f), Index(i), ..] if f == "jobEnvironments" => {
                let env = self.job_template?.job_environments.as_ref()?.get(*i)?;
                Some(Site::JobEnvironment(env))
            }
            [Field(f), ..] if f == "environment" => Some(Site::JobEnvironment(
                self.env_template?.environment.as_ref()?,
            )),
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
    // The leaf decides for the `@fmtstring` scalars; `hostRequirements`,
    // `let` and `parameterSpace` anywhere in the path.
    if fields.contains(&"hostRequirements") {
        return Some("hostRequirements");
    }
    if fields.contains(&"parameterSpace") {
        return Some("a parameterSpace range");
    }
    if fields.contains(&"let") {
        return Some("a let binding");
    }
    match fields.last().copied() {
        Some("timeout") => Some("timeout"),
        Some("notifyPeriodInSeconds") => Some("notifyPeriodInSeconds"),
        Some("port") if fields.contains(&"ports") => Some("port"),
        Some("readinessIntervalSeconds") => Some("readinessIntervalSeconds"),
        Some("readyTimeoutSeconds") => Some("readyTimeoutSeconds"),
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
fn refine(doc: &Document<'_>, errors: &mut ValidationErrors, from: usize) {
    for err in errors.errors.iter_mut().skip(from) {
        let Some((start, end, name)) = undefined_variable_span(&err.message) else {
            continue;
        };
        let Some(site) = doc.site(&err.path) else {
            continue;
        };
        let Some(reason) = reason(doc, &site, name) else {
            continue;
        };
        rewrite(err, start, end, &reason);
    }
}

/// The scope rule `name` breaks at `site`, if this module states one.
fn reason(doc: &Document<'_>, site: &Site<'_>, name: &str) -> Option<String> {
    if name.starts_with("Task.") {
        return match site {
            Site::Service { .. } => Some("Task.* is not available within a Service.".to_string()),
            _ => None,
        };
    }
    let mut parts = name.strip_prefix("Service.")?.splitn(3, '.');
    let svc = parts.next()?;
    let port = parts.next();
    let value = parts.next();
    let decls = doc.declared(svc);
    let first = decls.first()?;

    // Rule 2: job-creation fields never see Service.*.
    if let Site::JobCreation(field) = site {
        return Some(format!(
            "Service.* is not available in {field}: it is resolved at job creation, before any \
             Service has an endpoint."
        ));
    }

    // Rule 3: an Environment entered in Service Sessions.
    let env = match site {
        Site::JobEnvironment(env) => Some(*env),
        Site::StepEnvironment { env, .. } => Some(*env),
        Site::Task { .. } | Site::Service { .. } | Site::JobCreation(_) => None,
    };
    if let Some(env) = env {
        if env.runs_in(RunScope::Service) {
            return Some(format!(
                "Environment '{}' is entered in Service Sessions (its runScope includes SERVICE) \
                 and may not reference Service.*; declare runScope: [TASK] if it configures Tasks.",
                env.name
            ));
        }
    }

    // Is any declaration of `svc` in scope here (by name)?
    let same_list = |d: &Decl<'_>, decl: &DeclSite<'_>| -> Option<(usize, usize)> {
        match (d.site, decl) {
            (DeclSite::Job { index: k }, DeclSite::Job { index }) => Some((k, *index)),
            (
                DeclSite::Step {
                    step: s, index: k, ..
                },
                DeclSite::Step { step, index, .. },
            ) if s == *step => Some((k, *index)),
            _ => None,
        }
    };
    let in_scope = decls.iter().find(|d| match (d.site, site) {
        // A Job Service is in scope everywhere but in a Job Service before it.
        (DeclSite::Job { .. }, Site::Service { decl, .. }) => match same_list(d, decl) {
            Some((k, index)) => k <= index,
            None => true,
        },
        (DeclSite::Job { .. }, _) => true,
        // A Step Service is in scope in its Step's Tasks and Step
        // Environments, and in the Step Services at or after it.
        (DeclSite::Step { .. }, Site::Service { decl, .. }) => {
            matches!(same_list(d, decl), Some((k, index)) if k <= index)
        }
        (DeclSite::Step { step: s, .. }, Site::Task { step, .. }) => s == *step,
        (DeclSite::Step { step: s, .. }, Site::StepEnvironment { step, .. }) => s == *step,
        (DeclSite::Step { .. }, Site::JobEnvironment(_) | Site::JobCreation(_)) => false,
    });

    let Some(decl) = in_scope else {
        // Rules 4 and 5: declared, but not here.
        if let Site::Service {
            decl: here,
            service_name,
            ..
        } = site
        {
            // Rule 5: later in the referencing Service's own list.
            if decls
                .iter()
                .any(|d| matches!(same_list(d, here), Some((k, index)) if k > index))
            {
                let list = match here {
                    DeclSite::Job { .. } => doc.job_list,
                    DeclSite::Step { .. } => "stepServices",
                };
                return Some(format!(
                    "Service '{svc}' is declared later in {list} than '{service_name}'; a Service \
                     may reference only itself and earlier Services."
                ));
            }
        }
        // Rule 4: a Step Service seen from outside its Step.
        let DeclSite::Step { step_name, .. } = first.site else {
            return None;
        };
        let here = match site {
            Site::Task { step_name, .. } | Site::StepEnvironment { step_name, .. } => {
                format!("step '{step_name}'")
            }
            Site::JobEnvironment(env) => format!("Job Environment '{}'", env.name),
            Site::Service { service_name, .. } => format!("Service '{service_name}'"),
            Site::JobCreation(field) => field.to_string(),
        };
        return Some(format!(
            "Service '{svc}' is a Step Service of step '{step_name}' and is not in scope in {here}."
        ));
    };

    // Rule 6: the port.
    let port = port?;
    let declared_ports: Vec<&str> = decl.service.ports.iter().map(|p| p.name.as_str()).collect();
    if !declared_ports.contains(&port) {
        return Some(format!(
            "Service '{svc}' has no port '{port}'; declared ports: {}.",
            declared_ports.join(", ")
        ));
    }

    // Rule 7: bindAddress outside the Service itself.
    if value == Some("bindAddress") {
        let within = matches!(site, Site::Service { service_name, .. } if *service_name == svc);
        if !within {
            return Some(format!(
                "Service.{svc}.{port}.bindAddress is available only within the Service '{svc}' \
                 itself; use connectAddress to reach it from elsewhere."
            ));
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
pub(crate) fn refine_job_template(jt: &JobTemplate, errors: &mut ValidationErrors, from: usize) {
    refine(&Document::for_job_template(jt), errors, from);
}

/// As [`refine_job_template`], for an Environment Template.
pub(crate) fn refine_environment_template(
    et: &EnvironmentTemplate,
    errors: &mut ValidationErrors,
    from: usize,
) {
    refine(&Document::for_environment_template(et), errors, from);
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
            job_creation_field(&p(&["jobServices", "let"])),
            Some("a let binding")
        );
        assert_eq!(
            job_creation_field(&p(&["steps", "script", "actions", "onRun", "timeout"])),
            Some("timeout")
        );
        assert_eq!(
            job_creation_field(&[
                Field("jobServices".into()),
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
    }
}
