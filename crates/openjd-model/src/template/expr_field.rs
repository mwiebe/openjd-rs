// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Expression fields per Template Schemas §7.5 (`@expr <type>`).
//!
//! An expression field holds one Expression Language expression, written
//! without `{{ }}`, or a boolean, integer, or float scalar used as written.
//! [`ExprField`] is the value as a template writes it. The crate-private
//! functions below check it against the field's **target type**: at
//! template validation ([`validate`]) and when it is evaluated ([`resolve`]).
//! Each field that uses the type supplies its own target type, scope, and
//! stage, so later fields reuse the checker unchanged.

use openjd_expr::{
    ExprProfile, ExprType, ExprValue, ExpressionError, FunctionLibrary, ParsedExpression,
    PathFormat, SymbolTable, TypeCode,
};
use serde::{Deserialize, Deserializer};

/// The value of an expression field (Template Schemas §7.5), as written.
///
/// A YAML or JSON boolean decodes as [`Bool`](Self::Bool), an integer as
/// [`Int`](Self::Int), a number with a fraction or exponent as
/// [`Float`](Self::Float), a string as [`Expression`](Self::Expression),
/// and a present `null` (`null`, `~`, or a key with no value) as
/// [`Null`](Self::Null). A list, a map, or an integer above `i64::MAX`
/// that fits `u64` is a decode error; a larger integer, or one below
/// `i64::MIN`, arrives from JSON number parsing as a float and decodes as
/// [`Float`](Self::Float).
///
/// A scalar of any type decodes, and so does a present `null`, so that
/// template validation, which knows the field's target type, can report
/// a scalar that does not fit it, or the `null`, at the field's path. A
/// string is never read as a scalar: `"true"` is the expression `true`.
///
/// serde decodes a present `null` as `None` for a plain `Option` field, so
/// an optional expression field needs a `deserialize_with` that keeps it as
/// `Some(Null)`; `enableIf` uses one.
#[derive(Debug, Clone)]
pub enum ExprField {
    /// A boolean scalar, used as written.
    Bool(bool),
    /// An integer scalar, used as written (widened for a `float` target).
    Int(i64),
    /// A float scalar, used as written.
    Float(f64),
    /// A string: the source of one expression, without `{{ }}`.
    Expression(String),
    /// A present `null`, which template validation rejects (§7.5). An
    /// absent field is `None` in the `Option` that holds an `ExprField`,
    /// never this.
    Null,
}

/// Floats compare by bit pattern, which is reflexive, as `Eq` requires. A
/// decoded value is never NaN (JSON has none, and YAML's `.nan` does not
/// decode to a number); the one difference from `f64` equality a template
/// can show is that `-0.0` and `0.0` are unequal, as they are written
/// differently.
impl PartialEq for ExprField {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            (Self::Expression(a), Self::Expression(b)) => a == b,
            (Self::Null, Self::Null) => true,
            _ => false,
        }
    }
}

impl Eq for ExprField {}

impl ExprField {
    /// The boolean scalar, or `None` for any other value, an expression
    /// included (even a constant one such as `"true"`, which is evaluated
    /// like any other).
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl ExprField {
    /// Decode a YAML/JSON value. `field`, when given, names the field in a
    /// decode error (decode errors carry no path, so the name is what
    /// locates them).
    fn from_json(value: serde_json::Value, field: Option<&str>) -> Result<Self, String> {
        match value {
            serde_json::Value::Bool(b) => Ok(Self::Bool(b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Self::Int(i))
                } else if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
                    Ok(Self::Float(f))
                } else {
                    // A `u64` above `i64::MAX`. (An integer outside both
                    // reaches here as a JSON float, which YAML's and
                    // serde_json's number parsing produce for it.)
                    let prefix = field.map(|f| format!("{f}: ")).unwrap_or_default();
                    Err(format!(
                        "{prefix}the integer {n} is out of range for an expression field \
                         (Template Schemas §7.5)"
                    ))
                }
            }
            serde_json::Value::String(s) => Ok(Self::Expression(s)),
            serde_json::Value::Null => Ok(Self::Null),
            _ => Err(match field {
                Some(f) => format!(
                    "{f} must be a boolean, integer, float, or string (an expression field, \
                     Template Schemas §7.5)"
                ),
                None => "an expression field must be a boolean, integer, float, or string \
                         (Template Schemas §7.5)"
                    .to_string(),
            }),
        }
    }
}

impl<'de> Deserialize<'de> for ExprField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_json(value, None).map_err(serde::de::Error::custom)
    }
}

/// `deserialize_with` for an optional `enableIf` (§3.2 item 5, §4 item 6):
/// a present `null` becomes `Some(ExprField::Null)` rather than `None`, so
/// that template validation reports it at the field's path (§7.5). Pair it
/// with `#[serde(default)]`, which gives `None` when the key is absent. A
/// value of another kind is a decode error that names `enableIf`.
pub(crate) fn deserialize_enable_if<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ExprField>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    ExprField::from_json(value, Some("enableIf"))
        .map(Some)
        .map_err(serde::de::Error::custom)
}

/// What an expression field is evaluated with: the profile it parses
/// under, the function library, and the caller's evaluation budgets. The
/// scope (the symbol table) is passed per call. Paths are always POSIX:
/// both expression fields today (the two `enableIf`s) are evaluated in
/// template scope, not a host context (Template Schemas §3.2 item 5, §4
/// item 6; spec 5535230).
pub(crate) struct ExprFieldEval<'a> {
    pub(crate) profile: &'a ExprProfile,
    pub(crate) library: &'a FunctionLibrary,
    pub(crate) memory_limit: Option<usize>,
    pub(crate) operation_limit: Option<usize>,
}

impl ExprFieldEval<'_> {
    fn parse(&self, source: &str) -> Result<ParsedExpression, String> {
        ParsedExpression::with_profile(source, self.profile).map_err(|e| {
            let text = e.to_string();
            // §7.5 rule 1: a value written as a Format String is a parse
            // error, and the message should say why.
            let text = if source.contains("{{") {
                with_format_string_hint(&text, &e.message())
            } else {
                text
            };
            format!("Invalid expression: {text}")
        })
    }

    fn evaluate(
        &self,
        parsed: &ParsedExpression,
        symtab: &SymbolTable,
    ) -> Result<ExprValue, ExpressionError> {
        let mut builder = parsed
            .with_library(self.library)
            // Both expression fields (the two `enableIf`s) are evaluated
            // in template scope, not a host context: POSIX paths always
            // (Template Schemas §3.2 item 5, §4 item 6; spec 5535230). A
            // later expression field in a host context would need a path
            // format here.
            .with_path_format(PathFormat::Posix);
        if let Some(m) = self.memory_limit {
            builder = builder.with_memory_limit(m);
        }
        if let Some(o) = self.operation_limit {
            builder = builder.with_operation_limit(o);
        }
        builder.evaluate(&[symtab])
    }
}

/// `text`, a rendered expression error whose first part is `summary`,
/// with the §7.5 rule 1 hint after the summary. The hint goes before the
/// expression and caret lines that follow, so the error stays one block of
/// indented continuation lines. When `text` does not start with `summary`
/// the hint goes before the first line break.
fn with_format_string_hint(text: &str, summary: &str) -> String {
    let at = if text.starts_with(summary) {
        summary.len()
    } else {
        text.find('\n').unwrap_or(text.len())
    };
    let sep = if text[..at].ends_with(['.', '?', '!']) {
        " "
    } else {
        ". "
    };
    format!(
        "{}{sep}An expression field takes an expression without '{{{{ }}}}' (Template Schemas \
         §7.5).{}",
        &text[..at],
        &text[at..]
    )
}

/// Why a static type does not fit a target type (§7.5 rule 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FitError {
    /// The type is `nulltype`, and the target does not admit it.
    NullType,
    /// The type, after `unresolved[U]` is read as `U` and `nulltype` is
    /// removed from a union whose target does not admit it, does not fit
    /// even with `int` widened to `float`. Carries that type.
    Mismatch(ExprType),
}

/// Whether `target` admits `nulltype`: `nulltype` itself, `any`, or a
/// choice that includes one of them.
fn admits_null(target: &ExprType) -> bool {
    ExprType::NULLTYPE.satisfies(target)
}

/// `t` with every `int` replaced by `float`, inside lists and choices too:
/// the one conversion §7.5 rule 2 applies.
fn widen_int(t: &ExprType) -> ExprType {
    match t.code() {
        TypeCode::Int => ExprType::FLOAT,
        TypeCode::List => ExprType::list(widen_int(&t.params()[0])),
        TypeCode::Union => ExprType::union(t.params().iter().map(widen_int).collect()),
        // `unresolved[U]` is read as `U` before this is called.
        _ => t.clone(),
    }
}

/// Whether `t` fits `target` as is, or after `int` widens to `float`.
fn fits_widened(t: &ExprType, target: &ExprType) -> bool {
    t.satisfies(target) || widen_int(t).satisfies(target)
}

/// §7.5 rule 2 at template validation: whether an expression of static
/// type `t` is accepted for a field of type `target`.
///
/// `unresolved[U]` is checked as `U`. Where `target` admits `nulltype`,
/// `t` must fit it. Otherwise `nulltype` is an error, and an optional
/// `U?` is accepted when `U` fits, its `null` being an error when the
/// expression is evaluated ([`check_value`]). Fitting is
/// [`ExprType::satisfies`] (Expression Language §1.2.3 step 1), with
/// `int` also accepted where `float` is wanted, element-wise for lists.
pub(crate) fn check_static_type(t: &ExprType, target: &ExprType) -> Result<(), FitError> {
    // Normalization hoists `unresolved` to the root, so one unwrap reads
    // through it.
    let t = if t.code() == TypeCode::Unresolved {
        t.params().first().cloned().unwrap_or(ExprType::ANY)
    } else {
        t.clone()
    };
    let t = if admits_null(target) {
        t
    } else if t.code() == TypeCode::NullType {
        return Err(FitError::NullType);
    } else if t.code() == TypeCode::Union {
        let members: Vec<ExprType> = t
            .params()
            .iter()
            .filter(|m| m.code() != TypeCode::NullType)
            .cloned()
            .collect();
        ExprType::union(members)
    } else {
        t
    };
    if fits_widened(&t, target) {
        Ok(())
    } else {
        Err(FitError::Mismatch(t))
    }
}

/// Why a value does not fit a target type (§7.5 rule 2), at evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValueFitError {
    /// The value is `null`, and the target does not admit `nulltype`. An
    /// error even on an optional field: a `null` result never means the
    /// field was omitted.
    Null,
    /// The value's type does not fit. Carries that type.
    Mismatch(ExprType),
}

/// §7.5 rule 2 at evaluation: `value` as the field's value, with an `int`
/// widened to `float` (element-wise) where only that fits.
pub(crate) fn check_value(value: ExprValue, target: &ExprType) -> Result<ExprValue, ValueFitError> {
    if matches!(value, ExprValue::Null) && !admits_null(target) {
        return Err(ValueFitError::Null);
    }
    let t = value.expr_type();
    if t.satisfies(target) {
        return Ok(value);
    }
    let widened = widen_int(&t);
    if widened.satisfies(target) {
        // `widened` is concrete and differs from `t` only where an `int`
        // became a `float`, so coercion performs exactly that widening.
        return value
            .coerce(&widened, PathFormat::Posix)
            .map_err(|_| ValueFitError::Mismatch(t));
    }
    Err(ValueFitError::Mismatch(t))
}

/// "a boolean scalar or a string holding one expression", naming the
/// scalar kinds that fit `target`, for the messages about a written value.
fn accepted_forms(target: &ExprType) -> String {
    let kinds: Vec<&str> = [
        (ExprType::BOOL, "boolean"),
        (ExprType::INT, "integer"),
        (ExprType::FLOAT, "float"),
    ]
    .iter()
    .filter(|(t, _)| fits_widened(t, target))
    .map(|(_, k)| *k)
    .collect();
    let scalars = match kinds.as_slice() {
        [] => return "a string holding one expression".to_string(),
        [one] => (*one).to_string(),
        [a, b] => format!("{a} or {b}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    };
    let article = if scalars.starts_with('i') { "an" } else { "a" };
    format!("{article} {scalars} scalar or a string holding one expression")
}

fn scalar_value(field: &ExprField, target: &ExprType) -> Option<Result<ExprValue, String>> {
    let (value, kind, text) = match field {
        ExprField::Bool(b) => (ExprValue::Bool(*b), "boolean", b.to_string()),
        ExprField::Int(i) => (ExprValue::Int(*i), "integer", i.to_string()),
        ExprField::Float(f) => match openjd_expr::value::Float64::new(*f) {
            // `{:?}` keeps the fraction of a whole number: `3.0`, not `3`.
            Ok(v) => (ExprValue::Float(v), "float", format!("{f:?}")),
            Err(_) => {
                return Some(Err(format!(
                    "the float scalar {f:?} is not a finite number (Template Schemas §7.5)."
                )))
            }
        },
        ExprField::Null => {
            return Some(Err(format!(
                "must not be null: an expression field of type {target} takes {} (Template \
                 Schemas §7.5).",
                accepted_forms(target)
            )))
        }
        ExprField::Expression(_) => return None,
    };
    Some(check_value(value, target).map_err(|_| {
        format!(
            "the {kind} scalar {text} does not fit type {target}: an expression field of type \
             {target} takes {} (Template Schemas §7.5).",
            accepted_forms(target)
        )
    }))
}

/// Template validation of an expression field against `target` (§7.5
/// rules 1, 2, and 4), with `symtab` the field's scope at template
/// validation. Returns the message to report at the field's path.
///
/// A scalar must fit `target`, and a `null` is an error. An expression
/// must parse, its references must be available in `symtab`, and its
/// static type must fit `target` ([`check_static_type`]).
pub(crate) fn validate(
    field: &ExprField,
    target: &ExprType,
    symtab: &SymbolTable,
    ev: &ExprFieldEval<'_>,
) -> Result<(), String> {
    let ExprField::Expression(source) = field else {
        return scalar_value(field, target)
            .expect("every non-expression form is a scalar or null")
            .map(|_| ());
    };
    let parsed = ev.parse(source)?;
    let value = ev
        .evaluate(&parsed, symtab)
        .map_err(|e| format!("Invalid expression: {e}"))?;
    check_static_type(&value.expr_type(), target).map_err(|e| match e {
        FitError::NullType => {
            format!("has type nulltype, which does not fit type {target} (Template Schemas §7.5).")
        }
        FitError::Mismatch(t) => format!(
            "has type {t}, which does not fit type {target}; an expression field converts no \
             type except int to float (Template Schemas §7.5)."
        ),
    })
}

/// Evaluate an expression field against `target` (§7.5), with `symtab`
/// the field's scope at its stage. Returns the value, an `int` widened
/// where `target` wants a `float`, or the message to report at the
/// field's path: a failed evaluation, a `null` result, or a result that
/// does not fit. A scalar or `null` that template validation would reject
/// is rejected with its message.
pub(crate) fn resolve(
    field: &ExprField,
    target: &ExprType,
    symtab: &SymbolTable,
    ev: &ExprFieldEval<'_>,
) -> Result<ExprValue, String> {
    let ExprField::Expression(source) = field else {
        return scalar_value(field, target).expect("every non-expression form is a scalar or null");
    };
    let parsed = ev.parse(source)?;
    let value = ev
        .evaluate(&parsed, symtab)
        .map_err(|e| format!("could not be evaluated: {e}"))?;
    fit_result(value, target)
}

/// An evaluated expression field's value checked against `target`, or the
/// message for a `null` or a value that does not fit.
fn fit_result(value: ExprValue, target: &ExprType) -> Result<ExprValue, String> {
    check_value(value, target).map_err(|e| match e {
        ValueFitError::Null => {
            format!("resolved to null, which does not fit type {target} (Template Schemas §7.5).")
        }
        ValueFitError::Mismatch(t) => format!(
            "resolved to a value of type {t}, which does not fit type {target} (Template \
             Schemas §7.5)."
        ),
    })
}

/// The symbols an expression field reads, or `None` for a scalar, a
/// `null`, or an expression that does not parse.
pub(crate) fn accessed_symbols(
    field: &ExprField,
    ev: &ExprFieldEval<'_>,
) -> Option<std::collections::HashSet<String>> {
    let ExprField::Expression(source) = field else {
        return None;
    };
    ev.parse(source).ok().map(|p| p.accessed_symbols().clone())
}

/// [`resolve`] with some symbols unknown (Expression Language
/// "Progressive Expression Evaluation"): `symtab` may bind a symbol to an
/// `unresolved[T]` value, and a result that depends on one is
/// `Ok(None)`. Template validation has already checked that the
/// expression's static type fits `target`, so an unresolved result is not
/// checked further. A known result is checked as [`resolve`] checks it.
pub(crate) fn resolve_progressive(
    field: &ExprField,
    target: &ExprType,
    symtab: &SymbolTable,
    ev: &ExprFieldEval<'_>,
) -> Result<Option<ExprValue>, String> {
    let ExprField::Expression(source) = field else {
        return resolve(field, target, symtab, ev).map(Some);
    };
    let parsed = ev.parse(source)?;
    let value = ev
        .evaluate(&parsed, symtab)
        .map_err(|e| format!("could not be evaluated: {e}"))?;
    if value.is_unresolved() {
        return Ok(None);
    }
    fit_result(value, target).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(s: &str) -> ExprType {
        ExprType::parse(s).expect("type")
    }

    fn static_fit(t: &str, target: &str) -> Result<(), FitError> {
        check_static_type(&ty(t), &ty(target))
    }

    #[test]
    fn static_fit_exact_choice_and_any() {
        assert_eq!(static_fit("bool", "bool"), Ok(()));
        assert_eq!(static_fit("int", "int | string"), Ok(()));
        assert_eq!(static_fit("string", "int | string"), Ok(()));
        assert_eq!(static_fit("list[int]", "any"), Ok(()));
        assert_eq!(static_fit("list[string]", "list[string]"), Ok(()));
    }

    #[test]
    fn static_fit_no_conversion_but_int_to_float() {
        assert_eq!(
            static_fit("string", "bool"),
            Err(FitError::Mismatch(ExprType::STRING))
        );
        assert_eq!(
            static_fit("int", "bool"),
            Err(FitError::Mismatch(ExprType::INT))
        );
        assert_eq!(
            static_fit("float", "int"),
            Err(FitError::Mismatch(ExprType::FLOAT))
        );
        assert_eq!(static_fit("int", "float"), Ok(()));
        assert_eq!(static_fit("list[int]", "list[float]"), Ok(()));
        assert_eq!(static_fit("list[list[int]]", "list[list[float]]"), Ok(()));
        assert_eq!(static_fit("int | float", "float"), Ok(()));
        assert_eq!(
            static_fit("list[int]", "list[string]"),
            Err(FitError::Mismatch(ty("list[int]")))
        );
    }

    #[test]
    fn static_fit_optional_and_nulltype() {
        // `U?` is accepted where the target does not admit nulltype.
        assert_eq!(static_fit("bool?", "bool"), Ok(()));
        assert_eq!(static_fit("int?", "float"), Ok(()));
        assert_eq!(
            static_fit("string?", "bool"),
            Err(FitError::Mismatch(ExprType::STRING))
        );
        // nulltype is an error unless the target admits it.
        assert_eq!(static_fit("nulltype", "bool"), Err(FitError::NullType));
        assert_eq!(static_fit("nulltype", "bool?"), Ok(()));
        assert_eq!(static_fit("nulltype", "any"), Ok(()));
        assert_eq!(static_fit("bool?", "bool?"), Ok(()));
    }

    #[test]
    fn static_fit_reads_unresolved_as_its_type() {
        assert_eq!(
            check_static_type(&ExprType::unresolved(ExprType::BOOL), &ExprType::BOOL),
            Ok(())
        );
        assert_eq!(
            check_static_type(&ExprType::unresolved(ty("bool?")), &ExprType::BOOL),
            Ok(())
        );
        assert_eq!(
            check_static_type(&ExprType::unresolved(ExprType::STRING), &ExprType::BOOL),
            Err(FitError::Mismatch(ExprType::STRING))
        );
        assert_eq!(
            check_static_type(&ExprType::unresolved(ExprType::INT), &ExprType::FLOAT),
            Ok(())
        );
    }

    #[test]
    fn value_fit_widens_int_only() {
        let float = check_value(ExprValue::Int(3), &ExprType::FLOAT).expect("widens");
        assert_eq!(float.expr_type(), ExprType::FLOAT);
        assert_eq!(float.to_display_string(), "3.0");
        let list = check_value(ExprValue::ListInt(vec![1, 2]), &ty("list[float]")).expect("widens");
        assert_eq!(list.expr_type(), ty("list[float]"));
        assert_eq!(
            check_value(ExprValue::Int(1), &ExprType::BOOL),
            Err(ValueFitError::Mismatch(ExprType::INT))
        );
        assert_eq!(
            check_value(ExprValue::String("true".into()), &ExprType::BOOL),
            Err(ValueFitError::Mismatch(ExprType::STRING))
        );
        // An int stays an int where it fits as is. (`ExprValue`'s `==`
        // compares numbers by value, so compare the type.)
        assert_eq!(
            check_value(ExprValue::Int(3), &ty("int | float")).map(|v| v.expr_type()),
            Ok(ExprType::INT)
        );
    }

    #[test]
    fn value_null_is_an_error_unless_admitted() {
        assert_eq!(
            check_value(ExprValue::Null, &ExprType::BOOL),
            Err(ValueFitError::Null)
        );
        assert_eq!(
            check_value(ExprValue::Null, &ty("bool?")),
            Ok(ExprValue::Null)
        );
        assert_eq!(
            check_value(ExprValue::Null, &ExprType::ANY),
            Ok(ExprValue::Null)
        );
    }

    fn ev_with<'a>(profile: &'a ExprProfile, lib: &'a FunctionLibrary) -> ExprFieldEval<'a> {
        ExprFieldEval {
            profile,
            library: lib,
            memory_limit: None,
            operation_limit: None,
        }
    }

    fn with_ev<R>(f: impl FnOnce(&ExprFieldEval<'_>) -> R) -> R {
        let profile = ExprProfile::current();
        let lib = FunctionLibrary::for_profile(&profile);
        f(&ev_with(&profile, &lib))
    }

    fn expr(s: &str) -> ExprField {
        ExprField::Expression(s.to_string())
    }

    #[test]
    fn validate_scalars_against_target() {
        let st = SymbolTable::new();
        with_ev(|ev| {
            assert_eq!(
                validate(&ExprField::Bool(false), &ExprType::BOOL, &st, ev),
                Ok(())
            );
            assert_eq!(
                validate(&ExprField::Int(3), &ExprType::FLOAT, &st, ev),
                Ok(())
            );
            assert_eq!(
                validate(&ExprField::Float(3.0), &ExprType::FLOAT, &st, ev),
                Ok(())
            );
            assert_eq!(
                validate(&ExprField::Int(1), &ExprType::BOOL, &st, ev),
                Err(
                    "the integer scalar 1 does not fit type bool: an expression field of type \
                     bool takes a boolean scalar or a string holding one expression (Template \
                     Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                validate(&ExprField::Float(3.0), &ExprType::BOOL, &st, ev),
                Err(
                    "the float scalar 3.0 does not fit type bool: an expression field of type \
                     bool takes a boolean scalar or a string holding one expression (Template \
                     Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                validate(&ExprField::Float(1.5), &ExprType::INT, &st, ev),
                Err(
                    "the float scalar 1.5 does not fit type int: an expression field of type \
                     int takes an integer scalar or a string holding one expression (Template \
                     Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                validate(&ExprField::Null, &ty("int | float"), &st, ev),
                Err(
                    "must not be null: an expression field of type float | int takes an \
                     integer or float scalar or a string holding one expression (Template \
                     Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                validate(&ExprField::Bool(true), &ExprType::STRING, &st, ev),
                Err(
                    "the boolean scalar true does not fit type string: an expression field \
                     of type string takes a string holding one expression (Template Schemas \
                     §7.5)."
                        .to_string()
                )
            );
        });
    }

    #[test]
    fn validate_expressions() {
        let mut st = SymbolTable::new();
        st.set("Param.Mode", ExprValue::unresolved(ExprType::STRING))
            .expect("set");
        st.set("Param.Count", ExprValue::unresolved(ExprType::INT))
            .expect("set");
        with_ev(|ev| {
            assert_eq!(validate(&expr("true"), &ExprType::BOOL, &st, ev), Ok(()));
            assert_eq!(
                validate(&expr("Param.Mode == 'on'"), &ExprType::BOOL, &st, ev),
                Ok(())
            );
            assert_eq!(
                validate(
                    &expr("None if Param.Mode == 'x' else true"),
                    &ExprType::BOOL,
                    &st,
                    ev
                ),
                Ok(())
            );
            assert_eq!(
                validate(&expr("Param.Count"), &ExprType::FLOAT, &st, ev),
                Ok(())
            );
            assert_eq!(
                validate(&expr("Param.Mode"), &ExprType::BOOL, &st, ev),
                Err(
                    "has type string, which does not fit type bool; an expression field \
                     converts no type except int to float (Template Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                validate(&expr("None"), &ExprType::BOOL, &st, ev),
                Err(
                    "has type nulltype, which does not fit type bool (Template Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                validate(&expr("{{ true }}"), &ExprType::BOOL, &st, ev),
                Err(
                    "Invalid expression: Set literals are not supported. An expression field \
                     takes an expression without '{{ }}' (Template Schemas §7.5).\n  {{ true }}\n  \
                     ^~~~~~~~~~"
                        .to_string()
                )
            );
            // No hint for a parse error of a value without `{{`.
            assert_eq!(
                validate(&expr("1 +"), &ExprType::BOOL, &st, ev),
                Err("Invalid expression: Syntax error: invalid syntax\n  1 +\n  ^~~".to_string())
            );
            assert_eq!(
                validate(&expr("Param.Nope"), &ExprType::BOOL, &st, ev),
                Err(
                    "Invalid expression: Undefined variable: 'Param.Nope'. Did you mean: \
                     Param.Mode\n  Param.Nope\n  ~~~~~~^~~~"
                        .to_string()
                )
            );
            // A '{{' inside a string literal is text, not a format string.
            assert_eq!(
                validate(&expr("'{{x}}' != Param.Mode"), &ExprType::BOOL, &st, ev),
                Ok(())
            );
        });
    }

    #[test]
    fn format_string_hint_placement() {
        let hint = "An expression field takes an expression without '{{ }}' (Template Schemas \
                    §7.5).";
        // After the summary, before the expression and caret lines.
        assert_eq!(
            with_format_string_hint("Bad thing\n  {{x}}\n  ^", "Bad thing"),
            format!("Bad thing. {hint}\n  {{{{x}}}}\n  ^")
        );
        // A summary that already ends a sentence gets no second period.
        assert_eq!(
            with_format_string_hint("Bad thing.\n  {{x}}", "Bad thing."),
            format!("Bad thing. {hint}\n  {{{{x}}}}")
        );
        // So does one that ends a question.
        assert_eq!(
            with_format_string_hint("Forgot a comma?\n  {{x}}", "Forgot a comma?"),
            format!("Forgot a comma? {hint}\n  {{{{x}}}}")
        );
        assert_eq!(
            with_format_string_hint("Stop!\n  {{x}}", "Stop!"),
            format!("Stop! {hint}\n  {{{{x}}}}")
        );
        // A summary that spans lines: after the whole summary.
        assert_eq!(
            with_format_string_hint("Bad\nthing\n  {{x}}", "Bad\nthing"),
            format!("Bad\nthing. {hint}\n  {{{{x}}}}")
        );
        // Text that does not start with the summary: before the first line
        // break, or at the end when there is none.
        assert_eq!(
            with_format_string_hint("Other\n  {{x}}", "Bad thing"),
            format!("Other. {hint}\n  {{{{x}}}}")
        );
        assert_eq!(
            with_format_string_hint("Other", "Bad thing"),
            format!("Other. {hint}")
        );
    }

    #[test]
    fn the_hint_needs_a_double_brace() {
        // A single brace is a set literal, not a format string.
        with_ev(|ev| {
            assert_eq!(
                validate(&expr("{1}"), &ExprType::BOOL, &SymbolTable::new(), ev),
                Err("Invalid expression: Set literals are not supported\n  {1}\n  ^~~".to_string())
            );
        });
    }

    #[test]
    fn accepted_forms_name_the_scalars_that_fit() {
        assert_eq!(
            accepted_forms(&ExprType::BOOL),
            "a boolean scalar or a string holding one expression"
        );
        // An int widens to float, so a float target takes both.
        assert_eq!(
            accepted_forms(&ExprType::FLOAT),
            "an integer or float scalar or a string holding one expression"
        );
        assert_eq!(
            accepted_forms(&ExprType::ANY),
            "a boolean, integer, or float scalar or a string holding one expression"
        );
        assert_eq!(
            accepted_forms(&ty("list[int]")),
            "a string holding one expression"
        );
    }

    #[test]
    fn a_non_finite_float_scalar_is_an_error() {
        // Not reachable by decoding (YAML delivers `.inf` and `.nan` as
        // strings), but a template built in code may hold one.
        with_ev(|ev| {
            assert_eq!(
                validate(
                    &ExprField::Float(f64::NAN),
                    &ExprType::FLOAT,
                    &SymbolTable::new(),
                    ev
                ),
                Err(
                    "the float scalar NaN is not a finite number (Template Schemas §7.5)."
                        .to_string()
                )
            );
        });
    }

    #[test]
    fn resolve_values() {
        let mut st = SymbolTable::new();
        st.set("Param.Mode", ExprValue::String("off".into()))
            .expect("set");
        with_ev(|ev| {
            assert_eq!(
                resolve(&expr("Param.Mode == 'on'"), &ExprType::BOOL, &st, ev),
                Ok(ExprValue::Bool(false))
            );
            assert_eq!(
                resolve(&ExprField::Bool(true), &ExprType::BOOL, &st, ev),
                Ok(ExprValue::Bool(true))
            );
            assert_eq!(
                resolve(&expr("2"), &ExprType::FLOAT, &st, ev).map(|v| v.expr_type()),
                Ok(ExprType::FLOAT)
            );
            assert_eq!(
                resolve(&ExprField::Int(2), &ExprType::FLOAT, &st, ev).map(|v| v.expr_type()),
                Ok(ExprType::FLOAT)
            );
            assert_eq!(
                resolve(
                    &expr("None if Param.Mode == 'off' else true"),
                    &ExprType::BOOL,
                    &st,
                    ev
                ),
                Err(
                    "resolved to null, which does not fit type bool (Template Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                resolve(&expr("Param.Mode"), &ExprType::BOOL, &st, ev),
                Err(
                    "resolved to a value of type string, which does not fit type bool \
                     (Template Schemas §7.5)."
                        .to_string()
                )
            );
            assert_eq!(
                resolve(&expr("[1][5]"), &ExprType::BOOL, &st, ev),
                Err(
                    "could not be evaluated: Index 5 out of bounds for list of length 1\n  \
                     [1][5]\n  ~~~^~~"
                        .to_string()
                )
            );
        });
    }

    #[test]
    fn decode_forms() {
        let d = |s: &str| serde_json::from_str::<ExprField>(s);
        assert_eq!(d("true").expect("bool"), ExprField::Bool(true));
        assert_eq!(d("3").expect("int"), ExprField::Int(3));
        assert_eq!(d("3.0").expect("float"), ExprField::Float(3.0));
        assert_eq!(d("\"true\"").expect("string"), expr("true"));
        // A string is kept as written; the parser trims it.
        assert_eq!(d("\" true \"").expect("string"), expr(" true "));
        assert_eq!(d("null").expect("null"), ExprField::Null);
        assert_eq!(
            d("[true]").expect_err("list").to_string(),
            "an expression field must be a boolean, integer, float, or string (Template Schemas \
             §7.5)"
        );
        assert_eq!(
            d("18446744073709551615").expect_err("u64").to_string(),
            "the integer 18446744073709551615 is out of range for an expression field (Template \
             Schemas §7.5)"
        );
        // Beyond u64 (or below i64), JSON number parsing gives a float.
        assert_eq!(
            d("-9223372036854775809").expect("float"),
            ExprField::Float(-9223372036854775809.0)
        );
        // `deserialize_enable_if` names the field and keeps a present null.
        let e = |s: &str| {
            deserialize_enable_if(&mut serde_json::Deserializer::from_str(s))
                .map_err(|e| e.to_string())
        };
        assert_eq!(e("null"), Ok(Some(ExprField::Null)));
        assert_eq!(e("false"), Ok(Some(ExprField::Bool(false))));
        assert_eq!(
            e("{}"),
            Err(
                "enableIf must be a boolean, integer, float, or string (an expression field, \
                 Template Schemas §7.5)"
                    .to_string()
            )
        );
        assert_eq!(
            e("18446744073709551615"),
            Err(
                "enableIf: the integer 18446744073709551615 is out of range for an expression \
                 field (Template Schemas §7.5)"
                    .to_string()
            )
        );
        assert_ne!(ExprField::Float(0.0), ExprField::Float(-0.0));
        assert_eq!(ExprField::Float(1.5), ExprField::Float(1.5));
        assert_ne!(ExprField::Int(1), ExprField::Float(1.0));
        assert_eq!(ExprField::Int(2), ExprField::Int(2));
        assert_ne!(ExprField::Int(1), ExprField::Int(2));
        assert_eq!(expr("ab"), expr("ab"));
        assert_ne!(expr("ab"), expr("ba"));
        assert_eq!(ExprField::Bool(true), ExprField::Bool(true));
        assert_ne!(ExprField::Bool(true), ExprField::Bool(false));
    }
}
