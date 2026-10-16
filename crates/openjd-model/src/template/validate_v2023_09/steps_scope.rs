// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The `Steps.*` scope of `CONTROL_FLOW` (RFC 0010 §The `Steps.*` scope).
//!
//! Two things live here, shared by template validation (pass 8) and the
//! job-creation re-checks in `create_job`:
//!
//! - [`add_steps_scope`] seeds a Step's symbol table with the `Steps.*`
//!   symbols of its direct dependencies, as `unresolved` values of the RFC's
//!   types, so every expression that reads one type-checks with EXPR's
//!   progressive evaluator.
//! - [`steps_ref_errors`] scans the symbols a format string or `let`
//!   binding reads for the `Steps.` prefix and explains each reference that
//!   is not available, instead of EXPR's generic "Undefined variable". The
//!   scan is static, so it also catches a reference in a branch that is
//!   never evaluated.

use std::sync::LazyLock;

use openjd_expr::symbol_table::SymbolTable;
use openjd_expr::types::ExprType;
use openjd_expr::value::ExprValue;
use regex::Regex;

use crate::template::{JobTemplate, StepTemplate, TaskParameterDefinition};
use crate::types::JobParameterType;

/// Where a format string is resolved, which decides whether the path-typed
/// `Steps.*.Output.*` and `Steps.*.TaskParam.*` values are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepsFieldScope {
    /// Resolved on the worker host (`@fmtstring[host]`): every symbol.
    Host,
    /// Resolved before a Session exists (job creation or step input
    /// binding): a value whose type is or contains `path` only through
    /// `RawOutput` and `TaskRawParam`.
    Template,
}

/// `<Identifier>` (Template Schemas §7.1), the pattern of
/// [`crate::template::Identifier`]. Only a Step whose name has this form can
/// be referenced through `Steps.`.
static IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid regex"));

fn is_identifier(name: &str) -> bool {
    IDENTIFIER_RE.is_match(name)
}

/// The members of a `Steps.<StepName>` table.
const MEMBERS: [&str; 4] = ["Output", "RawOutput", "TaskParam", "TaskRawParam"];

/// The expression type of one Task's value of a Task parameter: `U` in the
/// RFC's table. A `CHUNK[INT]` parameter is `int`, because the list has one
/// element per Task, not per chunk.
fn task_param_type(tp: &TaskParameterDefinition) -> ExprType {
    match tp {
        TaskParameterDefinition::INT(_) | TaskParameterDefinition::CHUNK_INT(_) => ExprType::INT,
        TaskParameterDefinition::FLOAT(_) => ExprType::FLOAT,
        TaskParameterDefinition::STRING(_) => ExprType::STRING,
        TaskParameterDefinition::PATH(_) => ExprType::PATH,
    }
}

/// Wrap one Task's value type `t` into the type of the `Steps.*` symbol for
/// `producer`: `list[t]` when it has a `parameterSpace`, and optional when it
/// has an `enableIf` (any `enableIf`, including a literal, as the RFC's table
/// is keyed on the field being present).
fn shape(producer: &StepTemplate, t: ExprType) -> ExprType {
    let t = if producer.parameter_space.is_some() {
        ExprType::list(t)
    } else {
        t
    };
    if producer.enable_if.is_some() {
        ExprType::union(vec![t, ExprType::NULLTYPE])
    } else {
        t
    }
}

/// The symbol type of `Steps.<P>.Output.<name>` (and `RawOutput`), with
/// whether its value is or contains `path`.
fn output_symbol(producer: &StepTemplate, name: &str) -> Option<(ExprType, bool)> {
    let ov = producer
        .output_variables
        .as_deref()?
        .iter()
        .find(|ov| ov.name() == name)?;
    let ty = ov.job_param_type();
    let is_path = matches!(ty, JobParameterType::Path | JobParameterType::ListPath);
    Some((shape(producer, ty.expr_type()), is_path))
}

/// The symbol type of `Steps.<P>.TaskParam.<name>` (and `TaskRawParam`),
/// with whether its value is or contains `path`.
fn task_param_symbol(producer: &StepTemplate, name: &str) -> Option<(ExprType, bool)> {
    let tp = producer
        .parameter_space
        .as_ref()?
        .task_parameter_definitions
        .iter()
        .find(|tp| tp.name() == name)?;
    let is_path = matches!(tp, TaskParameterDefinition::PATH(_));
    Some((shape(producer, task_param_type(tp)), is_path))
}

/// The Steps whose `Steps.*` symbols Step `step_index` may read: its direct
/// whole-Step dependencies that exist, are not the Step itself, and have an
/// `<Identifier>` name. (Structure rejects a duplicate dependency.)
fn visible_producers(jt: &JobTemplate, step_index: usize) -> Vec<&StepTemplate> {
    let step = &jt.steps[step_index];
    step.dependencies
        .iter()
        .flatten()
        .map(|dep| dep.depends_on.as_str())
        .filter(|name| *name != step.name && is_identifier(name))
        .filter_map(|name| jt.steps.iter().find(|s| s.name == name))
        .collect()
}

/// Seed `symtab` with the `Steps.*` symbols that Step `step_index` may read
/// in a field resolved in `scope`, each an `unresolved` value of the type
/// the RFC's §The `Steps.*` scope table gives it.
///
/// For each visible producer `P` (see [`visible_producers`]):
/// `Steps.P.Output.<V>` and `Steps.P.RawOutput.<V>` for each of its output
/// variables, and, when it has a `parameterSpace`, `Steps.P.TaskParam.<N>`
/// and `Steps.P.TaskRawParam.<N>` for each Task parameter. The `Raw` forms
/// have the same types as the others. In [`StepsFieldScope::Template`], an
/// `Output` or `TaskParam` whose type is or contains `path` is left out:
/// only its `Raw` form is available there.
///
/// Never call this for the job name, `jobEnvironments`, or an Environment
/// Template: `Steps.*` is not available there.
///
/// `Err` only when a key collides with an existing scalar entry. No user
/// symbol can start with the uppercase `Steps` root (a `let` name starts
/// lowercase), and only `<Identifier>`-named producers are seeded, so
/// `Steps.<P>` never contains a dot that could make one producer's table
/// overlap another's (a Step named `A.Output` would otherwise collide with
/// Step `A`'s output `Output`).
pub(crate) fn add_steps_scope(
    symtab: &mut SymbolTable,
    jt: &JobTemplate,
    step_index: usize,
    scope: StepsFieldScope,
) -> Result<(), openjd_expr::symbol_table::SymbolTableError> {
    for producer in visible_producers(jt, step_index) {
        let p = &producer.name;
        for ov in producer.output_variables.iter().flatten() {
            if let Some((ty, is_path)) = output_symbol(producer, ov.name()) {
                if !(is_path && scope == StepsFieldScope::Template) {
                    symtab.set(
                        &format!("Steps.{p}.Output.{}", ov.name()),
                        ExprValue::unresolved(ty.clone()),
                    )?;
                }
                symtab.set(
                    &format!("Steps.{p}.RawOutput.{}", ov.name()),
                    ExprValue::unresolved(ty),
                )?;
            }
        }
        if let Some(ps) = &producer.parameter_space {
            for tp in &ps.task_parameter_definitions {
                if let Some((ty, is_path)) = task_param_symbol(producer, tp.name()) {
                    if !(is_path && scope == StepsFieldScope::Template) {
                        symtab.set(
                            &format!("Steps.{p}.TaskParam.{}", tp.name()),
                            ExprValue::unresolved(ty.clone()),
                        )?;
                    }
                    symtab.set(
                        &format!("Steps.{p}.TaskRawParam.{}", tp.name()),
                        ExprValue::unresolved(ty),
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Where a reference is being checked, for [`steps_ref_errors`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct StepsRefContext<'a> {
    pub(crate) jt: &'a JobTemplate,
    /// The Step whose field this is, or `None` for a field where `Steps.*`
    /// is not available at all (the job name, `jobEnvironments`).
    pub(crate) step_index: Option<usize>,
    pub(crate) scope: StepsFieldScope,
}

/// Explain every `Steps.*` symbol in `symbols` that is not available in a
/// field described by `ctx`, one message per symbol, in sorted symbol
/// order. Symbols outside the `Steps` root, and available ones, produce
/// nothing.
///
/// Each message starts with the symbol, as written except in the path-type
/// message, which names only the four-part symbol and not a property read
/// from it. The checks, in order, the first that fails being reported: the
/// field allows `Steps.*`; the reference names a member
/// (`Steps.<StepName>.<Member>.<Name>`); the Step exists; its name is an
/// `<Identifier>`; it is not the reading Step; it is a direct dependency;
/// the member is one of the four; the output or Task parameter exists; and
/// a path-typed `Output` or `TaskParam` is not read in a TEMPLATE-scope
/// field.
pub(crate) fn steps_ref_errors<'s>(
    symbols: impl IntoIterator<Item = &'s String>,
    ctx: &StepsRefContext<'_>,
) -> Vec<String> {
    let mut steps_symbols: Vec<&String> = symbols
        .into_iter()
        .filter(|s| *s == "Steps" || s.starts_with("Steps."))
        .collect();
    steps_symbols.sort();
    steps_symbols.dedup();
    steps_symbols
        .into_iter()
        .filter_map(|sym| steps_ref_error(sym, ctx))
        .collect()
}

fn steps_ref_error(sym: &str, ctx: &StepsRefContext<'_>) -> Option<String> {
    let Some(step_index) = ctx.step_index else {
        return Some(format!(
            "'{sym}': Steps.* symbols are not available in this field. Only a Step's enableIf, let, hostRequirements, parameterSpace, stepEnvironments, and script may read them."
        ));
    };
    let parts: Vec<&str> = sym.split('.').collect();
    if parts.len() < 4 {
        return Some(format!(
            "'{sym}' is not a complete Steps.* reference; expected Steps.<StepName>.Output.<Name>, .RawOutput.<Name>, .TaskParam.<Name>, or .TaskRawParam.<Name>."
        ));
    }
    let (p, member, name) = (parts[1], parts[2], parts[3]);
    let jt = ctx.jt;
    let reader = &jt.steps[step_index];
    let Some(producer) = jt.steps.iter().find(|s| s.name == p) else {
        return Some(format!("'{sym}': there is no Step named '{p}'."));
    };
    if !is_identifier(p) {
        return Some(format!(
            "'{sym}': Step '{p}' cannot be referenced through Steps.*, because its name is not an <Identifier>."
        ));
    }
    if p == reader.name {
        return Some(format!(
            "'{sym}': a Step cannot read its own Steps.* symbols."
        ));
    }
    let is_direct = reader
        .dependencies
        .iter()
        .flatten()
        .any(|d| d.depends_on == p);
    if !is_direct {
        return Some(format!(
            "'{sym}': Step '{p}' is not a direct dependency of Step '{}'. Add 'dependsOn: {p}' to its dependencies to read Steps.{p}.*.",
            reader.name
        ));
    }
    let (found, raw_member) = match member {
        "Output" | "RawOutput" => {
            let Some(found) = output_symbol(producer, name) else {
                return Some(format!(
                    "'{sym}': Step '{p}' has no output variable '{name}'."
                ));
            };
            (found, "RawOutput")
        }
        "TaskParam" | "TaskRawParam" => {
            if producer.parameter_space.is_none() {
                return Some(format!(
                    "'{sym}': Step '{p}' has no parameterSpace, so it has no Task parameters."
                ));
            }
            let Some(found) = task_param_symbol(producer, name) else {
                return Some(format!(
                    "'{sym}': Step '{p}' has no Task parameter '{name}'."
                ));
            };
            (found, "TaskRawParam")
        }
        _ => {
            return Some(format!(
                "'{sym}': '{member}' is not a member of Steps.{p}; expected {}.",
                MEMBERS.join(", ")
            ));
        }
    };
    let (_, is_path) = found;
    if is_path && ctx.scope == StepsFieldScope::Template && member != raw_member {
        // Name the symbol itself, not a property read from it (`.name`).
        return Some(format!(
            "'Steps.{p}.{member}.{name}' has a path type, so it is available only in fields resolved on the worker host. Use Steps.{p}.{raw_member}.{name} here."
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CallerLimits;

    /// A job template with producers `P` (no parameterSpace), `Q`
    /// (parameterSpace, CHUNK[INT], PATH, FLOAT, and INT Task parameters), `R`
    /// (enableIf, no parameterSpace), `M` (enableIf and parameterSpace), and
    /// a consumer `C` that depends on all four, and `Z`, which depends on `C`
    /// only.
    const TEMPLATE: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, TASK_CHUNKING]
name: J
steps:
  - name: P
    outputVariables:
      - {name: Count, type: INT}
      - {name: F, type: PATH}
      - {name: Fs, type: "LIST[PATH]"}
      - {name: L, type: "LIST[STRING]"}
      - {name: S, type: STRING}
    script: {actions: {onRun: {command: x}}}
  - name: Q
    parameterSpace:
      taskParameterDefinitions:
        - {name: Frame, type: "CHUNK[INT]", range: "1-10", chunks: {defaultTaskCount: 2, rangeConstraint: NONCONTIGUOUS}}
        - {name: D, type: PATH, range: ["/a", "/b"]}
        - {name: X, type: FLOAT, range: [1.5]}
        - {name: I, type: INT, range: [1]}
    outputVariables:
      - {name: T, type: FLOAT}
      - {name: Fs, type: "LIST[PATH]"}
    script: {actions: {onRun: {command: x}}}
  - name: R
    enableIf: true
    outputVariables:
      - {name: B, type: BOOL}
    script: {actions: {onRun: {command: x}}}
  - name: M
    enableIf: "{{true}}"
    parameterSpace:
      taskParameterDefinitions:
        - {name: S, type: STRING, range: [a]}
    outputVariables:
      - {name: K, type: "LIST[INT]"}
    script: {actions: {onRun: {command: x}}}
  - name: C
    dependencies:
      - {dependsOn: P}
      - {dependsOn: Q}
      - {dependsOn: R}
      - {dependsOn: M}
    script: {actions: {onRun: {command: x}}}
  - name: Z
    dependencies:
      - {dependsOn: C}
    script: {actions: {onRun: {command: x}}}
"#;

    fn template() -> JobTemplate {
        crate::decode_job_template(
            serde_saphyr::from_str(TEMPLATE).unwrap(),
            Some(&["EXPR", "CONTROL_FLOW", "TASK_CHUNKING"]),
            &CallerLimits::default(),
        )
        .expect("valid template")
    }

    fn seeded(scope: StepsFieldScope) -> SymbolTable {
        let jt = template();
        let mut symtab = SymbolTable::new();
        add_steps_scope(&mut symtab, &jt, 4, scope).unwrap();
        symtab
    }

    fn opt(t: ExprType) -> ExprType {
        ExprType::union(vec![t, ExprType::NULLTYPE])
    }

    fn ty(symtab: &SymbolTable, key: &str) -> Option<ExprType> {
        match symtab.get_value(key)? {
            ExprValue::Unresolved(t) => Some(t.clone()),
            other => panic!("{key} is not unresolved: {other:?}"),
        }
    }

    #[test]
    fn host_scope_types_follow_the_rfc_table() {
        let s = seeded(StepsFieldScope::Host);
        let list = ExprType::list;
        let cases = [
            // No parameterSpace, no enableIf: T.
            ("Steps.P.Output.Count", ExprType::INT),
            ("Steps.P.Output.F", ExprType::PATH),
            ("Steps.P.Output.Fs", list(ExprType::PATH)),
            ("Steps.P.Output.L", list(ExprType::STRING)),
            ("Steps.P.Output.S", ExprType::STRING),
            // The Raw forms have the same types.
            ("Steps.P.RawOutput.Count", ExprType::INT),
            ("Steps.P.RawOutput.F", ExprType::PATH),
            // parameterSpace: list[T]; a list output nests.
            ("Steps.Q.Output.T", list(ExprType::FLOAT)),
            ("Steps.Q.Output.Fs", list(list(ExprType::PATH))),
            // CHUNK[INT] TaskParam is one int per Task.
            ("Steps.Q.TaskParam.Frame", list(ExprType::INT)),
            ("Steps.Q.TaskRawParam.Frame", list(ExprType::INT)),
            ("Steps.Q.TaskParam.D", list(ExprType::PATH)),
            ("Steps.Q.TaskRawParam.D", list(ExprType::PATH)),
            ("Steps.Q.TaskParam.X", list(ExprType::FLOAT)),
            ("Steps.Q.TaskParam.I", list(ExprType::INT)),
            ("Steps.Q.TaskRawParam.I", list(ExprType::INT)),
            // enableIf (even a literal one): T?.
            ("Steps.R.Output.B", opt(ExprType::BOOL)),
            ("Steps.R.RawOutput.B", opt(ExprType::BOOL)),
            // enableIf and parameterSpace: list[T]?.
            ("Steps.M.Output.K", opt(list(list(ExprType::INT)))),
            ("Steps.M.TaskParam.S", opt(list(ExprType::STRING))),
        ];
        for (key, expected) in cases {
            assert_eq!(ty(&s, key), Some(expected), "{key}");
        }
        // No Task parameters for a Step without a parameterSpace.
        assert_eq!(s.get_table("Steps.P.TaskParam").map(|_| ()), None);
    }

    #[test]
    fn template_scope_omits_path_output_and_task_param_but_keeps_raw() {
        let s = seeded(StepsFieldScope::Template);
        for key in [
            "Steps.P.Output.F",
            "Steps.P.Output.Fs",
            "Steps.Q.Output.Fs",
            "Steps.Q.TaskParam.D",
        ] {
            assert_eq!(ty(&s, key), None, "{key}");
        }
        assert_eq!(ty(&s, "Steps.P.RawOutput.F"), Some(ExprType::PATH));
        assert_eq!(
            ty(&s, "Steps.Q.TaskRawParam.D"),
            Some(ExprType::list(ExprType::PATH))
        );
        assert_eq!(ty(&s, "Steps.P.Output.Count"), Some(ExprType::INT));
    }

    #[test]
    fn only_direct_dependencies_are_seeded() {
        let jt = template();
        // P depends on nothing.
        let mut s = SymbolTable::new();
        add_steps_scope(&mut s, &jt, 0, StepsFieldScope::Host).unwrap();
        assert!(s.get_table("Steps").is_none());
        // Z depends on C, which depends on P, Q, R, and M: none of their
        // symbols are seeded, and C has none of its own.
        let mut s = SymbolTable::new();
        add_steps_scope(&mut s, &jt, 5, StepsFieldScope::Host).unwrap();
        assert!(s.get_table("Steps").is_none());
    }

    #[test]
    fn identifier_pattern() {
        assert!(is_identifier("Render_2"));
        assert!(is_identifier("_x"));
        for name in ["Find Frames", "2x", "Café", "", "a-b"] {
            assert!(!is_identifier(name), "{name}");
        }
    }
}
