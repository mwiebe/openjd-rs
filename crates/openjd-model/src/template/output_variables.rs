// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Template types the `CONTROL_FLOW` extension adds (RFC 0011, RFC 0012,
//! RFC 0013): `<OutputVariableDefinition>`, `<JobOutputDefinition>`,
//! `<LoopVariableDefinition>`, `<LoopOutputVariableDefinition>`, and the
//! `enableIf` field value.
//!
//! An output variable reuses the job-parameter machinery instead of a
//! parallel set of structs. Its mapping is the `<JobParameterDefinition>` of
//! the same type plus `collect` and `unit`, so [`OutputVariableDefinition`]
//! takes those two keys out of the mapping and deserializes the rest as a
//! [`JobParameterDefinition`], which would otherwise reject them as unknown.
//! It also takes out `default`, `userInterface`, and (for `PATH` and
//! `LIST[PATH]`) `dataFlow`, which an output variable may not have, and
//! records them so validation can report each one at its own path.
//!
//! A [`JobOutputDefinition`] is the same mapping with `value` in place of
//! `collect`, and takes the same route. So do the loop definitions of RFC
//! 0013: a [`LoopVariableDefinition`] takes `initial` and `next` out of the
//! mapping, and a [`LoopOutputVariableDefinition`] takes `collect`, `value`,
//! and `unit`.

use openjd_expr::value::Float64;
use serde::Deserialize;

use super::expr_parameters::{ListFloatItemConstraints, ListIntItemConstraints};
use super::parameters::{FlexFloat, FlexInt, JobParameterDefinition};
use crate::format_string::FormatString;
use crate::job::{JobOutput, OutputAllowedValues, OutputBound, OutputConstraints, OutputVariable};
use crate::types::{JobParameterType, OutputCollectMode};

/// A field that is either a YAML `bool` or a format string, such as
/// `enableIf` (RFC 0011 `<StepTemplate>` and `<Environment>`).
#[derive(Debug, Clone)]
pub enum BoolOrFormatString {
    /// A literal `true` or `false`.
    Bool(bool),
    /// A format string, evaluated with target type `bool`.
    FormatString(FormatString),
}

impl<'de> Deserialize<'de> for BoolOrFormatString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Bool(b) => Ok(Self::Bool(b)),
            serde_json::Value::String(s) => FormatString::new(&s)
                .map(Self::FormatString)
                .map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "expected a bool or a format string, got {}",
                json_kind(&other)
            ))),
        }
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a bool",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "a mapping",
    }
}

/// RFC 0012 `<OutputVariableDefinition>`: one output variable a Step's
/// Tasks report.
///
/// Wraps the [`JobParameterDefinition`] of the declared type, which holds
/// `name`, `description`, and the constraint properties, together with
/// `collect` and `unit`.
#[derive(Debug, Clone)]
pub struct OutputVariableDefinition {
    definition: JobParameterDefinition,
    /// The job-side form, built once: name, type, `collect`, `unit`, and the
    /// declared constraints.
    job: OutputVariable,
    /// Job-parameter properties the author wrote that an output variable may
    /// not have, in the order of [`UNAVAILABLE_PROPERTIES`].
    unavailable: Vec<&'static str>,
}

/// Job-parameter properties that an output variable may not have. `dataFlow`
/// is a property of `PATH` and `LIST[PATH]` only; on other types the
/// job-parameter definition already rejects it as an unknown field.
const UNAVAILABLE_PROPERTIES: [&str; 3] = ["default", "userInterface", "dataFlow"];

/// What every definition kind shares, parsed from one mapping: the wrapped
/// job parameter definition, the job-side declaration, the job-parameter
/// properties the author wrote that the definition may not have, and the
/// `collect` mode as written (only a loop output keeps it; it is `None`
/// for the other kinds).
struct ParsedDefinition {
    definition: JobParameterDefinition,
    job: OutputVariable,
    unavailable: Vec<&'static str>,
    written_collect: Option<OutputCollectMode>,
}

/// Which definition a mapping is, for the keys it takes out before the rest
/// is deserialized as a [`JobParameterDefinition`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum DefinitionKind {
    /// `<OutputVariableDefinition>`: takes `collect` and `unit`.
    OutputVariable,
    /// `<JobOutputDefinition>`: takes `unit`; `collect` is not available.
    JobOutput,
    /// `<LoopVariableDefinition>` (RFC 0013): `collect` and `unit` are not
    /// available.
    LoopVariable,
    /// `<LoopOutputVariableDefinition>` (RFC 0013): takes `collect`, as
    /// written, and `unit`.
    LoopOutput,
}

impl DefinitionKind {
    fn noun(self) -> &'static str {
        match self {
            Self::OutputVariable => "output variable",
            Self::JobOutput => "job output",
            Self::LoopVariable => "loop variable",
            Self::LoopOutput => "loop output",
        }
    }

    /// `an output variable definition`, `a job output definition`, and so on.
    fn definition_phrase(self) -> &'static str {
        match self {
            Self::OutputVariable => "an output variable definition",
            Self::JobOutput => "a job output definition",
            Self::LoopVariable => "a loop variable definition",
            Self::LoopOutput => "a loop output definition",
        }
    }

    /// The keys this kind adds to a job parameter definition's.
    fn own_fields(self) -> &'static [&'static str] {
        match self {
            Self::OutputVariable => &["collect", "unit"],
            Self::JobOutput => &["value", "unit"],
            Self::LoopVariable => &["initial", "next"],
            Self::LoopOutput => &["collect", "value", "unit"],
        }
    }
}

/// Parse the mapping of an output variable or job output definition, with
/// `map` holding every key except the ones the caller already took out
/// (`value` for a job output).
fn parse_definition(
    mut map: serde_json::Map<String, serde_json::Value>,
    kind: DefinitionKind,
) -> Result<ParsedDefinition, String> {
    let noun = kind.noun();
    let type_str = match map.get("type") {
        None => return Err(format!("missing 'type' field in {noun} definition")),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => return Err(format!("'type' must be a string, got {}", json_kind(other))),
    };
    let param_type = JobParameterType::from_spec_str(&type_str)
        .ok_or_else(|| format!("unknown {noun} type: '{type_str}'"))?;

    let mut unavailable = Vec::new();
    // An explicit `null` means omitted, as for the other optional fields.
    let parse_collect = |v: serde_json::Value| -> Result<OutputCollectMode, String> {
        serde_json::from_value(v).map_err(|e| format!("collect: {e}"))
    };
    let written_collect = match kind {
        DefinitionKind::OutputVariable | DefinitionKind::LoopOutput => {
            match map.remove("collect") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => Some(parse_collect(v)?),
            }
        }
        // A job output is evaluated, not reported, so it has no collect
        // mode (RFC 0011 `<JobOutputDefinition>`), and neither has a loop
        // variable (RFC 0013 `<LoopVariableDefinition>`). Reported by
        // validation at its own path.
        DefinitionKind::JobOutput | DefinitionKind::LoopVariable => {
            if map.remove("collect").is_some_and(|v| !v.is_null()) {
                unavailable.push("collect");
            }
            None
        }
    };
    let collect = written_collect.unwrap_or_default();
    let unit = match map.remove("unit") {
        None | Some(serde_json::Value::Null) => None,
        // A loop variable has no `unit` (RFC 0013 `<LoopVariableDefinition>`).
        Some(_) if kind == DefinitionKind::LoopVariable => {
            unavailable.push("unit");
            None
        }
        Some(serde_json::Value::String(s)) => Some(s),
        Some(other) => {
            return Err(format!(
                "unit: expected a string, got {}",
                json_kind(&other)
            ))
        }
    };

    let is_path = matches!(
        param_type,
        JobParameterType::Path | JobParameterType::ListPath
    );
    for key in UNAVAILABLE_PROPERTIES {
        if key == "dataFlow" && !is_path {
            continue;
        }
        if map.remove(key).is_some_and(|v| !v.is_null()) {
            unavailable.push(key);
        }
    }

    let definition: JobParameterDefinition = serde_json::from_value(serde_json::Value::Object(map))
        .map_err(|e| relist_unknown_field(&e.to_string(), kind))?;
    let constraints = constraints_of(&definition)?;
    let job = OutputVariable {
        name: definition.name().to_string(),
        param_type,
        collect,
        unit,
        constraints,
    };
    Ok(ParsedDefinition {
        definition,
        job,
        unavailable,
        // Only a loop output keeps the mode as written: its `collect` is
        // required, and `ONCE` is not allowed (validation reports both).
        written_collect: (kind == DefinitionKind::LoopOutput)
            .then_some(written_collect)
            .flatten(),
    })
}

fn into_map<E: serde::de::Error>(
    value: serde_json::Value,
    kind: DefinitionKind,
) -> Result<serde_json::Map<String, serde_json::Value>, E> {
    match value {
        serde_json::Value::Object(map) => Ok(map),
        other => Err(E::custom(format!(
            "expected {} mapping, got {}",
            kind.definition_phrase(),
            json_kind(&other)
        ))),
    }
}

impl<'de> Deserialize<'de> for OutputVariableDefinition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let map = into_map(
            serde_json::Value::deserialize(deserializer)?,
            DefinitionKind::OutputVariable,
        )?;
        let parsed =
            parse_definition(map, DefinitionKind::OutputVariable).map_err(D::Error::custom)?;
        Ok(Self {
            definition: parsed.definition,
            job: parsed.job,
            unavailable: parsed.unavailable,
        })
    }
}

impl OutputVariableDefinition {
    /// The output's `<Identifier>` name.
    pub fn name(&self) -> &str {
        self.definition.name()
    }

    /// The declared `<OutputVariableType>`.
    pub fn job_param_type(&self) -> JobParameterType {
        self.definition.job_param_type()
    }

    /// The `collect` mode (`ONCE` when omitted).
    pub fn collect(&self) -> OutputCollectMode {
        self.job.collect
    }

    /// The display `unit`, if declared.
    pub fn unit(&self) -> Option<&str> {
        self.job.unit.as_deref()
    }

    /// The `description`, if declared.
    pub fn description(&self) -> Option<&str> {
        self.definition.description()
    }

    /// The job-parameter definition of the declared type, which holds the
    /// name, description, constraint properties, and (for `PATH` and
    /// `LIST[PATH]`) `objectType`.
    pub fn definition(&self) -> &JobParameterDefinition {
        &self.definition
    }

    /// The declared constraint properties.
    pub fn constraints(&self) -> &OutputConstraints {
        &self.job.constraints
    }

    /// The job-side form `create_job` stores on `job::Step`.
    pub fn to_job_output_variable(&self) -> OutputVariable {
        self.job.clone()
    }

    /// Check one Task's whole value against the declared type and the
    /// declared constraint properties. See [`OutputVariable::check_constraints`].
    pub fn check_constraints(&self, value: &openjd_expr::ExprValue) -> Result<(), String> {
        self.job.check_constraints(value)
    }

    /// Check one element reported under `collect: APPEND`. See
    /// [`OutputVariable::check_element`].
    pub fn check_element(&self, element: &openjd_expr::ExprValue) -> Result<(), String> {
        self.job.check_element(element)
    }

    /// Check the length of a list being collected under `collect: APPEND`.
    /// See [`OutputVariable::check_length`].
    pub fn check_length(&self, len: usize, at_exit: bool) -> Result<(), String> {
        self.job.check_length(len, at_exit)
    }

    /// The job-parameter properties the author wrote that an output variable
    /// may not have (`default`, `userInterface`, `dataFlow`).
    pub(crate) fn unavailable_properties(&self) -> &[&'static str] {
        &self.unavailable
    }
}

/// RFC 0011 `<JobOutputDefinition>`: one of the Job's outputs, a typed value
/// computed from its parameters and its Steps' outputs (Template Schemas
/// §2.17).
///
/// Wraps the [`JobParameterDefinition`] of the declared type, which holds
/// `name`, `description`, and the constraint properties, together with
/// `value` and `unit`. A job output has no `collect`: it is evaluated, not
/// reported.
#[derive(Debug, Clone)]
pub struct JobOutputDefinition {
    definition: JobParameterDefinition,
    /// The declaration as an output variable: name, type, `unit`, and the
    /// declared constraints (`collect` is always the default).
    job: OutputVariable,
    value: FormatString,
    /// Properties the author wrote that a job output may not have, in the
    /// order `collect`, then [`UNAVAILABLE_PROPERTIES`].
    unavailable: Vec<&'static str>,
}

impl<'de> Deserialize<'de> for JobOutputDefinition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut map = into_map(
            serde_json::Value::deserialize(deserializer)?,
            DefinitionKind::JobOutput,
        )?;
        let value = take_format_string(&mut map, "value", DefinitionKind::JobOutput)
            .map_err(D::Error::custom)?;
        let parsed = parse_definition(map, DefinitionKind::JobOutput).map_err(D::Error::custom)?;
        Ok(Self {
            definition: parsed.definition,
            job: parsed.job,
            value,
            unavailable: parsed.unavailable,
        })
    }
}

impl JobOutputDefinition {
    /// The output's `<Identifier>` name.
    pub fn name(&self) -> &str {
        self.definition.name()
    }

    /// The declared `<OutputVariableType>`.
    pub fn job_param_type(&self) -> JobParameterType {
        self.definition.job_param_type()
    }

    /// The `value` format string.
    pub fn value(&self) -> &FormatString {
        &self.value
    }

    /// The display `unit`, if declared.
    pub fn unit(&self) -> Option<&str> {
        self.job.unit.as_deref()
    }

    /// The `description`, if declared.
    pub fn description(&self) -> Option<&str> {
        self.definition.description()
    }

    /// The job-parameter definition of the declared type, which holds the
    /// name, description, constraint properties, and (for `PATH` and
    /// `LIST[PATH]`) `objectType`.
    pub fn definition(&self) -> &JobParameterDefinition {
        &self.definition
    }

    /// The declared constraint properties.
    pub fn constraints(&self) -> &OutputConstraints {
        &self.job.constraints
    }

    /// The job-side form `create_job` stores in [`job::Job::outputs`](crate::job::Job::outputs),
    /// not yet evaluated.
    pub fn to_job_output(&self) -> JobOutput {
        JobOutput {
            declaration: self.job.clone(),
            object_type: self.definition.path_properties().0,
            description: self.description().map(str::to_string),
            value: self.value.clone(),
            evaluated: None,
        }
    }

    /// The properties the author wrote that a job output may not have
    /// (`collect`, `default`, `userInterface`, `dataFlow`).
    pub(crate) fn unavailable_properties(&self) -> &[&'static str] {
        &self.unavailable
    }
}

/// Take the required format-string field `key` out of a definition mapping.
/// An explicit `null` is missing, as for the other fields.
fn take_format_string(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    kind: DefinitionKind,
) -> Result<FormatString, String> {
    match map.remove(key) {
        None | Some(serde_json::Value::Null) => Err(format!(
            "missing '{key}' field in {} definition",
            kind.noun()
        )),
        Some(v) => FormatString::deserialize(v).map_err(|e| format!("{key}: {e}")),
    }
}

/// RFC 0013 `<LoopVariableDefinition>`: one variable of a loop, read as
/// `Loop.<name>` and `RawLoop.<name>`, set by `initial` before the first
/// iteration and by `next` at each iteration boundary.
///
/// Wraps the [`JobParameterDefinition`] of the declared type, which holds
/// `name`, `description`, and the constraint properties, together with
/// `initial` and `next`. A loop variable has no `collect` or `unit`.
#[derive(Debug, Clone)]
pub struct LoopVariableDefinition {
    definition: JobParameterDefinition,
    /// The declaration as an output variable: name, type, and the declared
    /// constraints (`collect` is always the default, `unit` always unset).
    job: OutputVariable,
    initial: FormatString,
    next: FormatString,
    /// Properties the author wrote that a loop variable may not have, in the
    /// order `collect`, `unit`, then [`UNAVAILABLE_PROPERTIES`].
    unavailable: Vec<&'static str>,
}

impl<'de> Deserialize<'de> for LoopVariableDefinition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let kind = DefinitionKind::LoopVariable;
        let mut map = into_map(serde_json::Value::deserialize(deserializer)?, kind)?;
        let initial = take_format_string(&mut map, "initial", kind).map_err(D::Error::custom)?;
        let next = take_format_string(&mut map, "next", kind).map_err(D::Error::custom)?;
        let parsed = parse_definition(map, kind).map_err(D::Error::custom)?;
        Ok(Self {
            definition: parsed.definition,
            job: parsed.job,
            initial,
            next,
            unavailable: parsed.unavailable,
        })
    }
}

impl LoopVariableDefinition {
    /// The variable's `<Identifier>` name.
    pub fn name(&self) -> &str {
        self.definition.name()
    }

    /// The declared `<OutputVariableType>`.
    pub fn job_param_type(&self) -> JobParameterType {
        self.definition.job_param_type()
    }

    /// The `initial` format string: the value for iteration 0.
    pub fn initial(&self) -> &FormatString {
        &self.initial
    }

    /// The `next` format string: the value for the following iteration.
    pub fn next(&self) -> &FormatString {
        &self.next
    }

    /// The `description`, if declared.
    pub fn description(&self) -> Option<&str> {
        self.definition.description()
    }

    /// The job-parameter definition of the declared type, which holds the
    /// name, description, constraint properties, and (for `PATH` and
    /// `LIST[PATH]`) `objectType`.
    pub fn definition(&self) -> &JobParameterDefinition {
        &self.definition
    }

    /// The declared constraint properties.
    pub fn constraints(&self) -> &OutputConstraints {
        &self.job.constraints
    }

    /// The declaration as an [`OutputVariable`] (name, type, and
    /// constraints), whose `check_constraints` checks every value the
    /// variable takes.
    pub fn to_output_variable(&self) -> OutputVariable {
        self.job.clone()
    }

    /// The properties the author wrote that a loop variable may not have
    /// (`collect`, `unit`, `default`, `userInterface`, `dataFlow`).
    pub(crate) fn unavailable_properties(&self) -> &[&'static str] {
        &self.unavailable
    }
}

/// RFC 0013 `<LoopOutputVariableDefinition>`: one output of a loop, read
/// after the loop as `Steps.<LoopName>.Output.<name>`.
///
/// Wraps the [`JobParameterDefinition`] of the declared type, which holds
/// `name`, `description`, and the constraint properties, together with
/// `collect`, `value`, and `unit`. `collect` is kept as written: it is
/// required, and only `LAST` and `APPEND` are allowed, which validation
/// checks at its own path.
#[derive(Debug, Clone)]
pub struct LoopOutputVariableDefinition {
    definition: JobParameterDefinition,
    /// The declaration as an output variable: name, type, `collect` (the
    /// default when not written), `unit`, and the declared constraints.
    job: OutputVariable,
    collect: Option<OutputCollectMode>,
    value: FormatString,
    /// Properties the author wrote that a loop output may not have, in the
    /// order of [`UNAVAILABLE_PROPERTIES`].
    unavailable: Vec<&'static str>,
}

impl<'de> Deserialize<'de> for LoopOutputVariableDefinition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let kind = DefinitionKind::LoopOutput;
        let mut map = into_map(serde_json::Value::deserialize(deserializer)?, kind)?;
        let value = take_format_string(&mut map, "value", kind).map_err(D::Error::custom)?;
        let parsed = parse_definition(map, kind).map_err(D::Error::custom)?;
        Ok(Self {
            definition: parsed.definition,
            job: parsed.job,
            collect: parsed.written_collect,
            value,
            unavailable: parsed.unavailable,
        })
    }
}

impl LoopOutputVariableDefinition {
    /// The output's `<Identifier>` name.
    pub fn name(&self) -> &str {
        self.definition.name()
    }

    /// The declared `<OutputVariableType>`.
    pub fn job_param_type(&self) -> JobParameterType {
        self.definition.job_param_type()
    }

    /// The `collect` mode as written, `None` when it was omitted. A valid
    /// template has `LAST` or `APPEND`.
    pub fn collect(&self) -> Option<OutputCollectMode> {
        self.collect
    }

    /// The `value` format string, evaluated at each iteration boundary.
    pub fn value(&self) -> &FormatString {
        &self.value
    }

    /// The display `unit`, if declared.
    pub fn unit(&self) -> Option<&str> {
        self.job.unit.as_deref()
    }

    /// The `description`, if declared.
    pub fn description(&self) -> Option<&str> {
        self.definition.description()
    }

    /// The job-parameter definition of the declared type, which holds the
    /// name, description, constraint properties, and (for `PATH` and
    /// `LIST[PATH]`) `objectType`.
    pub fn definition(&self) -> &JobParameterDefinition {
        &self.definition
    }

    /// The declared constraint properties.
    pub fn constraints(&self) -> &OutputConstraints {
        &self.job.constraints
    }

    /// The declaration as an [`OutputVariable`], whose `check_constraints`,
    /// `check_element`, and `check_length` apply the constraint properties
    /// as RFC 0013 §`<LoopOutputVariableDefinition>` item 6 says.
    pub fn to_output_variable(&self) -> OutputVariable {
        self.job.clone()
    }

    /// The properties the author wrote that a loop output may not have
    /// (`default`, `userInterface`, `dataFlow`).
    pub(crate) fn unavailable_properties(&self) -> &[&'static str] {
        &self.unavailable
    }
}

/// serde's unknown-field message for the wrapped job parameter definition
/// lists the fields that definition accepts, including `default` and
/// `userInterface`, which an output variable or job output may not have, and
/// not the fields `kind` adds (`collect` and `unit`, or `value` and `unit`).
/// Rewrite the list to the fields of `kind`.
/// Any other message, including one about a nested `item` mapping, is
/// returned unchanged.
fn relist_unknown_field(msg: &str, kind: DefinitionKind) -> String {
    let Some(rest) = msg.strip_prefix("unknown field `") else {
        return msg.to_string();
    };
    let Some((field, tail)) = rest.split_once('`') else {
        return msg.to_string();
    };
    let Some(expected) = tail.strip_prefix(", expected ") else {
        return msg.to_string();
    };
    let written: Vec<&str> = expected.split('`').skip(1).step_by(2).collect();
    // Only the definition's own fields include `name`; a nested `item`
    // mapping's message is about that mapping and is left alone.
    if !written.contains(&"name") {
        return msg.to_string();
    }
    let mut names: Vec<&str> = written
        .into_iter()
        .filter(|n| !UNAVAILABLE_PROPERTIES.contains(n))
        .collect();
    names.extend(kind.own_fields());
    let list: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
    format!(
        "unknown field `{field}`, expected one of {}",
        list.join(", ")
    )
}

fn float64(f: &FlexFloat) -> Result<Float64, String> {
    Float64::new(f.0).map_err(|e| e.to_string())
}

fn int_items(item: &ListIntItemConstraints) -> OutputConstraints {
    OutputConstraints {
        allowed_values: item
            .allowed_values
            .as_ref()
            .map(|v| OutputAllowedValues::Int(v.iter().map(|x: &FlexInt| x.0).collect())),
        min_value: item.min_value.as_ref().map(|v| OutputBound::Int(v.0)),
        max_value: item.max_value.as_ref().map(|v| OutputBound::Int(v.0)),
        ..Default::default()
    }
}

fn float_items(item: &ListFloatItemConstraints) -> Result<OutputConstraints, String> {
    Ok(OutputConstraints {
        allowed_values: item
            .allowed_values
            .as_ref()
            .map(|v| v.iter().map(float64).collect::<Result<Vec<_>, _>>())
            .transpose()?
            .map(OutputAllowedValues::Float),
        min_value: item
            .min_value
            .as_ref()
            .map(float64)
            .transpose()?
            .map(OutputBound::Float),
        max_value: item
            .max_value
            .as_ref()
            .map(float64)
            .transpose()?
            .map(OutputBound::Float),
        ..Default::default()
    })
}

/// The constraint properties `def` declares, and only those.
fn constraints_of(def: &JobParameterDefinition) -> Result<OutputConstraints, String> {
    use JobParameterDefinition as D;
    let some_box = |c: OutputConstraints| (!c.is_empty()).then(|| Box::new(c));
    Ok(match def {
        D::STRING(p) => OutputConstraints {
            allowed_values: p.allowed_values.clone().map(OutputAllowedValues::String),
            min_length: p.min_length,
            max_length: p.max_length,
            ..Default::default()
        },
        D::PATH(p) => OutputConstraints {
            allowed_values: p.allowed_values.clone().map(OutputAllowedValues::String),
            min_length: p.min_length,
            max_length: p.max_length,
            ..Default::default()
        },
        D::INT(p) => OutputConstraints {
            allowed_values: p
                .allowed_values
                .as_ref()
                .map(|v| OutputAllowedValues::Int(v.iter().map(|x| x.0).collect())),
            min_value: p.min_value.as_ref().map(|v| OutputBound::Int(v.0)),
            max_value: p.max_value.as_ref().map(|v| OutputBound::Int(v.0)),
            ..Default::default()
        },
        D::FLOAT(p) => OutputConstraints {
            allowed_values: p
                .allowed_values
                .as_ref()
                .map(|v| v.iter().map(float64).collect::<Result<Vec<_>, _>>())
                .transpose()?
                .map(OutputAllowedValues::Float),
            min_value: p
                .min_value
                .as_ref()
                .map(float64)
                .transpose()?
                .map(OutputBound::Float),
            max_value: p
                .max_value
                .as_ref()
                .map(float64)
                .transpose()?
                .map(OutputBound::Float),
            ..Default::default()
        },
        D::BOOL(_) => OutputConstraints::default(),
        D::RANGE_EXPR(p) => OutputConstraints {
            min_length: p.min_length,
            max_length: p.max_length,
            ..Default::default()
        },
        D::LIST_STRING(p) => OutputConstraints {
            min_length: p.min_length,
            max_length: p.max_length,
            item: p.item.as_ref().and_then(|i| {
                some_box(OutputConstraints {
                    allowed_values: i.allowed_values.clone().map(OutputAllowedValues::String),
                    min_length: i.min_length,
                    max_length: i.max_length,
                    ..Default::default()
                })
            }),
            ..Default::default()
        },
        D::LIST_PATH(p) => OutputConstraints {
            min_length: p.min_length,
            max_length: p.max_length,
            item: p.item.as_ref().and_then(|i| {
                some_box(OutputConstraints {
                    allowed_values: i.allowed_values.clone().map(OutputAllowedValues::String),
                    min_length: i.min_length,
                    max_length: i.max_length,
                    ..Default::default()
                })
            }),
            ..Default::default()
        },
        D::LIST_INT(p) => OutputConstraints {
            min_length: p.min_length,
            max_length: p.max_length,
            item: p.item.as_ref().and_then(|i| some_box(int_items(i))),
            ..Default::default()
        },
        D::LIST_FLOAT(p) => OutputConstraints {
            min_length: p.min_length,
            max_length: p.max_length,
            item: match p.item.as_ref() {
                Some(i) => some_box(float_items(i)?),
                None => None,
            },
            ..Default::default()
        },
        D::LIST_BOOL(p) => OutputConstraints {
            min_length: p.min_length,
            max_length: p.max_length,
            ..Default::default()
        },
        D::LIST_LIST_INT(p) => OutputConstraints {
            min_length: p.min_length,
            max_length: p.max_length,
            item: p.item.as_ref().and_then(|i| {
                some_box(OutputConstraints {
                    min_length: i.min_length,
                    max_length: i.max_length,
                    item: i.item.as_ref().and_then(|ii| some_box(int_items(ii))),
                    ..Default::default()
                })
            }),
            ..Default::default()
        },
    })
}
