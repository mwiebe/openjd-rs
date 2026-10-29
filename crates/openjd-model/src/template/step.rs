// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Step types per spec §3.

use super::actions::{Action, CancelationMode, StepActions};
use super::constrained_strings::Description;
use super::env_parameters::EnvironmentParameterValueMap;
use super::environment::{EmbeddedFile, Environment};
use super::expr_field::ExprField;
use super::host_requirements::HostRequirements;
use super::task_parameters::StepParameterSpaceDefinition;
use crate::error::PathElement;
use crate::format_string::FormatString;
use serde::Deserialize;

/// SimpleAction syntax sugar (FEATURE_BUNDLE_1, Template Schemas §8).
/// Allows specifying a script interpreter directly instead of a full StepScript.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleAction {
    /// Let bindings evaluated once per task (requires EXPR extension).
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    /// The script content to execute. Required. A `<DataString>`
    /// (`@fmtstring[host]`), so malformed `{{ ... }}` syntax is rejected at
    /// parse time exactly like every other format-string field.
    pub script: FormatString,
    /// Additional arguments to pass to the interpreter.
    pub args: Option<Vec<FormatString>>,
    /// Maximum allowed runtime in seconds.
    pub timeout: Option<FormatString>,
    /// How to cancel the action.
    pub cancelation: Option<CancelationMode>,
}

/// The interpreter key a [`SimpleAction`] was written under (§8).
///
/// Each kind fixes the desugared `command`, the embedded file's extension,
/// and the interpreter arguments that precede the generated file reference
/// in `args`. It also knows how to map a validation-error path on the
/// desugared [`StepScript`] back onto the field the author actually wrote,
/// so diagnostics never name a node that does not exist in the template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimpleActionKind {
    /// `python:` — `command: python`, `.py`.
    Python,
    /// `bash:` — `command: bash`, `.sh`.
    Bash,
    /// `cmd:` — `command: cmd`, `.bat`, args prefixed with `/C`.
    Cmd,
    /// `powershell:` — `command: powershell`, `.ps1`, args prefixed with `-File`.
    Powershell,
    /// `node:` — `command: node`, `.js`.
    Node,
}

impl SimpleActionKind {
    /// Every kind, in the order [`StepTemplate::resolve_syntax_sugar`]
    /// considers them.
    pub const ALL: [SimpleActionKind; 5] = [
        SimpleActionKind::Python,
        SimpleActionKind::Bash,
        SimpleActionKind::Cmd,
        SimpleActionKind::Powershell,
        SimpleActionKind::Node,
    ];

    /// The step field name this kind is written under — also the desugared
    /// `command` (§8 requires the lowercase interpreter name on `PATH`).
    #[must_use]
    pub fn field_name(self) -> &'static str {
        match self {
            SimpleActionKind::Python => "python",
            SimpleActionKind::Bash => "bash",
            SimpleActionKind::Cmd => "cmd",
            SimpleActionKind::Powershell => "powershell",
            SimpleActionKind::Node => "node",
        }
    }

    /// File extension of the generated embedded file.
    #[must_use]
    pub fn file_extension(self) -> &'static str {
        match self {
            SimpleActionKind::Python => ".py",
            SimpleActionKind::Bash => ".sh",
            SimpleActionKind::Cmd => ".bat",
            SimpleActionKind::Powershell => ".ps1",
            SimpleActionKind::Node => ".js",
        }
    }

    /// Interpreter arguments inserted before the generated file reference.
    #[must_use]
    pub fn arg_prefix(self) -> &'static [&'static str] {
        match self {
            SimpleActionKind::Cmd => &["/C"],
            SimpleActionKind::Powershell => &["-File"],
            _ => &[],
        }
    }

    /// Number of synthesized leading `args` entries in the desugared action:
    /// the interpreter prefix plus the `{{Task.File.<name>}}` reference.
    /// The author's own `args[k]` lands at desugared `args[k + offset]`.
    #[must_use]
    pub fn synthetic_arg_count(self) -> usize {
        self.arg_prefix().len() + 1
    }

    /// Expand `sa` into the equivalent [`StepScript`] per §8. `step_name` is
    /// sanitized into the generated embedded file's name: every character
    /// outside `[A-Za-z0-9]` becomes `_` (as in `openjd-model-for-python`),
    /// truncated to 200 characters, `_`-prefixed if it would start with a
    /// digit, suffixed `_script`. The result is always a valid identifier,
    /// so the `{{Task.File.<name>}}` reference always parses.
    #[must_use]
    pub fn desugar(self, step_name: &str, sa: &SimpleAction) -> StepScript {
        let safe_name: String = step_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(200)
            .collect();
        let safe_name = if safe_name.starts_with(|c: char| c.is_ascii_digit()) {
            format!("_{safe_name}")
        } else {
            safe_name
        };
        let embedded_name = format!("{safe_name}_script");
        let filename = format!("{embedded_name}{}", self.file_extension());
        let file_ref = format!("{{{{Task.File.{embedded_name}}}}}");

        let mut args = Vec::new();
        for prefix_arg in self.arg_prefix() {
            args.push(FormatString::new(prefix_arg).expect("literal arg prefix"));
        }
        args.push(FormatString::new(&file_ref).expect("generated Task.File reference"));
        if let Some(user_args) = &sa.args {
            args.extend(user_args.iter().cloned());
        }

        StepScript {
            let_bindings: sa.let_bindings.clone(),
            actions: StepActions {
                on_run: Action {
                    command: FormatString::new(self.field_name()).expect("literal command"),
                    args: Some(args),
                    cancelation: sa.cancelation.clone(),
                    timeout: sa.timeout.clone(),
                },
            },
            embedded_files: Some(vec![EmbeddedFile {
                name: embedded_name,
                file_type: crate::types::FileType::Text,
                filename: Some(filename),
                data: Some(sa.script.clone()),
                runnable: Some(true),
                end_of_line: None,
            }]),
        }
    }

    /// Map a validation-error path *relative to the desugared `script` node*
    /// back onto the corresponding path relative to the `SimpleAction` the
    /// author wrote. Validation runs on the desugared form (so the sugar and
    /// its expansion are checked by exactly the same code), but every
    /// diagnostic must point at a node that exists in the template:
    ///
    /// | desugared (relative to `script`)           | sugar (relative to `<kind>`) |
    /// |--------------------------------------------|------------------------------|
    /// | `let[...]`                                 | `let[...]`                   |
    /// | `actions -> onRun -> args`                 | `args`                       |
    /// | `actions -> onRun -> args[k]`, `k ≥ offset`| `args[k - offset]`           |
    /// | `actions -> onRun -> timeout`              | `timeout`                    |
    /// | `actions -> onRun -> cancelation ...`      | `cancelation ...`            |
    /// | `embeddedFiles[0] -> data`                 | `script`                     |
    /// | anything synthesized (`command`, the generated file's `name` / `filename`, the `Task.File` arg, `actions -> onRun` itself) | the `<kind>` field itself |
    ///
    /// where `offset` is [`Self::synthetic_arg_count`].
    #[must_use]
    pub fn remap_desugared_path(self, rel: &[PathElement]) -> Vec<PathElement> {
        use PathElement::{Field, Index};
        match rel {
            [Field(f), rest @ ..] if f == "let" => {
                let mut out = vec![Field("let".into())];
                out.extend_from_slice(rest);
                out
            }
            [Field(actions), Field(on_run), rest @ ..]
                if actions == "actions" && on_run == "onRun" =>
            {
                match rest {
                    [Field(f)] if f == "args" => vec![Field("args".into())],
                    [Field(f), Index(k), tail @ ..] if f == "args" => {
                        let offset = self.synthetic_arg_count();
                        if *k < offset {
                            // A synthesized interpreter argument — nothing
                            // the author wrote corresponds to it.
                            Vec::new()
                        } else {
                            let mut out = vec![Field("args".into()), Index(k - offset)];
                            out.extend_from_slice(tail);
                            out
                        }
                    }
                    [Field(f), tail @ ..] if f == "timeout" || f == "cancelation" => {
                        let mut out = vec![Field(f.clone())];
                        out.extend_from_slice(tail);
                        out
                    }
                    // `command` and the action node itself are synthesized.
                    _ => Vec::new(),
                }
            }
            [Field(files), Index(0), Field(data), tail @ ..]
                if files == "embeddedFiles" && data == "data" =>
            {
                let mut out = vec![Field("script".into())];
                out.extend_from_slice(tail);
                out
            }
            // The generated file's `name`/`filename`/`type`, the
            // `embeddedFiles` list itself, and anything unforeseen: point
            // at the sugar field as a whole rather than at a phantom node.
            _ => Vec::new(),
        }
    }
}

/// §3 StepTemplate
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
    /// The SimpleAction fields this step sets, paired with their kind, in
    /// [`SimpleActionKind::ALL`] order. A valid step has at most one (pass 7
    /// rejects more), but validation iterates them all so every field the
    /// author wrote is checked and reported at its own path.
    pub fn simple_actions(&self) -> impl Iterator<Item = (SimpleActionKind, &SimpleAction)> {
        SimpleActionKind::ALL
            .into_iter()
            .filter_map(move |kind| self.simple_action(kind).map(|sa| (kind, sa)))
    }

    /// The SimpleAction written under `kind`, if any.
    #[must_use]
    pub fn simple_action(&self, kind: SimpleActionKind) -> Option<&SimpleAction> {
        match kind {
            SimpleActionKind::Python => self.python.as_ref(),
            SimpleActionKind::Bash => self.bash.as_ref(),
            SimpleActionKind::Cmd => self.cmd.as_ref(),
            SimpleActionKind::Powershell => self.powershell.as_ref(),
            SimpleActionKind::Node => self.node.as_ref(),
        }
    }

    /// The step's script as a [`StepScript`]: a clone of `script` when
    /// present, otherwise the first SimpleAction field (in
    /// [`SimpleActionKind::ALL`] order) desugared per §8. `None` when the
    /// step has neither — a structural error pass 6 reports.
    #[must_use]
    pub fn resolve_syntax_sugar(&self) -> Option<StepScript> {
        if let Some(script) = &self.script {
            return Some(script.clone());
        }
        self.simple_actions()
            .next()
            .map(|(kind, sa)| kind.desugar(&self.name, sa))
    }
}

/// What a `<StepDependency>` names (§3.2): a Step of the same Job Template
/// (the `dependsOn` key or, with the `ENV_PARAMS` extension, its synonym
/// `step`) or, with `ENV_PARAMS`, an Environment that the Step's Sessions
/// enter (the `environment` key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyTarget<'a> {
    /// A Step, by name, and the key the entry wrote it under; satisfied
    /// when the Step has completed. The two keys mean the same thing
    /// (§3.2 item 1): the key is kept only so that messages can name what
    /// was written.
    Step(&'a str, StepKey),
    /// An Environment, by name: a `jobEnvironments` entry of the same Job
    /// Template or an external Environment. It does not delay scheduling.
    Environment(&'a str),
}

/// The key a Step-naming `<StepDependency>` entry is written with (§3.2
/// item 1). `step` is a synonym of `dependsOn`, available with the
/// `ENV_PARAMS` extension: every rule treats the two alike, and a `step`
/// entry and a `dependsOn` entry naming one Step are duplicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepKey {
    /// `dependsOn: <StepName>`.
    DependsOn,
    /// `step: <StepName>` (`ENV_PARAMS`).
    Step,
}

impl StepKey {
    /// The key as a template writes it: `dependsOn` or `step`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DependsOn => "dependsOn",
            Self::Step => "step",
        }
    }
}

impl<'a> DependencyTarget<'a> {
    /// The Step name, for a Step target, whichever key named it.
    #[must_use]
    pub fn step(self) -> Option<&'a str> {
        match self {
            Self::Step(name, _) => Some(name),
            Self::Environment(_) => None,
        }
    }

    /// The Environment name, for an Environment target.
    #[must_use]
    pub fn environment(self) -> Option<&'a str> {
        match self {
            Self::Step(..) => None,
            Self::Environment(name) => Some(name),
        }
    }
}

/// `dependsOn: X`, `step: X`, or `environment: X`, the entry as a template
/// writes it.
impl std::fmt::Display for DependencyTarget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Step(name, key) => write!(f, "{}: {name}", key.as_str()),
            Self::Environment(name) => write!(f, "environment: {name}"),
        }
    }
}

/// §3.2 StepDependency: one entry of a Step's `dependencies`.
///
/// It is one of `dependsOn: <StepName>` or, with the `ENV_PARAMS`
/// extension, its synonym `step: <StepName>`, or `environment:
/// <EnvironmentName>` with optional `required`, `enableIf`, and
/// `parameters`. Every key is optional here so that an entry with more
/// than one form, or none, decodes and template validation reports it at
/// the entry's path (§3.2 constraint 1). Use [`target`](Self::target) to
/// read which form an entry has.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct StepDependency {
    /// The name of a Step of the same Job Template.
    #[serde(default)]
    pub depends_on: Option<String>,
    /// The name of a Step of the same Job Template: the `ENV_PARAMS`
    /// synonym of [`depends_on`](Self::depends_on) (§3.2 item 1).
    #[serde(default)]
    pub step: Option<String>,
    /// The name of an Environment (`ENV_PARAMS`).
    #[serde(default)]
    pub environment: Option<String>,
    /// Whether the Environment must be present; absent means `true`
    /// (`ENV_PARAMS`). See [`is_required`](Self::is_required).
    #[serde(default)]
    pub required: Option<bool>,
    /// Whether the entry is in effect (`ENV_PARAMS`, §3.2 item 5): an
    /// expression field of type `bool` (§7.5), resolved when the Job is
    /// created. Absent means enabled; a disabled entry is as if
    /// absent. A present `null` decodes as [`ExprField::Null`], which
    /// template validation rejects.
    #[serde(default, deserialize_with = "super::expr_field::deserialize_enable_if")]
    pub enable_if: Option<ExprField>,
    /// The parameter values this Step supplies (`ENV_PARAMS`).
    #[serde(default)]
    pub parameters: Option<EnvironmentParameterValueMap>,
}

impl StepDependency {
    /// An entry naming the Step `name`, written `dependsOn: <name>`.
    #[must_use]
    pub fn on_step(name: impl Into<String>) -> Self {
        Self {
            depends_on: Some(name.into()),
            step: None,
            environment: None,
            required: None,
            enable_if: None,
            parameters: None,
        }
    }

    /// An entry naming the Environment `name`, with no `required` and no
    /// `parameters`.
    #[must_use]
    pub fn on_environment(name: impl Into<String>) -> Self {
        Self {
            depends_on: None,
            step: None,
            environment: Some(name.into()),
            required: None,
            enable_if: None,
            parameters: None,
        }
    }

    /// What this entry names, or `None` for an entry with more than one of
    /// `dependsOn`, `step`, and `environment` or with none, which template
    /// validation rejects (§3.2 constraint 1).
    #[must_use]
    pub fn target(&self) -> Option<DependencyTarget<'_>> {
        match (&self.depends_on, &self.step, &self.environment) {
            (Some(name), None, None) => Some(DependencyTarget::Step(name, StepKey::DependsOn)),
            (None, Some(name), None) => Some(DependencyTarget::Step(name, StepKey::Step)),
            (None, None, Some(env)) => Some(DependencyTarget::Environment(env)),
            _ => None,
        }
    }

    /// The Step this entry names, if it is a well-formed `dependsOn` or
    /// `step` entry. (The `step` field holds only a `step:` key as
    /// written, whether or not the entry is well-formed; this method reads
    /// either key.)
    #[must_use]
    pub fn step(&self) -> Option<&str> {
        self.target().and_then(DependencyTarget::step)
    }

    /// The Environment this entry names, if it is a well-formed
    /// `environment` entry. (The `environment` field holds the name as
    /// written, whether or not the entry is well-formed.)
    #[must_use]
    pub fn environment(&self) -> Option<&str> {
        self.target().and_then(DependencyTarget::environment)
    }

    /// True when the entry is well-formed: exactly one of `dependsOn`,
    /// `step`, and `environment`.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.target().is_some()
    }

    /// Whether the entry is enabled, when that is known before the Job is
    /// created: `Some(true)` when `enableIf` is absent or the boolean
    /// scalar `true`, `Some(false)` for the scalar `false`, and `None` for
    /// any other value, an expression included, even a constant one such
    /// as `"true"`, whose value is known only when the Job is created
    /// (§3.2, checked-at paragraph; §7.5 item 1).
    #[must_use]
    pub fn literal_enabled(&self) -> Option<bool> {
        match &self.enable_if {
            None => Some(true),
            Some(e) => e.as_bool(),
        }
    }

    /// `required` with its default applied (§3.2 item 3: absent means
    /// `true`).
    #[must_use]
    pub fn is_required(&self) -> bool {
        self.required.unwrap_or(true)
    }
}

/// §3.5 StepScript
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct StepScript {
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

#[cfg(test)]
mod tests {
    use super::{SimpleActionKind, StepTemplate};
    use crate::error::PathElement::{Field, Index};

    fn f(s: &str) -> crate::error::PathElement {
        Field(s.to_string())
    }

    #[test]
    fn malformed_script_format_string_is_rejected_at_parse_time() {
        // `script` is a `<DataString>` (@fmtstring[host]): brace mismatches
        // fail deserialization like every other format-string field, rather
        // than surviving to job creation.
        let result: Result<StepTemplate, _> = serde_saphyr::from_str(
            r#"
            name: TestStep
            bash:
              script: "echo '{{broken'"
            "#,
        );
        let err = result
            .expect_err("malformed script must not parse")
            .to_string();
        assert!(err.contains("Braces mismatch"), "got: {err}");
    }

    #[test]
    fn resolve_syntax_sugar_desugars_first_simple_action() {
        let step: StepTemplate = serde_saphyr::from_str(
            r#"
            name: Test Step
            powershell:
              script: "Write-Host hi"
              args: ["--flag"]
            "#,
        )
        .unwrap();

        let script = step
            .resolve_syntax_sugar()
            .expect("powershell step desugars");
        assert_eq!(script.actions.on_run.command.raw(), "powershell");
        let args: Vec<&str> = script
            .actions
            .on_run
            .args
            .as_ref()
            .unwrap()
            .iter()
            .map(|a| a.raw())
            .collect();
        assert_eq!(args, ["-File", "{{Task.File.Test_Step_script}}", "--flag"]);
        let file = &script.embedded_files.as_ref().unwrap()[0];
        assert_eq!(file.name, "Test_Step_script");
        assert_eq!(file.filename.as_deref(), Some("Test_Step_script.ps1"));
        assert_eq!(file.data.as_ref().unwrap().raw(), "Write-Host hi");
    }

    #[test]
    fn resolve_syntax_sugar_none_without_script_or_sugar() {
        let step: StepTemplate = serde_saphyr::from_str("name: S\n").unwrap();
        assert!(step.resolve_syntax_sugar().is_none());
    }

    #[test]
    fn desugar_sanitizes_step_name_to_ascii_identifier() {
        // `²` and `½` are `char::is_alphanumeric` but not identifier
        // characters; `١` (Arabic-Indic one) is a non-ASCII digit. Each
        // used to panic at the generated `{{Task.File.<name>}}` reference.
        for (name, expected) in [
            ("²x", "_x_script"),
            ("a½", "a__script"),
            ("١", "__script"),
            ("9lives", "_9lives_script"),
            ("Test Step", "Test_Step_script"),
            ("ünïcödé", "_n_c_d__script"),
        ] {
            let step: StepTemplate =
                serde_saphyr::from_str(&format!("name: \"{name}\"\nbash:\n  script: echo\n"))
                    .unwrap();
            let script = step.resolve_syntax_sugar().unwrap();
            let file = &script.embedded_files.as_ref().unwrap()[0];
            assert_eq!(file.name, expected, "step name {name:?}");
            assert_eq!(
                script.actions.on_run.args.as_ref().unwrap()[0].raw(),
                format!("{{{{Task.File.{expected}}}}}")
            );
        }
    }

    #[test]
    fn remap_authored_fields() {
        let k = SimpleActionKind::Bash;
        assert_eq!(k.remap_desugared_path(&[]), vec![]);
        assert_eq!(
            k.remap_desugared_path(&[f("let"), Index(2)]),
            vec![f("let"), Index(2)]
        );
        assert_eq!(
            k.remap_desugared_path(&[f("actions"), f("onRun"), f("args")]),
            vec![f("args")]
        );
        assert_eq!(
            k.remap_desugared_path(&[f("actions"), f("onRun"), f("timeout")]),
            vec![f("timeout")]
        );
        assert_eq!(
            k.remap_desugared_path(&[f("actions"), f("onRun"), f("cancelation")]),
            vec![f("cancelation")]
        );
        assert_eq!(
            k.remap_desugared_path(&[f("embeddedFiles"), Index(0), f("data")]),
            vec![f("script")]
        );
    }

    #[test]
    fn remap_args_offsets_by_synthetic_arg_count() {
        // bash: [<file>, user0, user1]
        let bash = SimpleActionKind::Bash;
        assert_eq!(bash.synthetic_arg_count(), 1);
        assert_eq!(
            bash.remap_desugared_path(&[f("actions"), f("onRun"), f("args"), Index(1)]),
            vec![f("args"), Index(0)]
        );
        // The synthesized Task.File reference has no authored counterpart.
        assert_eq!(
            bash.remap_desugared_path(&[f("actions"), f("onRun"), f("args"), Index(0)]),
            vec![]
        );
        // cmd: ["/C", <file>, user0]
        let cmd = SimpleActionKind::Cmd;
        assert_eq!(cmd.synthetic_arg_count(), 2);
        assert_eq!(
            cmd.remap_desugared_path(&[f("actions"), f("onRun"), f("args"), Index(2)]),
            vec![f("args"), Index(0)]
        );
        assert_eq!(
            cmd.remap_desugared_path(&[f("actions"), f("onRun"), f("args"), Index(1)]),
            vec![]
        );
        assert_eq!(SimpleActionKind::Powershell.synthetic_arg_count(), 2);
    }

    #[test]
    fn remap_synthesized_nodes_collapse_to_sugar_field() {
        let k = SimpleActionKind::Node;
        assert_eq!(
            k.remap_desugared_path(&[f("actions"), f("onRun"), f("command")]),
            vec![]
        );
        assert_eq!(k.remap_desugared_path(&[f("actions"), f("onRun")]), vec![]);
        assert_eq!(k.remap_desugared_path(&[f("embeddedFiles")]), vec![]);
        assert_eq!(
            k.remap_desugared_path(&[f("embeddedFiles"), Index(0), f("name")]),
            vec![]
        );
        assert_eq!(
            k.remap_desugared_path(&[f("embeddedFiles"), Index(0), f("filename")]),
            vec![]
        );
    }
}
