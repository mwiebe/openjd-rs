// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! JSON value to typed `ExprValue` (RFC 0010 §Processing a directive, "Type
//! conversion").
//!
//! A JSON value first gets its natural EXPR type: an integer-spelled number
//! that fits in an `i64` is an `int`, any other number a `float`, a string a
//! `string`, `true`/`false` a `bool`, and an array a list. Then it is
//! converted to the target type with `ExprValue::coerce`, EXPR's implicit
//! coercion. An array is converted element by element toward the target's
//! element type, as a list literal typed by its target is (EXPR §1.2.6), so
//! `[1, "a"]` is a valid `list[string]`.
//!
//! This deliberately does not reuse `json_to_expr_value` (which builds lists
//! untyped, so `[1, "a"]` fails) or `coerce_json_to_job_parameter_type`
//! (which follows the more lenient job-parameter rules, so `1` is a `bool`).
//!
//! `serde_json` is built without `arbitrary_precision`, so a parsed number no
//! longer has its written digits. They are recovered from the text instead
//! ([`number_spellings`]), so a number keeps its spelling (`1.50` stays
//! `1.50` as a `float` and as a `string`), `-0` is the `int` 0, and an
//! integer outside the `i64` range keeps its digits when converted to
//! `string`.

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::types::TypeCode;
use openjd_expr::value::Float64;
use openjd_expr::{ExprType, ExprValue};
use serde_json::Value;

use super::{quote, shorten};

/// A conversion failure: where in the value, and what went wrong.
pub(crate) struct ConvertError {
    /// `value`, `item[1]`, `item[0][2]`.
    pub(crate) subject: String,
    /// The problem, starting lowercase.
    pub(crate) detail: String,
}

/// The text of each JSON number in `text`, in document order.
///
/// `text` must already be known to be valid JSON. Outside a string, a `-` or
/// a digit can only begin a number, and a number is a run of
/// `[0-9+\-.eE]`.
pub(crate) fn number_spellings(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut spellings = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    // An escape covers the next byte; a `\uXXXX` escape's
                    // hex digits never contain `"`.
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    i += 1;
                }
                spellings.push(&text[start..i]);
            }
            _ => i += 1,
        }
    }
    spellings
}

/// Reject `null` and objects anywhere in `value`.
pub(crate) fn check_no_null_or_object(value: &Value, subject: &str) -> Result<(), ConvertError> {
    match value {
        Value::Null => Err(ConvertError {
            subject: subject.to_string(),
            detail: "is null; output values cannot be or contain null".to_string(),
        }),
        Value::Object(_) => Err(ConvertError {
            subject: subject.to_string(),
            detail: "is a JSON object; output values cannot be or contain objects".to_string(),
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, item)| check_no_null_or_object(item, &item_subject(subject, i))),
        _ => Ok(()),
    }
}

fn item_subject(subject: &str, i: usize) -> String {
    if subject == "value" {
        format!("item[{i}]")
    } else {
        format!("{subject}[{i}]")
    }
}

/// Converts JSON values to a target type, consuming number spellings in
/// document order.
pub(crate) struct Converter<'s, I: Iterator<Item = &'s str>> {
    pub(crate) spellings: I,
    /// The format given to a `path` value, before reverse mapping.
    pub(crate) path_format: PathFormat,
    /// Whether the error detail may show the value (inline values) or only
    /// its type (`@` file values).
    pub(crate) show_values: bool,
}

impl<'s, I: Iterator<Item = &'s str>> Converter<'s, I> {
    /// Convert `value` (already checked by [`check_no_null_or_object`]) to
    /// `target`.
    pub(crate) fn convert(
        &mut self,
        value: &Value,
        target: &ExprType,
        subject: &str,
    ) -> Result<ExprValue, ConvertError> {
        if target.code() == TypeCode::List {
            let elem_ty = &target.params()[0];
            let Value::Array(items) = value else {
                let natural = self.natural(value, subject)?;
                let detail = natural.clone().coerce(target, self.path_format).err();
                return Err(self.coerce_error(subject, value, &natural, target, detail));
            };
            let mut elements = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                elements.push(self.convert(item, elem_ty, &item_subject(subject, i))?);
            }
            return ExprValue::make_list(elements, elem_ty.clone()).map_err(|e| ConvertError {
                subject: subject.to_string(),
                detail: format!("does not form a {target}: {e}"),
            });
        }
        let natural = self.natural(value, subject)?;
        match natural.clone().coerce(target, self.path_format) {
            Ok(v) => Ok(v),
            Err(e) => Err(self.coerce_error(subject, value, &natural, target, Some(e))),
        }
    }

    /// The natural EXPR value of a JSON value. An array is built as an untyped
    /// list literal, which is only used to name its type in a message.
    fn natural(&mut self, value: &Value, subject: &str) -> Result<ExprValue, ConvertError> {
        match value {
            Value::Bool(b) => Ok(ExprValue::Bool(*b)),
            Value::String(s) => Ok(ExprValue::String(s.clone())),
            Value::Number(n) => {
                let spelling = self.spellings.next().unwrap_or_default();
                // An `i64` parse accepts exactly the spellings with neither a
                // fraction nor an exponent that fit (`-0` is 0).
                if let Ok(i) = spelling.parse::<i64>() {
                    return Ok(ExprValue::Int(i));
                }
                // The value comes from the spelling, not `n.as_f64()`:
                // serde_json without `float_roundtrip` can round differently
                // from Rust's (and EXPR's) correctly rounded `str::parse`.
                // serde_json rejected numbers that overflow f64 when it
                // parsed the text, so this is finite.
                let f = spelling
                    .parse::<f64>()
                    .unwrap_or_else(|_| n.as_f64().unwrap_or_default());
                Float64::with_str(f, spelling.to_string())
                    .map(ExprValue::Float)
                    .map_err(|e| ConvertError {
                        subject: subject.to_string(),
                        detail: format!("is not a finite float: {e}"),
                    })
            }
            Value::Array(items) => {
                let mut elements = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    elements.push(self.natural(item, &item_subject(subject, i))?);
                }
                // A list whose elements don't form a list literal has no
                // type to name; `Null` stands in and is described as a list.
                Ok(ExprValue::make_list(elements, ExprType::NULLTYPE).unwrap_or(ExprValue::Null))
            }
            // Rejected beforehand by `check_no_null_or_object`.
            Value::Null | Value::Object(_) => Ok(ExprValue::Null),
        }
    }

    fn coerce_error(
        &self,
        subject: &str,
        value: &Value,
        natural: &ExprValue,
        target: &ExprType,
        detail: Option<String>,
    ) -> ConvertError {
        // A list with no element type (one that doesn't form a list
        // literal, stood in for by `Null`, or holds only empty lists, which
        // EXPR types with `nulltype`) is described without EXPR's type name,
        // which means nothing to an author, and without EXPR's message,
        // which would repeat it.
        let full_ty = natural.expr_type().to_string();
        // `Null`'s own type is `nulltype`, so this covers it too.
        let untyped = full_ty.contains("nulltype");
        let ty = if natural.list_len() == Some(0) {
            "empty list".to_string()
        } else if untyped {
            "list".to_string()
        } else {
            full_ty
        };
        let shown = if self.show_values {
            let text = match natural {
                ExprValue::Null => value.to_string(),
                v => v.to_display_string(),
            };
            format!(" {}", quote(&text))
        } else {
            String::new()
        };
        let detail = match detail {
            Some(d) if self.show_values && !untyped => format!(": {}", shorten(&d)),
            _ => String::new(),
        };
        ConvertError {
            subject: subject.to_string(),
            detail: format!("({ty}){shown} does not convert to {target}{detail}"),
        }
    }
}
