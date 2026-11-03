// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Host requirements per spec §3.3.

use crate::format_string::FormatString;
use serde::Deserialize;

/// §3.3 HostRequirements
///
/// In a document that lists `ENV_PARAMS`, an empty object, or empty
/// `amounts` and `attributes` lists, is valid and means no requirements
/// (§3.3 constraint 1 does not apply), and a requirement that resolves to
/// nothing at job creation is discarded (§3.3).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostRequirements {
    /// The amount requirements (§3.3.1), as written.
    pub amounts: Option<Vec<AmountRequirement>>,
    /// The attribute requirements (§3.3.2), as written.
    pub attributes: Option<Vec<AttributeRequirement>>,
}

/// §3.3.1 AmountRequirement
///
/// `name` is `@fmtstring`: a name containing expressions is resolved, and
/// its §3.3.1.1 constraints checked, at job creation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AmountRequirement {
    /// The amount capability name (§3.3.1.1).
    pub name: FormatString,
    /// The minimum amount; a format string only with `FEATURE_BUNDLE_1`.
    pub min: Option<FormatString>,
    /// The maximum amount; a format string only with `FEATURE_BUNDLE_1`.
    pub max: Option<FormatString>,
}

impl AmountRequirement {
    /// Whether job creation may discard this amount, judged from its text
    /// alone: it has a bound, and every bound it has is a format string
    /// with expressions, which may resolve to `null`. A literal bound never
    /// resolves to `null`, so an amount with one is always kept. Only a
    /// document that lists `ENV_PARAMS` discards requirements (§3.3), so
    /// the answer means nothing in any other. Conservative: `true` also
    /// for an expression that can never be `null`, such as `{{ 2 }}`.
    pub fn may_be_discarded(&self) -> bool {
        let bounds: Vec<&FormatString> = self.min.iter().chain(&self.max).collect();
        !bounds.is_empty() && bounds.iter().all(|b| !b.is_literal())
    }
}

/// §3.3.2 AttributeRequirement
///
/// `name` is `@fmtstring`: a name containing expressions is resolved, and
/// its §3.3.2.1 constraints checked, at job creation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeRequirement {
    /// The attribute capability name (§3.3.2.1).
    pub name: FormatString,
    /// The values of which a host must have at least one.
    pub any_of: Option<AttributeValues>,
    /// The values a host must have all of.
    pub all_of: Option<AttributeValues>,
}

impl AttributeRequirement {
    /// Whether job creation may discard this attribute, judged from its
    /// text alone: it has `anyOf` or `allOf`, and each it has is written as
    /// one expression, which may resolve to `null`. A list is never omitted
    /// (one that resolves to no values is an error), so an attribute with
    /// one is always kept. As for
    /// [`AmountRequirement::may_be_discarded`], only a document that lists
    /// `ENV_PARAMS` discards requirements (§3.3 item 2), and `true` is
    /// conservative.
    pub fn may_be_discarded(&self) -> bool {
        let fields: Vec<&AttributeValues> = self.any_of.iter().chain(&self.all_of).collect();
        !fields.is_empty()
            && fields
                .iter()
                .all(|f| matches!(f, AttributeValues::Expression(_)))
    }
}

/// The value of an attribute requirement's `anyOf` or `allOf` (§3.3.2).
#[derive(Debug, Clone)]
pub enum AttributeValues {
    /// A list of format strings, each an `<AttributeCapabilityValue>`. A
    /// whole-field expression item resolves with the target type
    /// `string? | list[string]` (Expression Language §1.3.2): `null` skips
    /// it and a list flattens inline.
    List(Vec<FormatString>),
    /// A `<ListExpressionString>`, written as a string in place of the
    /// list: valid only in a document that lists `ENV_PARAMS`, and only
    /// when it is exactly one `{{ }}` expression with no surrounding text,
    /// which template validation checks. It resolves with the target type
    /// `list[string]?`: `null` omits the field.
    Expression(FormatString),
}

impl AttributeValues {
    /// The format strings of the list form; `None` for the expression form.
    pub fn as_list(&self) -> Option<&[FormatString]> {
        match self {
            Self::List(items) => Some(items),
            Self::Expression(_) => None,
        }
    }

    /// Whether the value is known without resolving anything: a list
    /// whose items are all literals. The expression form never is.
    pub fn is_literal(&self) -> bool {
        self.as_list()
            .is_some_and(|items| items.iter().all(FormatString::is_literal))
    }
}

impl<'de> Deserialize<'de> for AttributeValues {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let val = serde_json::Value::deserialize(deserializer)?;
        let kind = match &val {
            serde_json::Value::Array(_) => {
                return serde_json::from_value(val)
                    .map(AttributeValues::List)
                    .map_err(serde::de::Error::custom);
            }
            serde_json::Value::String(s) => {
                return FormatString::new(s)
                    .map(AttributeValues::Expression)
                    .map_err(serde::de::Error::custom);
            }
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Bool(b) => format!("boolean `{b}`"),
            serde_json::Value::Number(n) if n.is_f64() => format!("floating point `{n}`"),
            serde_json::Value::Number(n) => format!("integer `{n}`"),
            serde_json::Value::Object(_) => "map".to_string(),
        };
        Err(serde::de::Error::custom(format!(
            "invalid type: {kind}, expected a list of values, or (with ENV_PARAMS) a string \
             holding one {{{{ }}}} expression"
        )))
    }
}
