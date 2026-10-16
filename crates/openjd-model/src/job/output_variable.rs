// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Job-side `CONTROL_FLOW` types (RFC 0010): a Step's output variable
//! declarations, its binding state, and the path-typed `Steps.*` inputs it
//! reads.
//!
//! `job::Step` derives `PartialEq`, `Hash`, `Serialize`, and `Deserialize`, and
//! the template's `JobParameterDefinition` implements none of the first three,
//! so an output declaration is carried here in its own form:
//! [`OutputVariable`] holds the declared constraint properties as
//! [`OutputConstraints`], which is hashable and serializable.

use openjd_expr::value::Float64;
use openjd_expr::{ExprType, ExprValue};
use serde::{Deserialize, Serialize};

use crate::types::{JobParameterType, OutputCollectMode};

/// One output variable declared by a Step (RFC 0010 `<OutputVariableDefinition>`),
/// in the form `create_job` stores on [`job::Step`](super::Step).
///
/// The constraint checks apply only the properties the definition declared:
/// no property that a job parameter of the same type implies when omitted is
/// checked (RFC 0010 `<OutputVariableDefinition>` item 6).
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputVariable {
    /// The `<Identifier>` the output is reported and referenced by.
    pub name: String,
    /// The declared `<OutputVariableType>`. Serialized as the spec string
    /// (`"INT"`, `"LIST[STRING]"`), as in the template.
    #[serde(rename = "type", with = "job_param_type_spec")]
    pub param_type: JobParameterType,
    /// How a Task's directives for this output become its value.
    #[serde(default, skip_serializing_if = "is_default_collect")]
    pub collect: OutputCollectMode,
    /// The display unit, if declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The constraint properties the definition declared.
    #[serde(default, skip_serializing_if = "OutputConstraints::is_empty")]
    pub constraints: OutputConstraints,
}

fn is_default_collect(c: &OutputCollectMode) -> bool {
    *c == OutputCollectMode::default()
}

/// Serialize a [`JobParameterType`] as its spec string and parse it back
/// (case-insensitive, as `JobParameterType::from_spec_str` is).
mod job_param_type_spec {
    use crate::types::JobParameterType;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(ty: &JobParameterType, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(ty.as_spec_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<JobParameterType, D::Error> {
        let s = String::deserialize(d)?;
        JobParameterType::from_spec_str(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown output variable type: '{s}'")))
    }
}

/// The constraint properties an output variable declared, in hashable,
/// serializable form.
///
/// The meaning of each field follows the job parameter definition of the
/// same type. For a list type, `min_length`/`max_length` bound the number of
/// elements and `item` holds the per-element constraints; for `STRING`,
/// `PATH`, and `RANGE_EXPR` they bound the character count.
#[derive(Debug, Clone, Default, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputConstraints {
    /// `allowedValues`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<OutputAllowedValues>,
    /// `minValue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<OutputBound>,
    /// `maxValue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<OutputBound>,
    /// `minLength`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    /// `maxLength`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    /// `item`: constraints on each element of a list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Box<OutputConstraints>>,
}

impl OutputConstraints {
    /// `true` when no constraint property is declared.
    pub fn is_empty(&self) -> bool {
        self.allowed_values.is_none()
            && self.min_value.is_none()
            && self.max_value.is_none()
            && self.min_length.is_none()
            && self.max_length.is_none()
            && self.item.is_none()
    }
}

/// An `allowedValues` list.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputAllowedValues {
    /// For `INT` values.
    Int(Vec<i64>),
    /// For `FLOAT` values.
    Float(Vec<Float64>),
    /// For `STRING` and `PATH` values.
    String(Vec<String>),
}

/// A `minValue` or `maxValue` bound.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputBound {
    /// For `INT` values.
    Int(i64),
    /// For `FLOAT` values.
    Float(Float64),
}

/// What a check is looking at, for the message.
#[derive(Clone, Copy)]
enum Subject {
    /// The whole value.
    Value,
    /// Element `i` of a list value.
    Item(usize),
    /// Element `i` of element `j` of a `LIST[LIST[INT]]` value.
    ItemItem(usize, usize),
    /// One element reported under `collect: APPEND`.
    Element,
    /// Element `i` of an element reported under `collect: APPEND`.
    ElementItem(usize),
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value => f.write_str("value"),
            Self::Item(i) => write!(f, "item[{i}]"),
            Self::ItemItem(j, i) => write!(f, "item[{j}][{i}]"),
            Self::Element => f.write_str("element"),
            Self::ElementItem(i) => write!(f, "element item[{i}]"),
        }
    }
}

impl OutputVariable {
    /// Check a whole value (one Task's value) against the declared type and
    /// constraint properties. For a list type that is the length, with
    /// `minLength` applied, and every element.
    pub fn check_constraints(&self, value: &ExprValue) -> Result<(), String> {
        match self.param_type {
            JobParameterType::ListInt
            | JobParameterType::ListFloat
            | JobParameterType::ListString
            | JobParameterType::ListPath
            | JobParameterType::ListBool
            | JobParameterType::ListListInt => {
                let items = self.list_items(value)?;
                self.check_length(items.len(), true)?;
                for (i, item) in items.iter().enumerate() {
                    self.check_list_element(item, Subject::Item(i))?;
                }
                Ok(())
            }
            _ => self.check_scalar(&self.constraints, self.param_type, value, Subject::Value),
        }
    }

    /// Check one element reported under `collect: APPEND` against the
    /// element type and the `item` constraints. Errors when the declared
    /// type is not a list.
    pub fn check_element(&self, element: &ExprValue) -> Result<(), String> {
        if element_type(self.param_type).is_none() {
            return Err(self.msg(format_args!(
                "{} is not a list type, so it has no elements",
                self.param_type
            )));
        }
        self.check_list_element(element, Subject::Element)
    }

    /// Check the length of a list being collected under `collect: APPEND`.
    /// `maxLength` applies at every length; `minLength` only when `at_exit`
    /// is `true`, because the list is still growing until the Action exits.
    pub fn check_length(&self, len: usize, at_exit: bool) -> Result<(), String> {
        if at_exit {
            if let Some(min) = self.constraints.min_length {
                if len < min {
                    return Err(
                        self.msg(format_args!("list length {len} is less than minimum {min}"))
                    );
                }
            }
        }
        if let Some(max) = self.constraints.max_length {
            if len > max {
                return Err(self.msg(format_args!("list length {len} exceeds maximum {max}")));
            }
        }
        Ok(())
    }

    fn msg(&self, detail: std::fmt::Arguments<'_>) -> String {
        format!("Output variable '{}': {detail}", self.name)
    }

    fn type_mismatch(&self, expected: &ExprType, value: &ExprValue, subject: Subject) -> String {
        self.msg(format_args!(
            "{subject} has type {}, expected {expected}",
            value.expr_type()
        ))
    }

    /// The elements of a list value of the declared list type.
    fn list_items(&self, value: &ExprValue) -> Result<Vec<ExprValue>, String> {
        let expected = self.param_type.expr_type();
        let items: Option<Vec<ExprValue>> = match (self.param_type, value) {
            (JobParameterType::ListInt, ExprValue::ListInt(v)) => {
                Some(v.iter().map(|i| ExprValue::Int(*i)).collect())
            }
            (JobParameterType::ListFloat, ExprValue::ListFloat(v)) => {
                Some(v.iter().cloned().map(ExprValue::Float).collect())
            }
            (JobParameterType::ListString, ExprValue::ListString(v, _)) => {
                Some(v.iter().cloned().map(ExprValue::String).collect())
            }
            (JobParameterType::ListPath, ExprValue::ListPath(v, _, _))
            | (JobParameterType::ListPath, ExprValue::ListString(v, _)) => {
                Some(v.iter().cloned().map(ExprValue::String).collect())
            }
            (JobParameterType::ListBool, ExprValue::ListBool(v)) => {
                Some(v.iter().map(|b| ExprValue::Bool(*b)).collect())
            }
            (JobParameterType::ListListInt, ExprValue::ListList(v, _, _)) => Some(v.clone()),
            // An empty list built without a type hint is `list[nulltype]`.
            (_, ExprValue::ListList(v, hint, _)) if v.is_empty() && *hint == ExprType::NULLTYPE => {
                Some(Vec::new())
            }
            _ => None,
        };
        items.ok_or_else(|| self.type_mismatch(&expected, value, Subject::Value))
    }

    /// Check one element of a list-typed output.
    fn check_list_element(&self, element: &ExprValue, subject: Subject) -> Result<(), String> {
        let Some(elem_ty) = element_type(self.param_type) else {
            return Ok(());
        };
        let empty = OutputConstraints::default();
        let item = self.constraints.item.as_deref().unwrap_or(&empty);
        if elem_ty != JobParameterType::ListInt {
            return self.check_scalar(item, elem_ty, element, subject);
        }
        // LIST[LIST[INT]]: the element is a list[int].
        let ints: Vec<i64> = match element {
            ExprValue::ListInt(v) => v.clone(),
            ExprValue::ListList(v, hint, _) if v.is_empty() && *hint == ExprType::NULLTYPE => {
                Vec::new()
            }
            _ => {
                return Err(self.type_mismatch(&elem_ty.expr_type(), element, subject));
            }
        };
        if let Some(min) = item.min_length {
            if ints.len() < min {
                return Err(self.msg(format_args!(
                    "{subject} length {} is less than minimum {min}",
                    ints.len()
                )));
            }
        }
        if let Some(max) = item.max_length {
            if ints.len() > max {
                return Err(self.msg(format_args!(
                    "{subject} length {} exceeds maximum {max}",
                    ints.len()
                )));
            }
        }
        let inner = item.item.as_deref().unwrap_or(&empty);
        for (i, v) in ints.iter().enumerate() {
            let sub = match subject {
                Subject::Item(j) => Subject::ItemItem(j, i),
                _ => Subject::ElementItem(i),
            };
            self.check_scalar(inner, JobParameterType::Int, &ExprValue::Int(*v), sub)?;
        }
        Ok(())
    }

    /// Check a scalar value of type `ty` against `c`.
    fn check_scalar(
        &self,
        c: &OutputConstraints,
        ty: JobParameterType,
        value: &ExprValue,
        subject: Subject,
    ) -> Result<(), String> {
        match (ty, value) {
            (JobParameterType::Int, ExprValue::Int(v)) => self.check_int(c, *v, subject),
            (JobParameterType::Float, ExprValue::Float(v)) => self.check_float(c, v, subject),
            (JobParameterType::String, ExprValue::String(s))
            | (JobParameterType::Path, ExprValue::String(s))
            | (JobParameterType::Path, ExprValue::Path { value: s, .. }) => {
                self.check_str(c, s, subject)
            }
            (JobParameterType::RangeExpr, ExprValue::RangeExpr(r)) => {
                self.check_lengths(c, r.to_string().chars().count(), subject)
            }
            // A range expression as written: checked in that spelling, as a
            // RANGE_EXPR job parameter value is.
            (JobParameterType::RangeExpr, ExprValue::String(s)) => {
                s.parse::<openjd_expr::RangeExpr>().map_err(|_| {
                    self.msg(format_args!(
                        "{subject} '{s}' is not a valid range expression"
                    ))
                })?;
                self.check_lengths(c, s.chars().count(), subject)
            }
            (JobParameterType::Bool, ExprValue::Bool(_)) => Ok(()),
            _ => Err(self.type_mismatch(&ty.expr_type(), value, subject)),
        }
    }

    fn check_int(&self, c: &OutputConstraints, v: i64, subject: Subject) -> Result<(), String> {
        if let Some(OutputBound::Int(min)) = &c.min_value {
            if v < *min {
                return Err(self.msg(format_args!("{subject} {v} is less than minimum {min}")));
            }
        }
        if let Some(OutputBound::Int(max)) = &c.max_value {
            if v > *max {
                return Err(self.msg(format_args!("{subject} {v} exceeds maximum {max}")));
            }
        }
        if let Some(OutputAllowedValues::Int(allowed)) = &c.allowed_values {
            if !allowed.contains(&v) {
                return Err(self.msg(format_args!("{subject} {v} is not in allowed values")));
            }
        }
        Ok(())
    }

    fn check_float(
        &self,
        c: &OutputConstraints,
        v: &Float64,
        subject: Subject,
    ) -> Result<(), String> {
        let f = v.value();
        if let Some(OutputBound::Float(min)) = &c.min_value {
            if f < min.value() {
                return Err(self.msg(format_args!("{subject} {v} is less than minimum {min}")));
            }
        }
        if let Some(OutputBound::Float(max)) = &c.max_value {
            if f > max.value() {
                return Err(self.msg(format_args!("{subject} {v} exceeds maximum {max}")));
            }
        }
        if let Some(OutputAllowedValues::Float(allowed)) = &c.allowed_values {
            if !allowed.iter().any(|a| a.value() == f) {
                return Err(self.msg(format_args!("{subject} {v} is not in allowed values")));
            }
        }
        Ok(())
    }

    fn check_str(&self, c: &OutputConstraints, s: &str, subject: Subject) -> Result<(), String> {
        self.check_lengths(c, s.chars().count(), subject)?;
        if let Some(OutputAllowedValues::String(allowed)) = &c.allowed_values {
            if !allowed.iter().any(|a| a == s) {
                return Err(self.msg(format_args!("{subject} '{s}' is not in allowed values")));
            }
        }
        Ok(())
    }

    fn check_lengths(
        &self,
        c: &OutputConstraints,
        len: usize,
        subject: Subject,
    ) -> Result<(), String> {
        if let Some(min) = c.min_length {
            if len < min {
                return Err(self.msg(format_args!(
                    "{subject} length {len} is less than minimum {min}"
                )));
            }
        }
        if let Some(max) = c.max_length {
            if len > max {
                return Err(self.msg(format_args!("{subject} length {len} exceeds maximum {max}")));
            }
        }
        Ok(())
    }
}

/// The element type of a list output type, or `None` for a scalar type.
fn element_type(ty: JobParameterType) -> Option<JobParameterType> {
    match ty {
        JobParameterType::ListInt => Some(JobParameterType::Int),
        JobParameterType::ListFloat => Some(JobParameterType::Float),
        JobParameterType::ListString => Some(JobParameterType::String),
        JobParameterType::ListPath => Some(JobParameterType::Path),
        JobParameterType::ListBool => Some(JobParameterType::Bool),
        JobParameterType::ListListInt => Some(JobParameterType::ListInt),
        _ => None,
    }
}

/// The binding state of a [`job::Step`](super::Step) (RFC 0010 §Step input
/// binding).
///
/// `create_job` produces only [`Bound`](Self::Bound) today. The other two
/// states belong to step input binding, which a later change adds; they are
/// part of the type now so that the serialized form of a job is stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "SCREAMING_SNAKE_CASE",
    rename_all_fields = "camelCase"
)]
pub enum StepBinding {
    /// The Step's Tasks are enumerable.
    #[default]
    Bound,
    /// The Step's materialization expressions read `Steps.*` symbols that are
    /// not concrete yet (`PENDING_INPUTS`).
    PendingInputs {
        /// The `Steps.*` symbols the Step waits on, sorted.
        waiting_on: Vec<String>,
    },
    /// The Step's `enableIf` evaluated to `false` (`SKIPPED`).
    Skipped,
}

impl StepBinding {
    /// `true` for [`Bound`](Self::Bound).
    pub fn is_bound(&self) -> bool {
        matches!(self, Self::Bound)
    }
}

/// A path-typed `Steps.*` value a Step reads in a host-scope field, stored in
/// its submission (`Raw`) form as a `string` (or list of strings) in the
/// Step's `resolved_symtab`, so the session can build the `path` values and
/// map them forward on the worker host.
///
/// Recorded by step input binding; `create_job` leaves the list empty today.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathInput {
    /// The `Steps.*` symbol, such as `Steps.P.RawOutput.X`.
    pub symbol: String,
    /// `path`, `list[path]`, or `list[list[path]]`. Serialized under the key
    /// `type`, as the type's expression-language spelling.
    #[serde(rename = "type", with = "expr_type_string")]
    pub ty: ExprType,
}

/// Serialize an [`ExprType`] as its spelling (`list[path]`) and parse it back.
mod expr_type_string {
    use openjd_expr::ExprType;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(ty: &ExprType, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(ty)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ExprType, D::Error> {
        let s = String::deserialize(d)?;
        ExprType::parse(&s).map_err(serde::de::Error::custom)
    }
}
