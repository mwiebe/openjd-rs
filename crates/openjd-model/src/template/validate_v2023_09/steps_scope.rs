// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The `Steps.*` scope of `CONTROL_FLOW` (RFC 0011 §The `Steps.*` scope), with
//! the loop additions of RFC 0013: the `Loop.*` scope (RFC 0013 §The `Loop.*`
//! scope) and `Steps.*` in and around a loop (RFC 0013 §`Steps.*` in and
//! around a loop).
//!
//! Two things live here, shared by template validation (pass 8) and the
//! job-creation re-checks in `create_job`:
//!
//! - [`add_control_flow_scope`] seeds a field's symbol table with the
//!   `Steps.*` symbols of the Steps and loops it may read, and, in a loop's
//!   body Steps and the loop's own expressions, with the `Loop.*` and
//!   `RawLoop.*` symbols, all as `unresolved` values of the RFCs' types, so
//!   every expression that reads one type-checks with EXPR's progressive
//!   evaluator.
//! - [`control_flow_ref_errors`] scans the symbols a format string or `let`
//!   binding reads for the `Steps.`, `Loop.`, and `RawLoop.` prefixes and
//!   explains each reference that is not available, instead of EXPR's
//!   generic "Undefined variable". The scan is static, so it also catches a
//!   reference in a branch that is never evaluated.

use std::fmt;
use std::sync::LazyLock;

use openjd_expr::symbol_table::SymbolTable;
use openjd_expr::types::ExprType;
use openjd_expr::value::ExprValue;
use regex::Regex;

use crate::template::{
    JobTemplate, StepEntry, StepLoop, StepTemplate, StepTemplateRef, TaskParameterDefinition,
};
use crate::types::{JobParameterType, OutputCollectMode};

/// Where a format string is resolved, which decides whether the path-typed
/// `Steps.*.Output.*`, `Steps.*.TaskParam.*`, and `Loop.*` values are
/// available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepsFieldScope {
    /// Resolved on the worker host (`@fmtstring[host]`): every symbol.
    Host,
    /// Resolved before a Session exists (job creation, step input binding,
    /// or an iteration boundary): a value whose type is or contains `path`
    /// only through `RawOutput`, `TaskRawParam`, and `RawLoop`.
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

/// Where a `<StepTemplate>` is in its Job Template: its entry in `steps`,
/// and, for a body Step, its index in that loop's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StepLocation {
    /// The index of its entry in `steps`: its own, or its loop's.
    pub(crate) entry: usize,
    /// Its index in its loop's body, `None` for a top-level Step.
    pub(crate) body: Option<usize>,
}

impl StepLocation {
    /// A top-level Step, entry `entry` of `steps`.
    pub(crate) fn top(entry: usize) -> Self {
        Self { entry, body: None }
    }

    /// The location of `st`.
    pub(crate) fn of(st: &StepTemplateRef<'_>) -> Self {
        Self {
            entry: st.entry_index,
            body: st.body_index,
        }
    }

    /// The Step at this location, and its loop for a body Step.
    fn resolve(self, jt: &JobTemplate) -> Option<(&StepTemplate, Option<&StepLoop>)> {
        match (jt.steps.get(self.entry)?, self.body) {
            (StepEntry::Step(s), None) => Some((s, None)),
            (StepEntry::Loop(l), Some(j)) => Some((l.body().get(j)?, Some(l))),
            _ => None,
        }
    }
}

/// One of a loop's own expression fields (RFC 0013 §`<StepLoop>` and
/// §`<LoopDefinition>`), which decides the `Steps.*` and `Loop.*` symbols
/// available in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopField {
    /// `enableIf`: the loop's dependencies; no `Loop.*`.
    EnableIf,
    /// `maxIterations`: neither `Steps.*` nor `Loop.*`.
    MaxIterations,
    /// A variable's `initial`: the loop's dependencies; no `Loop.*`.
    Initial,
    /// `while`: the loop's dependencies and `Loop.*`.
    While,
    /// `until`: the loop's dependencies, its body Steps, and `Loop.*`.
    Until,
    /// A variable's `next`: as `until`.
    Next,
    /// A loop output's `value`: as `until`.
    OutputValue,
}

impl LoopField {
    /// Whether the field may read `Steps.*` at all.
    fn reads_steps(self) -> bool {
        self != Self::MaxIterations
    }

    /// Whether the field may read the loop's body Steps.
    fn reads_body(self) -> bool {
        matches!(self, Self::Until | Self::Next | Self::OutputValue)
    }

    /// Whether the field may read `Loop.*` and `RawLoop.*`.
    fn reads_loop(self) -> bool {
        matches!(
            self,
            Self::While | Self::Until | Self::Next | Self::OutputValue
        )
    }
}

impl fmt::Display for LoopField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EnableIf => "enableIf",
            Self::MaxIterations => "maxIterations",
            Self::Initial => "initial",
            Self::While => "while",
            Self::Until => "until",
            Self::Next => "next",
            Self::OutputValue => "output value",
        })
    }
}

/// What reads a field, which decides the `Steps.*` and `Loop.*` symbols
/// available in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepsReader {
    /// A field where `Steps.*` is not available at all (the job name,
    /// `jobEnvironments`).
    None,
    /// A field of a Step: its direct dependencies' symbols, and for a body
    /// Step also its loop's dependencies' symbols and `Loop.*`.
    Step(StepLocation),
    /// A loop's own field: see [`LoopField`].
    Loop {
        /// The loop's entry in `steps`.
        entry: usize,
        /// Which of its fields.
        field: LoopField,
    },
    /// A `<JobOutputDefinition>`'s `value`: any top-level Step's or loop's
    /// symbols.
    JobOutput,
}

/// A Step or a loop, as the producer of `Steps.<Name>.*` symbols.
#[derive(Debug, Clone, Copy)]
enum Producer<'a> {
    Step(&'a StepTemplate),
    Loop(&'a StepLoop),
}

impl<'a> Producer<'a> {
    fn of(entry: &'a StepEntry) -> Self {
        match entry {
            StepEntry::Step(s) => Self::Step(s),
            StepEntry::Loop(l) => Self::Loop(l),
        }
    }

    fn name(self) -> &'a str {
        match self {
            Self::Step(s) => &s.name,
            Self::Loop(l) => &l.name,
        }
    }
}

/// The expression type of one Task's value of a Task parameter: `U` in the
/// RFC 0011 table. A `CHUNK[INT]` parameter is `int`, because the list has one
/// element per Task, not per chunk.
fn task_param_type(tp: &TaskParameterDefinition) -> ExprType {
    match tp {
        TaskParameterDefinition::INT(_) | TaskParameterDefinition::CHUNK_INT(_) => ExprType::INT,
        TaskParameterDefinition::FLOAT(_) => ExprType::FLOAT,
        TaskParameterDefinition::STRING(_) => ExprType::STRING,
        TaskParameterDefinition::PATH(_) => ExprType::PATH,
    }
}

fn optional(t: ExprType) -> ExprType {
    ExprType::union(vec![t, ExprType::NULLTYPE])
}

fn is_path_type(ty: JobParameterType) -> bool {
    matches!(ty, JobParameterType::Path | JobParameterType::ListPath)
}

/// Wrap one Task's value type `t` into the type of the `Steps.*` symbol for
/// `producer`: `list[t]` when it has a `parameterSpace`, and optional when it
/// has an `enableIf` (any `enableIf`, including a literal, as RFC 0011's table
/// is keyed on the field being present).
fn shape(producer: &StepTemplate, t: ExprType) -> ExprType {
    let t = if producer.parameter_space.is_some() {
        ExprType::list(t)
    } else {
        t
    };
    if producer.enable_if.is_some() {
        optional(t)
    } else {
        t
    }
}

/// The symbol type of `Steps.<P>.Output.<name>` (and `RawOutput`), with
/// whether its value is or contains `path`.
fn output_symbol(producer: Producer<'_>, name: &str) -> Option<(ExprType, bool)> {
    match producer {
        Producer::Step(step) => {
            let ov = step
                .output_variables
                .as_deref()?
                .iter()
                .find(|ov| ov.name() == name)?;
            let ty = ov.job_param_type();
            Some((shape(step, ty.expr_type()), is_path_type(ty)))
        }
        Producer::Loop(l) => loop_output_symbol(l, name),
    }
}

/// The symbol type of loop output `name` of `l`, by RFC 0013 §`Steps.*` in
/// and around a loop's table: a `LAST` output is `T` for an `until` loop and
/// `T?` for a `while` loop; an `APPEND` output is the declared list type;
/// and both are optional in a loop with `enableIf`. A loop has no Tasks, so
/// its outputs are never wrapped in a per-Task list. An output whose
/// `collect` is missing or `ONCE`, which validation rejects, is typed as
/// `LAST`.
fn loop_output_symbol(l: &StepLoop, name: &str) -> Option<(ExprType, bool)> {
    let ov = l
        .output_variables_list()
        .iter()
        .find(|ov| ov.name() == name)?;
    let ty = ov.job_param_type();
    let append = ov.collect() == Some(OutputCollectMode::Append);
    let is_optional =
        l.enable_if.is_some() || (!append && l.loop_definition.while_condition.is_some());
    let t = ty.expr_type();
    Some((if is_optional { optional(t) } else { t }, is_path_type(ty)))
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

/// The top-level entry named `name`.
fn top_level<'a>(jt: &'a JobTemplate, name: &str) -> Option<(usize, Producer<'a>)> {
    jt.steps
        .iter()
        .enumerate()
        .find(|(_, e)| e.name() == name)
        .map(|(i, e)| (i, Producer::of(e)))
}

/// The top-level entries that `deps` name, other than `own` and those
/// without an `<Identifier>` name.
fn top_level_producers<'a>(
    jt: &'a JobTemplate,
    deps: impl Iterator<Item = &'a str>,
    own: &str,
) -> Vec<Producer<'a>> {
    deps.filter(|name| *name != own && is_identifier(name))
        .filter_map(|name| top_level(jt, name).map(|(_, p)| p))
        .collect()
}

fn dependency_names(
    deps: Option<&[crate::template::StepDependency]>,
) -> impl Iterator<Item = &str> {
    deps.into_iter().flatten().map(|d| d.depends_on.as_str())
}

/// The Steps and loops whose `Steps.*` symbols `reader` may read (RFC 0011
/// §The `Steps.*` scope, RFC 0013 §`Steps.*` in and around a loop):
///
/// - a top-level Step: its direct whole-Step dependencies;
/// - a body Step of loop `L`: the body Steps it depends on, and `L`'s
///   dependencies;
/// - a loop's `enableIf`, `while`, and each `initial`: its dependencies;
/// - a loop's `until`, `next`, and output `value`s: its dependencies and
///   every body Step;
/// - a job output: every top-level Step and loop.
///
/// A producer must exist, not be the reader itself, and have an
/// `<Identifier>` name. (Structure rejects a duplicate dependency.)
fn visible_producers(jt: &JobTemplate, reader: StepsReader) -> Vec<Producer<'_>> {
    match reader {
        StepsReader::None => Vec::new(),
        StepsReader::JobOutput => jt
            .steps
            .iter()
            .filter(|e| is_identifier(e.name()))
            .map(Producer::of)
            .collect(),
        StepsReader::Step(loc) => {
            let Some((step, step_loop)) = loc.resolve(jt) else {
                return Vec::new();
            };
            let own_deps = dependency_names(step.dependencies.as_deref());
            match step_loop {
                None => top_level_producers(jt, own_deps, &step.name),
                Some(l) => {
                    let mut producers: Vec<Producer<'_>> = own_deps
                        .filter(|name| *name != step.name && is_identifier(name))
                        .filter_map(|name| l.body().iter().find(|s| s.name == name))
                        .map(Producer::Step)
                        .collect();
                    producers.extend(top_level_producers(
                        jt,
                        dependency_names(l.dependencies.as_deref()),
                        &l.name,
                    ));
                    producers
                }
            }
        }
        StepsReader::Loop { entry, field } => {
            let Some(StepEntry::Loop(l)) = jt.steps.get(entry) else {
                return Vec::new();
            };
            if !field.reads_steps() {
                return Vec::new();
            }
            let mut producers =
                top_level_producers(jt, dependency_names(l.dependencies.as_deref()), &l.name);
            if field.reads_body() {
                producers.extend(
                    l.body()
                        .iter()
                        .filter(|s| is_identifier(&s.name))
                        .map(Producer::Step),
                );
            }
            producers
        }
    }
}

/// The loop whose `Loop.*` and `RawLoop.*` symbols `reader` may read (RFC
/// 0013 §The `Loop.*` scope): a body Step's loop, and the loop itself in its
/// `while`, `until`, `next`, and output `value`s.
fn readable_loop(jt: &JobTemplate, reader: StepsReader) -> Option<&StepLoop> {
    match reader {
        StepsReader::Step(StepLocation {
            entry,
            body: Some(_),
        }) => jt.steps.get(entry)?.as_loop(),
        StepsReader::Loop { entry, field } if field.reads_loop() => jt.steps.get(entry)?.as_loop(),
        _ => None,
    }
}

/// Seed `symtab` with the `Steps.*` and `Loop.*` symbols available to
/// `reader` in a field resolved in `scope`, each an `unresolved` value of the
/// type the RFCs give it.
///
/// For each visible producer `P` (see [`visible_producers`]):
/// `Steps.P.Output.<V>` and `Steps.P.RawOutput.<V>` for each of its output
/// variables (a loop's are its loop outputs), and, for a Step with a
/// `parameterSpace`, `Steps.P.TaskParam.<N>` and `Steps.P.TaskRawParam.<N>`
/// for each Task parameter. For the readable loop `L` (see
/// [`readable_loop`]): `Loop.Iteration` (`int`), and `Loop.<V>` and
/// `RawLoop.<V>` for each of its variables. The `Raw` forms have the same
/// types as the others. In [`StepsFieldScope::Template`], an `Output`,
/// `TaskParam`, or `Loop` symbol whose type is or contains `path` is left
/// out: only its `Raw` form is available there.
///
/// A key the table already holds is left as it is, so step input binding can
/// seed placeholders around the values that are concrete.
///
/// Never call this for the job name, `jobEnvironments`, or an Environment
/// Template: neither `Steps.*` nor `Loop.*` is available there.
///
/// `Err` only when a key collides with an existing scalar entry. No user
/// symbol can start with the uppercase `Steps`, `Loop`, or `RawLoop` roots (a
/// `let` name starts lowercase), and only `<Identifier>`-named producers are
/// seeded, so `Steps.<P>` never contains a dot that could make one
/// producer's table overlap another's (a Step named `A.Output` would
/// otherwise collide with Step `A`'s output `Output`).
pub(crate) fn add_control_flow_scope(
    symtab: &mut SymbolTable,
    jt: &JobTemplate,
    reader: StepsReader,
    scope: StepsFieldScope,
) -> Result<(), openjd_expr::symbol_table::SymbolTableError> {
    seed_producers(symtab, visible_producers(jt, reader), scope)?;
    if let Some(l) = readable_loop(jt, reader) {
        seed_loop(symtab, l, scope)?;
    }
    Ok(())
}

/// [`add_control_flow_scope`] for top-level Step `step_index` (used by the
/// tests; the job-creation callers pass a [`StepLocation`]).
#[cfg(test)]
pub(crate) fn add_steps_scope(
    symtab: &mut SymbolTable,
    jt: &JobTemplate,
    step_index: usize,
    scope: StepsFieldScope,
) -> Result<(), openjd_expr::symbol_table::SymbolTableError> {
    add_control_flow_scope(
        symtab,
        jt,
        StepsReader::Step(StepLocation::top(step_index)),
        scope,
    )
}

/// Seed `symtab` with the `Steps.*` symbols a `<JobOutputDefinition>`'s
/// `value` may read: those of every top-level Step and loop with an
/// `<Identifier>` name, with no dependency required (RFC 0011 §Job outputs,
/// RFC 0013 §`Steps.*` in and around a loop item 4), each typed as for
/// [`add_control_flow_scope`] in [`StepsFieldScope::Template`], so a value
/// whose type is or contains `path` is available only through `RawOutput`
/// and `TaskRawParam`.
pub(crate) fn add_job_output_steps_scope(
    symtab: &mut SymbolTable,
    jt: &JobTemplate,
) -> Result<(), openjd_expr::symbol_table::SymbolTableError> {
    add_control_flow_scope(
        symtab,
        jt,
        StepsReader::JobOutput,
        StepsFieldScope::Template,
    )
}

/// Seed the `Steps.*` symbols of `producers`; see [`add_control_flow_scope`].
fn seed_producers(
    symtab: &mut SymbolTable,
    producers: Vec<Producer<'_>>,
    scope: StepsFieldScope,
) -> Result<(), openjd_expr::symbol_table::SymbolTableError> {
    // A symbol already in the table (a concrete value at step input binding)
    // is kept.
    let mut seed = |key: String, ty: ExprType| {
        if symtab.contains(&key) {
            Ok(())
        } else {
            symtab.set(&key, ExprValue::unresolved(ty))
        }
    };
    for producer in producers {
        let p = producer.name();
        let output_names: Vec<&str> = match producer {
            Producer::Step(s) => s
                .output_variables
                .iter()
                .flatten()
                .map(|ov| ov.name())
                .collect(),
            Producer::Loop(l) => l
                .output_variables_list()
                .iter()
                .map(|ov| ov.name())
                .collect(),
        };
        for name in output_names {
            if let Some((ty, is_path)) = output_symbol(producer, name) {
                if !(is_path && scope == StepsFieldScope::Template) {
                    seed(format!("Steps.{p}.Output.{name}"), ty.clone())?;
                }
                seed(format!("Steps.{p}.RawOutput.{name}"), ty)?;
            }
        }
        let Producer::Step(step) = producer else {
            continue;
        };
        if let Some(ps) = &step.parameter_space {
            for tp in &ps.task_parameter_definitions {
                if let Some((ty, is_path)) = task_param_symbol(step, tp.name()) {
                    if !(is_path && scope == StepsFieldScope::Template) {
                        seed(format!("Steps.{p}.TaskParam.{}", tp.name()), ty.clone())?;
                    }
                    seed(format!("Steps.{p}.TaskRawParam.{}", tp.name()), ty)?;
                }
            }
        }
    }
    Ok(())
}

/// Seed the `Loop.*` and `RawLoop.*` symbols of `l`; see
/// [`add_control_flow_scope`]. A variable named `Iteration`, which
/// validation rejects, is not seeded, so `Loop.Iteration` stays an `int`.
fn seed_loop(
    symtab: &mut SymbolTable,
    l: &StepLoop,
    scope: StepsFieldScope,
) -> Result<(), openjd_expr::symbol_table::SymbolTableError> {
    let mut seed = |key: String, ty: ExprType| {
        if symtab.contains(&key) {
            Ok(())
        } else {
            symtab.set(&key, ExprValue::unresolved(ty))
        }
    };
    seed("Loop.Iteration".to_string(), ExprType::INT)?;
    for var in l.variables() {
        let name = var.name();
        if name == "Iteration" {
            continue;
        }
        let ty = var.job_param_type();
        if !(is_path_type(ty) && scope == StepsFieldScope::Template) {
            seed(format!("Loop.{name}"), ty.expr_type())?;
        }
        seed(format!("RawLoop.{name}"), ty.expr_type())?;
    }
    Ok(())
}

/// Where a reference is being checked, for [`control_flow_ref_errors`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct StepsRefContext<'a> {
    pub(crate) jt: &'a JobTemplate,
    pub(crate) reader: StepsReader,
    pub(crate) scope: StepsFieldScope,
}

fn is_steps_symbol(s: &str) -> bool {
    s == "Steps" || s.starts_with("Steps.")
}

fn is_loop_symbol(s: &str) -> bool {
    s == "Loop" || s == "RawLoop" || s.starts_with("Loop.") || s.starts_with("RawLoop.")
}

/// Explain every `Steps.*`, `Loop.*`, and `RawLoop.*` symbol in `symbols`
/// that is not available in a field described by `ctx`, one message per
/// symbol, in sorted symbol order. Other symbols, and available ones,
/// produce nothing.
///
/// For a `Steps.*` symbol, each message starts with the symbol, as written
/// except in the path-type message, which names only the four-part symbol
/// and not a property read from it. The checks, in order, the first that
/// fails being reported: the field allows `Steps.*`; the reference names a
/// member (`Steps.<StepName>.<Member>.<Name>`); the Step exists; its name
/// is an `<Identifier>`; the reader may read it (see [`visible_producers`]:
/// it is not the reader itself, it is a dependency the reader may read, and
/// it is not a body Step read from outside its loop or from a field of its
/// loop that runs before an iteration); the member is one of the four, and
/// not a Task parameter of a loop; the output or Task parameter exists; and
/// a path-typed `Output` or `TaskParam` is not read in a TEMPLATE-scope
/// field. For a `Loop.*` symbol, see [`loop_ref_error`].
pub(crate) fn control_flow_ref_errors<'s>(
    symbols: impl IntoIterator<Item = &'s String>,
    ctx: &StepsRefContext<'_>,
) -> Vec<String> {
    let mut cf_symbols: Vec<&String> = symbols
        .into_iter()
        .filter(|s| is_steps_symbol(s) || is_loop_symbol(s))
        .collect();
    cf_symbols.sort();
    cf_symbols.dedup();
    cf_symbols
        .into_iter()
        .filter_map(|sym| {
            if is_steps_symbol(sym) {
                steps_ref_error(sym, ctx)
            } else {
                loop_ref_error(sym, ctx)
            }
        })
        .collect()
}

/// Where a Step or loop named in a `Steps.*` reference is.
enum Named<'a> {
    /// Top-level entry `entry`.
    TopLevel {
        entry: usize,
        producer: Producer<'a>,
    },
    /// A body Step of `step_loop`.
    Body {
        step: &'a StepTemplate,
        step_loop: &'a StepLoop,
    },
}

fn find_named<'a>(jt: &'a JobTemplate, name: &str) -> Option<Named<'a>> {
    if let Some((entry, producer)) = top_level(jt, name) {
        return Some(Named::TopLevel { entry, producer });
    }
    jt.loops().find_map(|l| {
        l.body()
            .iter()
            .find(|s| s.name == name)
            .map(|step| Named::Body { step, step_loop: l })
    })
}

fn names_dependency(deps: Option<&[crate::template::StepDependency]>, p: &str) -> bool {
    dependency_names(deps).any(|d| d == p)
}

/// The message for a body Step of loop `l` read where it is not available.
fn body_step_outside_loop(sym: &str, p: &str, l: &str) -> String {
    format!(
        "'{sym}': Step '{p}' is a body Step of loop '{l}', and is available only within that loop. Read a loop output instead, as Steps.{l}.Output.<Name>."
    )
}

/// Why `reader` may not read Step or loop `p` (found as `named`), or `None`
/// when it may.
fn reader_error(
    sym: &str,
    p: &str,
    named: &Named<'_>,
    ctx: &StepsRefContext<'_>,
) -> Option<String> {
    let jt = ctx.jt;
    match ctx.reader {
        // Answered before the reference is parsed.
        StepsReader::None => None,
        StepsReader::JobOutput => match named {
            Named::Body { step_loop, .. } => Some(body_step_outside_loop(sym, p, &step_loop.name)),
            Named::TopLevel { .. } => None,
        },
        StepsReader::Step(loc) => {
            let (reader, reader_loop) = loc.resolve(jt)?;
            if p == reader.name {
                return Some(format!(
                    "'{sym}': a Step cannot read its own Steps.* symbols."
                ));
            }
            let reads_own = names_dependency(reader.dependencies.as_deref(), p);
            let not_direct = || {
                format!(
                    "'{sym}': Step '{p}' is not a direct dependency of Step '{}'. Add 'dependsOn: {p}' to its dependencies to read Steps.{p}.*.",
                    reader.name
                )
            };
            match (reader_loop, named) {
                (None, Named::TopLevel { .. }) => (!reads_own).then(not_direct),
                (None, Named::Body { step_loop, .. }) => {
                    Some(body_step_outside_loop(sym, p, &step_loop.name))
                }
                (Some(l), Named::Body { step_loop, .. }) if step_loop.name != l.name => {
                    Some(body_step_outside_loop(sym, p, &step_loop.name))
                }
                (Some(_), Named::Body { .. }) => (!reads_own).then(not_direct),
                (Some(l), Named::TopLevel { entry, .. }) if *entry == loc.entry => Some(format!(
                    "'{sym}': Step '{}' is in the body of loop '{}', which cannot read the loop's own outputs: they are available only after the loop ends.",
                    reader.name, l.name
                )),
                (Some(l), Named::TopLevel { .. }) => {
                    (!names_dependency(l.dependencies.as_deref(), p)).then(|| {
                        format!(
                            "'{sym}': Step '{p}' is not a dependency of loop '{}'. A body Step reads the Steps its loop depends on: add 'dependsOn: {p}' to the dependencies of loop '{}' to read Steps.{p}.*.",
                            l.name, l.name
                        )
                    })
                }
            }
        }
        StepsReader::Loop { entry, field } => {
            let l = jt.steps.get(entry)?.as_loop()?;
            match named {
                Named::TopLevel { entry: e, .. } if *e == entry => Some(format!(
                    "'{sym}': a loop cannot read its own Steps.* symbols; they are available only after the loop ends."
                )),
                Named::TopLevel { .. } => (!names_dependency(l.dependencies.as_deref(), p)).then(|| {
                    format!(
                        "'{sym}': Step '{p}' is not a direct dependency of loop '{}'. Add 'dependsOn: {p}' to its dependencies to read Steps.{p}.*.",
                        l.name
                    )
                }),
                Named::Body { step_loop, .. } if step_loop.name != l.name => {
                    Some(body_step_outside_loop(sym, p, &step_loop.name))
                }
                Named::Body { .. } => (!field.reads_body()).then(|| {
                    format!(
                        "'{sym}': Step '{p}' is a body Step of loop '{}', which the loop's {field} cannot read: it is evaluated before an iteration runs. The loop's until, next, and output values may read its body Steps.",
                        l.name
                    )
                }),
            }
        }
    }
}

fn steps_ref_error(sym: &str, ctx: &StepsRefContext<'_>) -> Option<String> {
    match ctx.reader {
        StepsReader::None => {
            return Some(format!(
                "'{sym}': Steps.* symbols are not available in this field. Only a Step's enableIf, let, hostRequirements, parameterSpace, stepEnvironments, and script, a loop's enableIf, while, until, initial, next, and output values, and a job output's value, may read them."
            ));
        }
        StepsReader::Loop {
            field: LoopField::MaxIterations,
            ..
        } => {
            return Some(format!(
                "'{sym}': Steps.* symbols are not available in a loop's maxIterations, which is known at Job creation: it may read only Param.*, RawParam.*, and Job.Name."
            ));
        }
        _ => {}
    }
    let parts: Vec<&str> = sym.split('.').collect();
    if parts.len() < 4 {
        return Some(format!(
            "'{sym}' is not a complete Steps.* reference; expected Steps.<StepName>.Output.<Name>, .RawOutput.<Name>, .TaskParam.<Name>, or .TaskRawParam.<Name>."
        ));
    }
    let (p, member, name) = (parts[1], parts[2], parts[3]);
    let Some(named) = find_named(ctx.jt, p) else {
        return Some(format!("'{sym}': there is no Step named '{p}'."));
    };
    if !is_identifier(p) {
        return Some(format!(
            "'{sym}': Step '{p}' cannot be referenced through Steps.*, because its name is not an <Identifier>."
        ));
    }
    if let Some(message) = reader_error(sym, p, &named, ctx) {
        return Some(message);
    }
    let producer = match named {
        Named::TopLevel { producer, .. } => producer,
        Named::Body { step, .. } => Producer::Step(step),
    };
    let (found, raw_member) = match (member, producer) {
        ("Output" | "RawOutput", _) => {
            let Some(found) = output_symbol(producer, name) else {
                let noun = match producer {
                    Producer::Step(_) => "Step",
                    Producer::Loop(_) => "loop",
                };
                return Some(format!(
                    "'{sym}': {noun} '{p}' has no output variable '{name}'."
                ));
            };
            (found, "RawOutput")
        }
        ("TaskParam" | "TaskRawParam", Producer::Loop(_)) => {
            return Some(format!(
                "'{sym}': '{p}' is a loop, which has no Task parameters. Declare a loop output and read Steps.{p}.Output.<Name> instead."
            ));
        }
        ("TaskParam" | "TaskRawParam", Producer::Step(step)) => {
            if step.parameter_space.is_none() {
                return Some(format!(
                    "'{sym}': Step '{p}' has no parameterSpace, so it has no Task parameters."
                ));
            }
            let Some(found) = task_param_symbol(step, name) else {
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

/// Explain a `Loop.*` or `RawLoop.*` symbol that is not available (RFC 0013
/// §The `Loop.*` scope, §Validation item 8). The checks, in order: the field
/// may read a loop's symbols; the reference names a variable or
/// `Iteration`; `Iteration` is read as `Loop.Iteration`; the variable is
/// one of the readable loop's; and a path-typed `Loop.<V>` is not read in a
/// TEMPLATE-scope field.
fn loop_ref_error(sym: &str, ctx: &StepsRefContext<'_>) -> Option<String> {
    let Some(l) = readable_loop(ctx.jt, ctx.reader) else {
        return Some(match ctx.reader {
            StepsReader::Loop {
                field: LoopField::MaxIterations,
                ..
            } => format!(
                "'{sym}': Loop.* and RawLoop.* symbols are not available in a loop's maxIterations, which is known at Job creation: it may read only Param.*, RawParam.*, and Job.Name."
            ),
            StepsReader::Loop { field, .. } => format!(
                "'{sym}': Loop.* and RawLoop.* symbols are not available in a loop's {field}, which is evaluated before the loop's first iteration."
            ),
            _ => format!(
                "'{sym}': Loop.* and RawLoop.* symbols are available only in the body Steps of a loop and in the loop's while, until, next, and output values."
            ),
        });
    };
    let parts: Vec<&str> = sym.split('.').collect();
    let root = parts[0];
    let Some(&name) = parts.get(1) else {
        return Some(format!(
            "'{sym}' is not a complete reference; expected Loop.<VarName>, RawLoop.<VarName>, or Loop.Iteration."
        ));
    };
    if name == "Iteration" {
        return (root == "RawLoop")
            .then(|| format!("'{sym}': Loop.Iteration has no RawLoop form; use Loop.Iteration."));
    }
    let Some(var) = l.variables().iter().find(|v| v.name() == name) else {
        let elsewhere = ctx
            .jt
            .loops()
            .filter(|m| m.name != l.name)
            .find(|m| m.variables().iter().any(|v| v.name() == name))
            .map(|m| {
                format!(
                    " '{name}' is a variable of loop '{}', which is available only in that loop.",
                    m.name
                )
            })
            .unwrap_or_default();
        return Some(format!(
            "'{sym}': loop '{}' has no variable '{name}'.{elsewhere}",
            l.name
        ));
    };
    if root == "Loop"
        && is_path_type(var.job_param_type())
        && ctx.scope == StepsFieldScope::Template
    {
        return Some(format!(
            "'Loop.{name}' has a path type, so it is available only in fields resolved on the worker host. Use RawLoop.{name} here."
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
    /// a consumer `C` that depends on all four, `Z`, which depends on `C`
    /// only, and `Find Frames`, whose name is not an `<Identifier>`.
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
  - name: Find Frames
    outputVariables:
      - {name: Count, type: INT}
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
    fn job_output_scope_seeds_every_step_in_template_scope() {
        let jt = template();
        let mut s = SymbolTable::new();
        add_job_output_steps_scope(&mut s, &jt).unwrap();
        // No dependency needed: C reads P, Q, R, and M, but none of them
        // depends on anything, and all are seeded.
        assert_eq!(ty(&s, "Steps.P.Output.Count"), Some(ExprType::INT));
        assert_eq!(
            ty(&s, "Steps.Q.TaskParam.Frame"),
            Some(ExprType::list(ExprType::INT))
        );
        assert_eq!(ty(&s, "Steps.R.Output.B"), Some(opt(ExprType::BOOL)));
        assert_eq!(
            ty(&s, "Steps.M.TaskParam.S"),
            Some(opt(ExprType::list(ExprType::STRING)))
        );
        // Path-typed values only in their Raw forms.
        assert_eq!(ty(&s, "Steps.P.Output.F"), None);
        assert_eq!(ty(&s, "Steps.Q.TaskParam.D"), None);
        assert_eq!(ty(&s, "Steps.P.RawOutput.F"), Some(ExprType::PATH));
        assert_eq!(
            ty(&s, "Steps.Q.TaskRawParam.D"),
            Some(ExprType::list(ExprType::PATH))
        );
        // Only Steps with an <Identifier> name.
        let paths = s.all_paths("");
        assert!(paths.iter().all(|p| !p.contains("Find")), "{paths:?}");
    }

    /// A job template with three loops: `U` (`until`), `Wl` (`while`), and
    /// `E` (`until` and `enableIf`), each with outputs `Last: INT` (`LAST`),
    /// `All: LIST[INT]` (`APPEND`), and `P: PATH` (`LAST`). `U` depends on
    /// `S` and has variables `V: INT`, `P: PATH`, and `G: LIST[LIST[INT]]`
    /// and body Steps `A` (output `X: INT`) and `B` (depending on `A`). `C`
    /// depends on the three loops.
    const LOOPS: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
  - name: S
    outputVariables: [{name: K, type: INT}]
    script: {actions: {onRun: {command: x}}}
  - name: U
    dependencies: [{dependsOn: S}]
    loop:
      variables:
        - {name: V, type: INT, initial: "1", next: "1"}
        - {name: P, type: PATH, initial: "p", next: "p"}
        - {name: G, type: "LIST[LIST[INT]]", initial: "{{[[1]]}}", next: "{{[[1]]}}"}
      until: true
      maxIterations: 2
      steps:
        - name: A
          outputVariables: [{name: X, type: INT}]
          script: {actions: {onRun: {command: x}}}
        - name: B
          dependencies: [{dependsOn: A}]
          script: {actions: {onRun: {command: x}}}
    outputVariables: &outs
      - {name: Last, type: INT, collect: LAST, value: "1"}
      - {name: All, type: "LIST[INT]", collect: APPEND, value: "1"}
      - {name: P, type: PATH, collect: LAST, value: "p"}
  - name: Wl
    loop:
      while: false
      maxIterations: 2
      steps:
        - name: WA
          script: {actions: {onRun: {command: x}}}
    outputVariables: *outs
  - name: E
    enableIf: true
    loop:
      until: true
      maxIterations: 2
      steps:
        - name: EA
          script: {actions: {onRun: {command: x}}}
    outputVariables: *outs
  - name: C
    dependencies: [{dependsOn: U}, {dependsOn: Wl}, {dependsOn: E}]
    script: {actions: {onRun: {command: x}}}
"#;

    fn loops_template() -> JobTemplate {
        crate::decode_job_template(
            serde_saphyr::from_str(LOOPS).unwrap(),
            Some(&["EXPR", "CONTROL_FLOW"]),
            &CallerLimits::default(),
        )
        .expect("valid template")
    }

    fn seeded_for(reader: StepsReader, scope: StepsFieldScope) -> SymbolTable {
        let jt = loops_template();
        let mut symtab = SymbolTable::new();
        add_control_flow_scope(&mut symtab, &jt, reader, scope).unwrap();
        symtab
    }

    #[test]
    fn loop_output_types_follow_the_rfc_0013_table() {
        let s = seeded_for(
            StepsReader::Step(StepLocation::top(4)),
            StepsFieldScope::Host,
        );
        let list = ExprType::list;
        let cases = [
            // until, no enableIf: T and list[E].
            ("Steps.U.Output.Last", ExprType::INT),
            ("Steps.U.Output.All", list(ExprType::INT)),
            ("Steps.U.Output.P", ExprType::PATH),
            ("Steps.U.RawOutput.P", ExprType::PATH),
            // while, no enableIf: T? and list[E].
            ("Steps.Wl.Output.Last", opt(ExprType::INT)),
            ("Steps.Wl.Output.All", list(ExprType::INT)),
            // enableIf: T? and list[E]?.
            ("Steps.E.Output.Last", opt(ExprType::INT)),
            ("Steps.E.Output.All", opt(list(ExprType::INT))),
        ];
        for (key, expected) in cases {
            assert_eq!(ty(&s, key), Some(expected), "{key}");
        }
        // A loop has no Task parameters, and its body is not visible.
        assert!(s.get_table("Steps.U.TaskParam").is_none());
        assert!(s.get_table("Steps.A").is_none());
        assert!(s.get_table("Loop").is_none());
        // TEMPLATE scope keeps only the Raw form of a path output.
        let t = seeded_for(
            StepsReader::Step(StepLocation::top(4)),
            StepsFieldScope::Template,
        );
        assert_eq!(ty(&t, "Steps.U.Output.P"), None);
        assert_eq!(ty(&t, "Steps.U.RawOutput.P"), Some(ExprType::PATH));
    }

    #[test]
    fn a_body_step_sees_its_body_dependencies_its_loops_dependencies_and_loop_symbols() {
        let b = StepsReader::Step(StepLocation {
            entry: 1,
            body: Some(1),
        });
        let s = seeded_for(b, StepsFieldScope::Host);
        assert_eq!(ty(&s, "Steps.A.Output.X"), Some(ExprType::INT));
        assert_eq!(ty(&s, "Steps.S.Output.K"), Some(ExprType::INT));
        assert!(s.get_table("Steps.U").is_none());
        assert!(s.get_table("Steps.B").is_none());
        assert_eq!(ty(&s, "Loop.Iteration"), Some(ExprType::INT));
        assert_eq!(ty(&s, "Loop.V"), Some(ExprType::INT));
        assert_eq!(ty(&s, "RawLoop.V"), Some(ExprType::INT));
        assert_eq!(ty(&s, "Loop.P"), Some(ExprType::PATH));
        assert_eq!(ty(&s, "RawLoop.P"), Some(ExprType::PATH));
        assert_eq!(
            ty(&s, "Loop.G"),
            Some(ExprType::list(ExprType::list(ExprType::INT)))
        );
        // TEMPLATE scope: a path-typed Loop.<V> only through RawLoop.
        let t = seeded_for(b, StepsFieldScope::Template);
        assert_eq!(ty(&t, "Loop.P"), None);
        assert_eq!(ty(&t, "RawLoop.P"), Some(ExprType::PATH));
        // A body Step with no dependencies sees only its loop's.
        let a = seeded_for(
            StepsReader::Step(StepLocation {
                entry: 1,
                body: Some(0),
            }),
            StepsFieldScope::Host,
        );
        assert!(a.get_table("Steps.A").is_none());
        assert_eq!(ty(&a, "Steps.S.Output.K"), Some(ExprType::INT));
    }

    #[test]
    fn each_loop_field_sees_what_rfc_0013_allows() {
        let field = |field: LoopField| {
            seeded_for(
                StepsReader::Loop { entry: 1, field },
                StepsFieldScope::Template,
            )
        };
        for f in [LoopField::EnableIf, LoopField::Initial] {
            let s = field(f);
            assert_eq!(ty(&s, "Steps.S.Output.K"), Some(ExprType::INT), "{f}");
            assert!(s.get_table("Steps.A").is_none(), "{f}");
            assert!(s.get_table("Loop").is_none(), "{f}");
        }
        let s = field(LoopField::While);
        assert_eq!(ty(&s, "Steps.S.Output.K"), Some(ExprType::INT));
        assert!(s.get_table("Steps.A").is_none());
        assert_eq!(ty(&s, "Loop.V"), Some(ExprType::INT));
        assert_eq!(ty(&s, "Loop.P"), None);
        for f in [LoopField::Until, LoopField::Next, LoopField::OutputValue] {
            let s = field(f);
            assert_eq!(ty(&s, "Steps.S.Output.K"), Some(ExprType::INT), "{f}");
            assert_eq!(ty(&s, "Steps.A.Output.X"), Some(ExprType::INT), "{f}");
            assert_eq!(ty(&s, "Loop.Iteration"), Some(ExprType::INT), "{f}");
            assert_eq!(ty(&s, "RawLoop.P"), Some(ExprType::PATH), "{f}");
        }
        let s = field(LoopField::MaxIterations);
        assert!(s.get_table("Steps").is_none());
        assert!(s.get_table("Loop").is_none());
    }

    #[test]
    fn a_job_output_sees_loops_and_not_their_body_steps() {
        let jt = loops_template();
        let mut s = SymbolTable::new();
        add_job_output_steps_scope(&mut s, &jt).unwrap();
        assert_eq!(ty(&s, "Steps.U.Output.Last"), Some(ExprType::INT));
        assert_eq!(ty(&s, "Steps.S.Output.K"), Some(ExprType::INT));
        assert!(s.get_table("Steps.A").is_none());
        assert!(s.get_table("Loop").is_none());
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
