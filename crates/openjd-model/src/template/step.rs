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

/// The literal prefix that makes a `dependsOn` value name a Service rather
/// than a Step under the `SERVICE` extension (§3.2, RFC 0009).
pub const SERVICE_DEPENDENCY_PREFIX: &str = "service:";

/// What a `dependsOn` value names (§3.2): a Step of the same Job Template,
/// or, with the `SERVICE` extension, a Service the Job Template declares in
/// `services` or requires in `requiresServices`.
///
/// Parsed from the raw string by [`DependencyTarget::parse`]: the `service:`
/// prefix is recognized only when `SERVICE` is declared. In any other
/// template `service:Cache` is an ordinary Step name (§3.1 constraint 4
/// forbids `:` in a Step name only under `SERVICE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyTarget<'a> {
    /// A Step, by name; satisfied when the Step has completed.
    Step(&'a str),
    /// A Service, by name (without the `service:` prefix); satisfied when the
    /// Service is READY.
    Service(&'a str),
}

impl<'a> DependencyTarget<'a> {
    /// Classify `depends_on`: with `service_active`, a value beginning
    /// `service:` names the Service after the prefix; otherwise, and for any
    /// other value, the whole string is a Step name.
    #[must_use]
    pub fn parse(depends_on: &'a str, service_active: bool) -> Self {
        match depends_on.strip_prefix(SERVICE_DEPENDENCY_PREFIX) {
            Some(name) if service_active => Self::Service(name),
            _ => Self::Step(depends_on),
        }
    }

    /// The Step name, for a Step target.
    #[must_use]
    pub fn step(self) -> Option<&'a str> {
        match self {
            Self::Step(name) => Some(name),
            Self::Service(_) => None,
        }
    }

    /// The Service name (without the prefix), for a Service target.
    #[must_use]
    pub fn service(self) -> Option<&'a str> {
        match self {
            Self::Step(_) => None,
            Self::Service(name) => Some(name),
        }
    }
}

/// §3.2 StepDependency: one entry of a Step's or, with `SERVICE`, a
/// Service's `dependencies`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepDependency {
    /// A Step name, or `service:<ServiceName>` under the `SERVICE`
    /// extension. See [`DependencyTarget`].
    pub depends_on: String,
}

impl StepDependency {
    /// What this entry names; see [`DependencyTarget::parse`].
    #[must_use]
    pub fn target(&self, service_active: bool) -> DependencyTarget<'_> {
        DependencyTarget::parse(&self.depends_on, service_active)
    }
}

/// True when `dependencies` lists `service:<name>` (with `SERVICE`
/// declared, which is the only context in which a Service exists).
#[must_use]
pub fn lists_service(dependencies: Option<&[StepDependency]>, name: &str) -> bool {
    dependencies
        .into_iter()
        .flatten()
        .any(|d| d.target(true) == DependencyTarget::Service(name))
}

/// The Service names `dependencies` lists as `service:<name>`, in list
/// order (with `SERVICE` declared).
pub fn listed_service_names(
    dependencies: Option<&[StepDependency]>,
) -> impl Iterator<Item = &str> + '_ {
    dependencies
        .into_iter()
        .flatten()
        .filter_map(|d| d.target(true).service())
}

/// The Step names `dependencies` lists, in list order, under `SERVICE`
/// (`service:` entries skipped) or without it (every entry).
pub fn listed_step_names(
    dependencies: Option<&[StepDependency]>,
    service_active: bool,
) -> impl Iterator<Item = &str> + '_ {
    dependencies
        .into_iter()
        .flatten()
        .filter_map(move |d| d.target(service_active).step())
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
    fn dependency_target_parsing_is_gated_on_service() {
        assert_eq!(
            DependencyTarget::parse("service:Cache", true),
            DependencyTarget::Service("Cache")
        );
        assert_eq!(
            DependencyTarget::parse("service:Cache", false),
            DependencyTarget::Step("service:Cache")
        );
        assert_eq!(
            DependencyTarget::parse("Render", true),
            DependencyTarget::Step("Render")
        );
        assert_eq!(DependencyTarget::Service("C").service(), Some("C"));
        assert_eq!(DependencyTarget::Service("C").step(), None);
        assert_eq!(DependencyTarget::Step("S").step(), Some("S"));
        assert_eq!(DependencyTarget::Step("S").service(), None);
        // An empty name after the prefix is still a Service target; the
        // validator reports it as unknown.
        assert_eq!(
            DependencyTarget::parse("service:", true),
            DependencyTarget::Service("")
        );
    }

    #[test]
    fn listed_names_split_by_kind() {
        let deps: Vec<StepDependency> = ["A", "service:X", "B", "service:Y"]
            .iter()
            .map(|d| StepDependency {
                depends_on: d.to_string(),
            })
            .collect();
        assert!(lists_service(Some(&deps), "X"));
        assert!(!lists_service(Some(&deps), "A"));
        assert!(!lists_service(None, "X"));
        assert_eq!(
            listed_service_names(Some(&deps)).collect::<Vec<_>>(),
            vec!["X", "Y"]
        );
        assert_eq!(
            listed_step_names(Some(&deps), true).collect::<Vec<_>>(),
            vec!["A", "B"]
        );
        assert_eq!(
            listed_step_names(Some(&deps), false).collect::<Vec<_>>(),
            vec!["A", "service:X", "B", "service:Y"]
        );
        assert_eq!(deps[1].target(true), DependencyTarget::Service("X"));
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
