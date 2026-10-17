// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Job outputs (`CONTROL_FLOW`, RFC 0010 §Job outputs): the job-side form of
//! a `<JobOutputDefinition>`, with its value once it has been evaluated.
//!
//! The functions that evaluate one are
//! [`job_output_inputs`](crate::create_job::job_output_inputs) and
//! [`evaluate_job_output`](crate::create_job::evaluate_job_output). See
//! `specs/model/job-outputs.md`.

use openjd_expr::format_string::FormatString;
use openjd_expr::ExprValue;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use super::OutputVariable;
use crate::types::ObjectType;

/// One of the Job's outputs (RFC 0010 `<JobOutputDefinition>`), in the form
/// `create_job` stores in [`Job::outputs`](super::Job::outputs).
///
/// Serialized as one struct with the declaration's fields (`name`, `type`,
/// `unit`, `constraints`) followed by `objectType`, `description`, `value`,
/// and `evaluated`, each optional one skipped when empty. (Written by hand, not
/// with `#[serde(flatten)]`, which serializes as a map and so as a JS `Map`
/// through `serde_wasm_bindgen`.)
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct JobOutput {
    /// The output's name, type, `unit`, and declared constraint properties,
    /// in the form a Step's output variable declaration takes. Its `collect`
    /// is always the default, since a job output has none.
    pub declaration: OutputVariable,
    /// The `objectType` of a `PATH` or `LIST[PATH]` output (`FILE` or
    /// `DIRECTORY`), for tools and user interfaces. Not checked.
    pub object_type: Option<ObjectType>,
    /// The `description`, for display.
    pub description: Option<String>,
    /// The `value` format string, as written.
    pub value: FormatString,
    /// The evaluated value: `None` while the output is pending, which is
    /// until every `Steps.*` symbol its `value` reads is concrete.
    /// `create_job` fills this in for the outputs whose inputs are concrete
    /// at Job creation; a caller fills in the rest from
    /// [`evaluate_job_output`](crate::create_job::evaluate_job_output).
    pub evaluated: Option<JobOutputValue>,
}

impl Serialize for JobOutput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let d = &self.declaration;
        let mut s = serializer.serialize_struct("JobOutput", 8)?;
        s.serialize_field("name", &d.name)?;
        s.serialize_field("type", d.param_type.as_spec_str())?;
        match &d.unit {
            Some(unit) => s.serialize_field("unit", unit)?,
            None => s.skip_field("unit")?,
        }
        if d.constraints.is_empty() {
            s.skip_field("constraints")?;
        } else {
            s.serialize_field("constraints", &d.constraints)?;
        }
        match &self.object_type {
            Some(object_type) => s.serialize_field("objectType", object_type)?,
            None => s.skip_field("objectType")?,
        }
        match &self.description {
            Some(description) => s.serialize_field("description", description)?,
            None => s.skip_field("description")?,
        }
        s.serialize_field("value", &self.value)?;
        match &self.evaluated {
            Some(evaluated) => s.serialize_field("evaluated", evaluated)?,
            None => s.skip_field("evaluated")?,
        }
        s.end()
    }
}

/// An evaluated job output.
///
/// Serialized with a `state` tag, as `job::StepBinding` is:
/// `{"state": "VALUE", "value": <ExprValue>}` or `{"state": "NULL"}`. (A
/// bare `null` would read back as `undefined` through `serde_wasm_bindgen`,
/// the same as a pending output's absent `evaluated`.)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobOutputValue {
    /// The value, converted to the declared type and checked against its
    /// constraint properties.
    Value(ExprValue),
    /// The `value` evaluated to `null`: the output has no value, which is not
    /// an error (for example, it reads a skipped Step).
    Null,
}

impl Serialize for JobOutputValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(v) => {
                let mut s = serializer.serialize_struct("JobOutputValue", 2)?;
                s.serialize_field("state", "VALUE")?;
                s.serialize_field("value", v)?;
                s.end()
            }
            Self::Null => {
                let mut s = serializer.serialize_struct("JobOutputValue", 1)?;
                s.serialize_field("state", "NULL")?;
                s.end()
            }
        }
    }
}

impl JobOutputValue {
    /// The value, or `None` for [`JobOutputValue::Null`].
    pub fn value(&self) -> Option<&ExprValue> {
        match self {
            Self::Value(v) => Some(v),
            Self::Null => None,
        }
    }
}

impl From<Option<ExprValue>> for JobOutputValue {
    /// `Some(v)` is [`JobOutputValue::Value`], `None` is
    /// [`JobOutputValue::Null`], as
    /// [`evaluate_job_output`](crate::create_job::evaluate_job_output)
    /// returns them.
    fn from(value: Option<ExprValue>) -> Self {
        match value {
            Some(v) => Self::Value(v),
            None => Self::Null,
        }
    }
}
