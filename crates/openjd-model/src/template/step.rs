// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Step types per spec §3.

use super::actions::{Action, CancelationMode, StepActions};
use super::constrained_strings::Description;
use super::environment::{EmbeddedFile, Environment};
use super::host_requirements::HostRequirements;
use super::task_parameters::StepParameterSpaceDefinition;
use crate::format_string::FormatString;
use serde::Deserialize;

/// SimpleAction syntax sugar (FEATURE_BUNDLE_1).
/// Allows specifying a script interpreter directly instead of a full StepScript.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleAction {
    /// Let bindings evaluated once per task (requires EXPR extension).
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    /// The script content to execute. Required.
    pub script: String,
    /// Additional arguments to pass to the interpreter.
    pub args: Option<Vec<FormatString>>,
    /// Maximum allowed runtime in seconds.
    pub timeout: Option<FormatString>,
    /// How to cancel the action.
    pub cancelation: Option<CancelationMode>,
}

/// §3 StepTemplate
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepTemplate {
    pub name: String,
    pub description: Option<Description>,
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    pub dependencies: Option<Vec<StepDependency>>,
    pub step_environments: Option<Vec<Environment>>,
    pub host_requirements: Option<HostRequirements>,
    pub parameter_space: Option<StepParameterSpaceDefinition>,
    pub script: Option<StepScript>,
    // SimpleAction syntax sugar (§3.5, FEATURE_BUNDLE_1)
    pub bash: Option<SimpleAction>,
    pub python: Option<SimpleAction>,
    pub cmd: Option<SimpleAction>,
    pub powershell: Option<SimpleAction>,
    pub node: Option<SimpleAction>,
}

impl StepTemplate {
    /// De-sugar SimpleAction syntax into equivalent StepScript.
    /// If the step already has a `script` field, returns `Ok(Some(clone))`.
    /// If it uses a SimpleAction (bash/python/cmd/powershell/node), transforms
    /// it into a StepScript with an embedded file and onRun action.
    /// Returns `Err` if the SimpleAction script contains malformed format string syntax.
    pub fn resolve_syntax_sugar(&self) -> Result<Option<StepScript>, crate::ModelError> {
        if let Some(script) = &self.script {
            return Ok(Some(script.clone()));
        }

        let interpreters: &[(&str, &str, &[&str], Option<&SimpleAction>)] = &[
            ("python", ".py", &[], self.python.as_ref()),
            ("bash", ".sh", &[], self.bash.as_ref()),
            ("cmd", ".bat", &["/C"], self.cmd.as_ref()),
            ("powershell", ".ps1", &["-File"], self.powershell.as_ref()),
            ("node", ".js", &[], self.node.as_ref()),
        ];

        for &(command, ext, arg_prefix, sa_opt) in interpreters {
            let Some(sa) = sa_opt else { continue };

            let safe_name: String = self
                .name
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .take(200)
                .collect();
            let safe_name = if safe_name.starts_with(|c: char| c.is_ascii_digit()) {
                format!("_{safe_name}")
            } else {
                safe_name
            };
            let embedded_name = format!("{safe_name}_script");
            let filename = format!("{embedded_name}{ext}");
            let file_ref = format!("{{{{Task.File.{embedded_name}}}}}");

            let mut args = Vec::new();
            for prefix_arg in arg_prefix {
                args.push(FormatString::new(prefix_arg).unwrap());
            }
            args.push(FormatString::new(&file_ref).unwrap());
            if let Some(user_args) = &sa.args {
                args.extend(user_args.iter().cloned());
            }

            return Ok(Some(StepScript {
                let_bindings: sa.let_bindings.clone(),
                actions: StepActions {
                    on_run: Action {
                        command: FormatString::new(command).unwrap(),
                        args: Some(args),
                        cancelation: sa.cancelation.clone(),
                        timeout: sa.timeout.clone(),
                    },
                },
                embedded_files: Some(vec![EmbeddedFile {
                    name: embedded_name,
                    file_type: crate::types::FileType::Text,
                    filename: Some(filename),
                    data: Some(FormatString::new(&sa.script).map_err(|e| {
                        crate::ModelError::DecodeValidation(format!(
                            "SimpleAction script format string error: {e}"
                        ))
                    })?),
                    runnable: Some(true),
                    end_of_line: None,
                }]),
            }));
        }

        Ok(None)
    }
}

/// What a `<StepDependency>` names (§3.2): a Step of the same Job Template
/// (the `dependsOn` key), or, with the `SERVICE` extension, a Service the
/// Job Template declares in `services` or requires in `requiresServices`
/// (the `service` key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyTarget<'a> {
    /// A Step, by name; satisfied when the Step has completed.
    Step(&'a str),
    /// A Service, by name; satisfied when the Service is READY.
    Service(&'a str),
}

impl<'a> DependencyTarget<'a> {
    /// The Step name, for a Step target.
    #[must_use]
    pub fn step(self) -> Option<&'a str> {
        match self {
            Self::Step(name) => Some(name),
            Self::Service(_) => None,
        }
    }

    /// The Service name, for a Service target.
    #[must_use]
    pub fn service(self) -> Option<&'a str> {
        match self {
            Self::Step(_) => None,
            Self::Service(name) => Some(name),
        }
    }
}

/// `dependsOn: X` or `service: X` — the entry as a template writes it.
impl std::fmt::Display for DependencyTarget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Step(name) => write!(f, "dependsOn: {name}"),
            Self::Service(name) => write!(f, "service: {name}"),
        }
    }
}

/// §3.2 StepDependency: one entry of a Step's, a Service's, or a Job
/// Environment's `dependencies`. It is one of `dependsOn: <StepName>` or,
/// with the `SERVICE` extension, `service: <ServiceName>`; exactly one of
/// the two keys must be present (an entry giving both or neither is a
/// validation error, reported at the entry's path). Without `SERVICE` the
/// `service` key is gated like every other `SERVICE` property. See
/// [`target`](Self::target).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepDependency {
    /// The name of a Step of the same Job Template.
    #[serde(default)]
    pub depends_on: Option<String>,
    /// The name of a Service the Job Template declares in `services` or
    /// requires in `requiresServices` (`SERVICE` extension).
    #[serde(default)]
    pub service: Option<String>,
}

impl StepDependency {
    /// An entry naming the Step `name`.
    #[must_use]
    pub fn on_step(name: impl Into<String>) -> Self {
        Self {
            depends_on: Some(name.into()),
            service: None,
        }
    }

    /// An entry naming the Service `name`.
    #[must_use]
    pub fn on_service(name: impl Into<String>) -> Self {
        Self {
            depends_on: None,
            service: Some(name.into()),
        }
    }

    /// What this entry names, or `None` when it gives both keys or neither
    /// (a validation error; such an entry is no dependency).
    #[must_use]
    pub fn target(&self) -> Option<DependencyTarget<'_>> {
        match (&self.depends_on, &self.service) {
            (Some(step), None) => Some(DependencyTarget::Step(step)),
            (None, Some(service)) => Some(DependencyTarget::Service(service)),
            _ => None,
        }
    }

    /// The Step this entry names, if it is a well-formed `dependsOn` entry.
    #[must_use]
    pub fn step(&self) -> Option<&str> {
        self.target().and_then(DependencyTarget::step)
    }

    /// The Service this entry names, if it is a well-formed `service` entry.
    #[must_use]
    pub fn service(&self) -> Option<&str> {
        self.target().and_then(DependencyTarget::service)
    }

    /// True when the entry is well-formed: exactly one of the two keys.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.target().is_some()
    }

    /// The entry as written (`dependsOn: X`, `service: X`), or a description
    /// of a malformed one, for messages.
    #[must_use]
    pub fn describe(&self) -> String {
        match (&self.depends_on, &self.service) {
            (Some(step), Some(service)) => format!("dependsOn: {step}, service: {service}"),
            (None, None) => "{}".to_string(),
            _ => self.target().expect("one key").to_string(),
        }
    }
}

/// True when `dependencies` lists Service `name` with the `service` key.
#[must_use]
pub fn lists_service(dependencies: Option<&[StepDependency]>, name: &str) -> bool {
    dependencies
        .into_iter()
        .flatten()
        .any(|d| d.service() == Some(name))
}

/// The Service names `dependencies` lists with the `service` key, in list
/// order.
pub fn listed_service_names(
    dependencies: Option<&[StepDependency]>,
) -> impl Iterator<Item = &str> + '_ {
    dependencies
        .into_iter()
        .flatten()
        .filter_map(|d| d.service())
}

/// The Step names `dependencies` lists with the `dependsOn` key, in list
/// order (`service` entries and malformed entries skipped).
pub fn listed_step_names(
    dependencies: Option<&[StepDependency]>,
) -> impl Iterator<Item = &str> + '_ {
    dependencies.into_iter().flatten().filter_map(|d| d.step())
}

/// §3.5 StepScript
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepScript {
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

#[cfg(test)]
mod tests {
    use super::{
        listed_service_names, listed_step_names, lists_service, DependencyTarget, StepDependency,
        StepTemplate,
    };

    #[test]
    fn dependency_target_accessors_and_display() {
        assert_eq!(DependencyTarget::Service("C").service(), Some("C"));
        assert_eq!(DependencyTarget::Service("C").step(), None);
        assert_eq!(DependencyTarget::Step("S").step(), Some("S"));
        assert_eq!(DependencyTarget::Step("S").service(), None);
        assert_eq!(DependencyTarget::Step("S").to_string(), "dependsOn: S");
        assert_eq!(DependencyTarget::Service("C").to_string(), "service: C");
    }

    #[test]
    fn step_dependency_is_one_of_two_keys() {
        let step = StepDependency::on_step("Render");
        assert_eq!(step.target(), Some(DependencyTarget::Step("Render")));
        assert_eq!(step.step(), Some("Render"));
        assert_eq!(step.service(), None);
        assert!(step.is_well_formed());
        assert_eq!(step.describe(), "dependsOn: Render");
        let svc = StepDependency::on_service("Cache");
        assert_eq!(svc.target(), Some(DependencyTarget::Service("Cache")));
        assert_eq!(svc.service(), Some("Cache"));
        assert_eq!(svc.step(), None);
        assert_eq!(svc.describe(), "service: Cache");
        // Both keys, or neither: no target.
        let both = StepDependency {
            depends_on: Some("Render".into()),
            service: Some("Cache".into()),
        };
        assert_eq!(both.target(), None);
        assert!(!both.is_well_formed());
        assert_eq!(both.describe(), "dependsOn: Render, service: Cache");
        let neither = StepDependency {
            depends_on: None,
            service: None,
        };
        assert_eq!(neither.target(), None);
        assert_eq!(neither.describe(), "{}");
        // Decoding: each key is optional; an unknown key is rejected.
        let decoded: StepDependency = serde_saphyr::from_str("service: Cache").unwrap();
        assert_eq!(decoded, StepDependency::on_service("Cache"));
        let decoded: StepDependency = serde_saphyr::from_str("{}").unwrap();
        assert_eq!(decoded.target(), None);
        assert!(serde_saphyr::from_str::<StepDependency>("dependsOnService: X").is_err());
    }

    #[test]
    fn listed_names_split_by_kind() {
        let deps = vec![
            StepDependency::on_step("A"),
            StepDependency::on_service("X"),
            StepDependency::on_step("B"),
            StepDependency::on_service("Y"),
            StepDependency {
                depends_on: Some("C".into()),
                service: Some("Z".into()),
            },
        ];
        assert!(lists_service(Some(&deps), "X"));
        assert!(!lists_service(Some(&deps), "A"));
        assert!(!lists_service(Some(&deps), "Z"));
        assert!(!lists_service(None, "X"));
        assert_eq!(
            listed_service_names(Some(&deps)).collect::<Vec<_>>(),
            vec!["X", "Y"]
        );
        assert_eq!(
            listed_step_names(Some(&deps)).collect::<Vec<_>>(),
            vec!["A", "B"]
        );
        assert_eq!(deps[1].target(), Some(DependencyTarget::Service("X")));
    }

    #[test]
    fn resolve_syntax_sugar_returns_error_for_malformed_format_string() {
        let step: StepTemplate = serde_saphyr::from_str(
            r#"
            name: TestStep
            bash:
              script: "echo '{{broken'"
            "#,
        )
        .unwrap();

        let result = step.resolve_syntax_sugar();
        assert!(
            result.is_err(),
            "resolve_syntax_sugar should return Err for malformed format string"
        );
    }

    #[test]
    fn resolve_syntax_sugar_ok_for_valid_script() {
        let step: StepTemplate = serde_saphyr::from_str(
            r#"
            name: TestStep
            bash:
              script: "echo hello"
            "#,
        )
        .unwrap();

        let result = step.resolve_syntax_sugar();
        assert!(result.is_ok(), "valid script should succeed");
        assert!(
            result.unwrap().is_some(),
            "bash step should produce a StepScript"
        );
    }
}
