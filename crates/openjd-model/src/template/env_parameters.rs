// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Environment parameter types (`ENV_PARAMS` extension): §4.5
//! `<EnvironmentParameterDefinition>` and the §3.2
//! `<EnvironmentParameterValue>` that both a definition's `default` and a
//! Step's `environment:` entry hold.

use std::collections::BTreeMap;

use super::parameters::JobParameterDefinition;
use crate::format_string::FormatString;
use crate::types::JobParameterType;
use serde::Deserialize;

/// A value written for an Environment parameter: the `default` of an
/// [`EnvironmentParameterDefinition`] or one value of a `<StepDependency>`
/// `environment:` entry's `parameters` (§3.2, §4.5).
///
/// Both positions are `@fmtstring`, and the spec splits them the same way:
/// a string containing `{{` is a format string, resolved at job creation,
/// and anything else is a literal written as the `default` of a
/// `<JobParameterDefinition>` of the parameter's type is written. Which
/// literals are valid depends on the parameter's type, which is not known
/// when the value is decoded, so a literal is kept as written and checked
/// by template validation (or, for an external Environment, at job
/// creation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentParameterValue {
    /// A string containing `{{`, parsed as a format string.
    FormatString(FormatString),
    /// Any other value, exactly as written: a string without `{{`, a
    /// number, a bool, a list, or (always invalid, reported by template
    /// validation) an object or `null`.
    Literal(serde_json::Value),
}

impl EnvironmentParameterValue {
    /// Classify a value as written. A string containing `{{` is parsed as a
    /// format string, and a parse failure (for example `"a{{b"`, "Braces
    /// mismatch") is returned as the error.
    pub fn from_json(value: serde_json::Value) -> Result<Self, String> {
        match value {
            serde_json::Value::String(s) if s.contains("{{") => FormatString::new(&s)
                .map(Self::FormatString)
                .map_err(|e| e.to_string()),
            other => Ok(Self::Literal(other)),
        }
    }

    /// The literal, if this value is one.
    #[must_use]
    pub fn as_literal(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Literal(v) => Some(v),
            Self::FormatString(_) => None,
        }
    }

    /// The format string, if this value is one.
    #[must_use]
    pub fn as_format_string(&self) -> Option<&FormatString> {
        match self {
            Self::FormatString(fs) => Some(fs),
            Self::Literal(_) => None,
        }
    }
}

impl<'de> Deserialize<'de> for EnvironmentParameterValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_json(value).map_err(serde::de::Error::custom)
    }
}

/// The `parameters` map of a `<StepDependency>` `environment:` entry
/// (§3.2 `<EnvironmentParameterValues>`), keyed by parameter name.
///
/// Keys are kept as plain strings so template validation can report a key
/// that is not an `<Identifier>` at its own path. The map is ordered by
/// key: the workspace's `serde_json` has no `preserve_order` feature, so a
/// template's map reaches the decoder sorted by key and written order is
/// not available to keep.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct EnvironmentParameterValueMap(pub BTreeMap<String, EnvironmentParameterValue>);

impl EnvironmentParameterValueMap {
    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate the entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &EnvironmentParameterValue)> {
        self.0.iter()
    }

    /// The value supplied for `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EnvironmentParameterValue> {
        self.0.get(name)
    }
}

/// §4.5 `<EnvironmentParameterDefinition>` (`ENV_PARAMS` extension).
///
/// It has the shape of a `<JobParameterDefinition>` of the same `type`,
/// except that `default` is a format string resolved at job creation, so
/// it cannot be held by the Job Parameter type's typed `default` field
/// (an INT `default` of `"{{Param.Seats}}"` is not an integer). The
/// definition is decoded by removing `default`, decoding the rest as a
/// [`JobParameterDefinition`] (whose `default` is then always `None`),
/// and keeping `default` as an [`EnvironmentParameterValue`].
///
/// Every field of that shape is permitted (§4.5 items 1–3): any type the
/// document allows, `RANGE_EXPR` included, and `userInterface`,
/// `objectType`, and `dataFlow`, which are informational: they describe
/// the parameter to tools and change nothing about job creation or how the
/// Job runs, and an Environment parameter's `dataFlow` does not add to the
/// Job's data flow.
#[derive(Debug, Clone)]
pub struct EnvironmentParameterDefinition {
    /// The definition without its `default`. Decode leaves its typed
    /// `default` field `None`; validation and job creation read only
    /// [`default`](Self::default), so a value built in code with a typed
    /// `default` here has that value ignored.
    pub definition: JobParameterDefinition,
    /// The `default`, as written.
    pub default: Option<EnvironmentParameterValue>,
}

impl EnvironmentParameterDefinition {
    /// The parameter's name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.definition.name()
    }

    /// The parameter's type.
    #[must_use]
    pub fn job_param_type(&self) -> JobParameterType {
        self.definition.job_param_type()
    }

    /// The type as the specification spells it (`STRING`, `LIST[PATH]`, ...).
    #[must_use]
    pub fn type_name(&self) -> &str {
        self.definition.type_name()
    }
}

impl<'de> Deserialize<'de> for EnvironmentParameterDefinition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let default = match value.as_object_mut() {
            Some(obj) => obj.remove("default"),
            None => {
                return Err(serde::de::Error::custom(
                    "an Environment parameter definition must be an object",
                ))
            }
        };
        let definition: JobParameterDefinition =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        // An explicit `default: null` means no default, as it does for a
        // `<JobParameterDefinition>`, whose typed `default` is an `Option`.
        let default = default
            .filter(|v| !v.is_null())
            .map(EnvironmentParameterValue::from_json)
            .transpose()
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            definition,
            default,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_classification() {
        let fs = EnvironmentParameterValue::from_json(serde_json::json!("{{Param.X}}")).unwrap();
        assert_eq!(fs.as_format_string().unwrap().raw(), "{{Param.X}}");
        let lit = EnvironmentParameterValue::from_json(serde_json::json!("plain")).unwrap();
        assert_eq!(lit.as_literal(), Some(&serde_json::json!("plain")));
        let n = EnvironmentParameterValue::from_json(serde_json::json!(7)).unwrap();
        assert_eq!(n.as_literal(), Some(&serde_json::json!(7)));
        // A list is a literal even when an element contains `{{`; template
        // validation rejects such elements.
        let l = EnvironmentParameterValue::from_json(serde_json::json!(["{{X}}"])).unwrap();
        assert!(l.as_literal().is_some());
    }

    #[test]
    fn unbalanced_braces_are_a_format_string_error() {
        let err = EnvironmentParameterValue::from_json(serde_json::json!("a{{b")).unwrap_err();
        assert_eq!(
            err,
            "Failed to parse interpolation expression at [1, 4]. Reason: Braces mismatch.\n  a{{b\n   ^~"
        );
    }

    #[test]
    fn definition_keeps_default_out_of_the_job_parameter() {
        let d: EnvironmentParameterDefinition = serde_json::from_value(serde_json::json!({
            "name": "Seats", "type": "INT", "default": "{{Param.Seats}}"
        }))
        .unwrap();
        assert_eq!(d.name(), "Seats");
        assert_eq!(d.job_param_type(), JobParameterType::Int);
        assert!(d.definition.default_value().is_none());
        assert!(d.default.unwrap().as_format_string().is_some());
    }
}
