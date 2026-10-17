// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Task parameter space and range resolution.

use std::borrow::Cow;

use indexmap::IndexMap;

use openjd_expr::symbol_table::SymbolTable;
use openjd_expr::value::Float64;
use openjd_expr::ExprValue;
use openjd_expr::RangeExpr;

use crate::error::ModelError;
use crate::job;
use crate::template;
use crate::template::validate_v2023_09::EffectiveLimits;
use openjd_expr::ExpressionError;

/// Resolve an optional float field (`hostRequirements` amount `min`/`max`).
///
/// A single whole-field expression resolves with target type `float?`
/// (Expression Language §1.3.2 — optional field): `Ok(None)` means the
/// expression resolved to `null` and the field is treated as omitted.
/// Multi-segment strings concatenate and parse, whitespace-tolerated.
pub(super) fn resolve_to_f64(
    fs: &openjd_expr::FormatString,
    symtab: &SymbolTable,
    context: &str,
    budgets: super::EvalBudgets,
) -> Result<Option<f64>, ModelError> {
    let target = openjd_expr::ExprType::union(vec![
        openjd_expr::ExprType::FLOAT,
        openjd_expr::ExprType::NULLTYPE,
    ]);
    let resolved = fs
        .resolve_with(symtab, &budgets.fs_options().with_target_type(&target))
        .map_err(|e| ModelError::FormatStringError {
            message: format!("{context}: {e}"),
            input: Some(fs.raw().to_string()),
            start: None,
            end: None,
        })?;
    let value = match resolved {
        openjd_expr::ExprValue::Null => return Ok(None),
        // A coerced Float is always finite: Float64 excludes NaN and the
        // infinities by construction.
        openjd_expr::ExprValue::Float(f) => f.value(),
        // Multi-segment string: parse the concatenated text. Only this
        // path can produce a non-finite value (e.g. "1e999" parses to
        // inf), so the finite check lives here where the user's text is
        // available for the message.
        other => {
            let s = other.to_display_string();
            let value = s.trim().parse::<f64>().map_err(|_| {
                ModelError::Expression(ExpressionError::new(format!(
                    "{context}: '{s}' is not a valid number"
                )))
            })?;
            if !value.is_finite() {
                return Err(ModelError::Expression(ExpressionError::new(format!(
                    "{context}: '{s}' is not a finite number"
                ))));
            }
            value
        }
    };
    Ok(Some(value))
}

/// Resolve a list of FormatStrings to strings.
///
/// Each element is a list item, so per Expression Language §1.3.2 a
/// whole-field expression resolves with target type
/// `string? | list[string]`: a `null` result skips the element, a list
/// result flattens inline, and a string is a single element. Callers
/// enforcing a non-empty list must re-check after resolution, since
/// null-skips can empty it.
pub(super) fn resolve_string_list(
    vals: &[openjd_expr::FormatString],
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
) -> Result<Vec<String>, ModelError> {
    let target = openjd_expr::ExprType::union(vec![
        openjd_expr::ExprType::NULLTYPE,
        openjd_expr::ExprType::STRING,
        openjd_expr::ExprType::list(openjd_expr::ExprType::STRING),
    ]);
    let mut out = Vec::new();
    for fs in vals {
        let value = fs
            .resolve_with(symtab, &budgets.fs_options().with_target_type(&target))
            .map_err(|e| ModelError::FormatStringError {
                message: e.to_string(),
                input: Some(fs.raw().to_string()),
                start: None,
                end: None,
            })?;
        match value {
            openjd_expr::ExprValue::Null => continue,
            val if val.is_list() => {
                for elem in val.list_elements().unwrap_or_default() {
                    out.push(elem.to_display_string());
                }
            }
            openjd_expr::ExprValue::String(s) => out.push(s),
            other => out.push(other.to_display_string()),
        }
    }
    Ok(out)
}

/// The range rules that depend on the template's extensions.
#[derive(Debug, Clone, Copy)]
pub(super) struct RangeRules {
    /// `EXPR` is enabled: a list item resolves as an expression with a
    /// target type (Expression Language §1.3.2).
    pub(super) expr: bool,
    /// `CONTROL_FLOW` is enabled (RFC 0010 §Task parameter ranges).
    pub(super) control_flow: bool,
}

impl RangeRules {
    /// The most values a resolved range may have, or `None` for no cap.
    ///
    /// Without `CONTROL_FLOW`, every resolved list range is capped at
    /// `max_task_param_range_len` (§3.4.1), whether it came from a list
    /// literal or a format string, and an `<IntRangeExpr>` is not. With
    /// `CONTROL_FLOW` (RFC 0010 §Range length) the cap applies to a list
    /// literal's items as written, which the structure pass checks, and not
    /// to resolved values. The RFC may still key this relaxation on ranges
    /// that read `Steps.*` (plan ambiguity A3); this is the one place to
    /// change.
    fn resolved_cap(self, limits: &EffectiveLimits) -> Option<usize> {
        (!self.control_flow).then_some(limits.max_task_param_range_len)
    }

    /// Check a resolved range's length against [`resolved_cap`](Self::resolved_cap).
    fn check_len(
        self,
        len: usize,
        param_name: &str,
        limits: &EffectiveLimits,
    ) -> Result<(), ModelError> {
        match self.resolved_cap(limits) {
            Some(cap) if len > cap => Err(ModelError::DecodeValidation(format!(
                "Task parameter '{param_name}' range exceeds {cap} elements ({len} elements)"
            ))),
            _ => Ok(()),
        }
    }

    /// A whole-field range expression that resolved to `null`. With
    /// `CONTROL_FLOW` a conditional producer's outputs are `T?`, so a `null`
    /// range is an ordinary mistake and gets its own message (RFC 0010 §How a
    /// Step is bound item 3: "a `null` where a value is required"); without
    /// it the existing messages are kept.
    fn check_not_null(self, value: &ExprValue, param_name: &str) -> Result<(), ModelError> {
        if self.control_flow && matches!(value, ExprValue::Null) {
            return Err(ModelError::DecodeValidation(format!(
                "Task parameter '{param_name}' range must not resolve to null"
            )));
        }
        Ok(())
    }

    /// A list literal whose items were all skipped (`null`) or flattened
    /// from empty lists. An error without `CONTROL_FLOW`; with it, the
    /// result follows the rules for a format-string range and may be empty
    /// (RFC 0010 §Empty parameter spaces).
    fn check_list_literal_not_emptied(
        self,
        len: usize,
        param_name: &str,
    ) -> Result<(), ModelError> {
        if len == 0 && !self.control_flow {
            return Err(ModelError::DecodeValidation(format!(
                "Task parameter '{param_name}' range has no elements after resolution"
            )));
        }
        Ok(())
    }
}

/// Resolve a StepParameterSpaceDefinition into a StepParameterSpace with concrete ranges.
pub(super) fn resolve_parameter_space(
    ps: &template::StepParameterSpaceDefinition,
    symtab: &SymbolTable,
    limits: &EffectiveLimits,
    budgets: super::EvalBudgets,
    rules: RangeRules,
) -> Result<job::StepParameterSpace, ModelError> {
    resolve_parameter_space_by_field(ps, symtab, limits, budgets, rules).map_err(|(_, e)| e.error)
}

/// The field of a Task parameter whose resolution failed: `range`, or
/// `chunks` for its `chunks` fields.
pub(super) struct TaskParamError {
    pub(super) field: &'static str,
    pub(super) error: ModelError,
}

fn on_range(error: ModelError) -> TaskParamError {
    TaskParamError {
        field: "range",
        error,
    }
}

fn on_chunks(error: ModelError) -> TaskParamError {
    TaskParamError {
        field: "chunks",
        error,
    }
}

/// [`resolve_parameter_space`], with a failure tagged by the index of its
/// Task parameter and the field that failed, so step input binding can
/// report it at its path.
pub(super) fn resolve_parameter_space_by_field(
    ps: &template::StepParameterSpaceDefinition,
    symtab: &SymbolTable,
    limits: &EffectiveLimits,
    budgets: super::EvalBudgets,
    rules: RangeRules,
) -> Result<job::StepParameterSpace, (usize, TaskParamError)> {
    let mut defs = IndexMap::new();
    for (k, tp) in ps.task_parameter_definitions.iter().enumerate() {
        let name = tp.name().to_string();
        let param =
            resolve_task_parameter(tp, symtab, limits, budgets, rules).map_err(|e| (k, e))?;
        defs.insert(name, param);
    }
    Ok(job::StepParameterSpace {
        task_parameter_definitions: defs,
        combination: ps.combination.clone(),
    })
}

fn resolve_task_parameter(
    tp: &template::TaskParameterDefinition,
    symtab: &SymbolTable,
    limits: &EffectiveLimits,
    budgets: super::EvalBudgets,
    rules: RangeRules,
) -> Result<job::TaskParameter, TaskParamError> {
    match tp {
        template::TaskParameterDefinition::INT(p) => {
            let range =
                resolve_int_range(&p.range, symtab, p.name.as_str(), limits, budgets, rules)
                    .map_err(on_range)?;
            Ok(job::TaskParameter::Int {
                range,
                chunks: None,
            })
        }
        template::TaskParameterDefinition::FLOAT(p) => {
            let range =
                resolve_float_range(&p.range, symtab, p.name.as_str(), limits, budgets, rules)
                    .map_err(on_range)?;
            Ok(job::TaskParameter::Float { range })
        }
        template::TaskParameterDefinition::STRING(p) => {
            let range = resolve_string_range(
                &p.range,
                symtab,
                p.name.as_str(),
                false,
                limits,
                budgets,
                rules,
            )
            .map_err(on_range)?;
            Ok(job::TaskParameter::String { range })
        }
        template::TaskParameterDefinition::PATH(p) => {
            let range = resolve_string_range(
                &p.range,
                symtab,
                p.name.as_str(),
                true,
                limits,
                budgets,
                rules,
            )
            .map_err(on_range)?;
            Ok(job::TaskParameter::Path { range })
        }
        template::TaskParameterDefinition::CHUNK_INT(p) => {
            let range =
                resolve_int_range(&p.range, symtab, p.name.as_str(), limits, budgets, rules)
                    .map_err(on_range)?;
            // CHUNK[INT] regroups values into generated RangeExpr chunks,
            // which bound values to |v| < 2^62. List ranges accept full
            // i64, so reject out-of-bound values here — at job creation,
            // with a path-annotated error — rather than panicking when a
            // chunk is built during iteration.
            if let job::TaskParamRange::List(values) = &range {
                if let Some(v) = values
                    .iter()
                    .find(|v| v.unsigned_abs() >= openjd_expr::MAX_RANGE_VALUE_MAGNITUDE as u64)
                {
                    return Err(on_range(ModelError::DecodeValidation(format!(
                        "Task parameter '{}': value {} exceeds the CHUNK[INT] \
                         range value bound (magnitude must be below 2^62)",
                        p.name.as_str(),
                        v
                    ))));
                }
            }
            // Chunks fields are `<integer> | <intstring>`: a single
            // whole-field expression resolves with target type `int`
            // (Expression Language §1.2.3 — every field gives its
            // expression a target type; there are no null semantics
            // here), so `{{ 4.0 }}` coerces to 4. A multi-segment string
            // concatenates and parses like Python's int(), tolerating
            // surrounding whitespace. Template validation applies the same
            // targeting, so the two stages accept identical values.
            let resolve_chunk_int = |fs: &crate::FormatString,
                                     field: &str|
             -> Result<i64, ModelError> {
                let value = fs
                    .resolve_with(
                        symtab,
                        &budgets
                            .fs_options()
                            .with_target_type(&openjd_expr::ExprType::INT),
                    )
                    .map_err(|e| {
                        ModelError::Expression(ExpressionError::new(format!("chunks.{field}: {e}")))
                    })?;
                match value {
                    openjd_expr::ExprValue::Int(n) => Ok(n),
                    other => {
                        let s = other.to_display_string();
                        s.trim().parse::<i64>().map_err(|_| {
                            ModelError::Expression(ExpressionError::new(format!(
                                "chunks.{field}: '{s}' is not a valid integer"
                            )))
                        })
                    }
                }
            };
            let default_task_count = match &p.chunks.default_task_count {
                template::IntOrFormatString::Int(n) => (*n).max(1) as usize,
                template::IntOrFormatString::FormatString(fs) => {
                    let count = resolve_chunk_int(fs, "defaultTaskCount").map_err(on_chunks)?;
                    // §3.4.1.5 sets a minimum of 1. The bound cannot be applied
                    // at decode when the value is a format string, so it is
                    // applied here. Rejecting rather than clamping: a resolved 0
                    // silently became 1, which ran the job with a chunk shape the
                    // author never asked for.
                    if count < 1 {
                        return Err(on_chunks(ModelError::Expression(ExpressionError::new(
                            format!(
                                "chunks.defaultTaskCount: resolved to {count}, but must be >= 1"
                            ),
                        ))));
                    }
                    count as usize
                }
            };
            let target_runtime_seconds = p
                .chunks
                .target_runtime_seconds
                .as_ref()
                .map(|v| match v {
                    template::IntOrFormatString::Int(n) => Ok(Some((*n).max(0) as usize)),
                    template::IntOrFormatString::FormatString(fs) => {
                        // Optional field: target type `int?` — a
                        // whole-field null means "field omitted".
                        let target = openjd_expr::ExprType::union(vec![
                            openjd_expr::ExprType::INT,
                            openjd_expr::ExprType::NULLTYPE,
                        ]);
                        let value = fs
                            .resolve_with(
                                symtab,
                                &budgets.fs_options().with_target_type(&target),
                            )
                            .map_err(|e| {
                                ModelError::Expression(ExpressionError::new(format!(
                                    "chunks.targetRuntimeSeconds: {e}"
                                )))
                            })?;
                        let seconds = match value {
                            openjd_expr::ExprValue::Null => return Ok(None),
                            openjd_expr::ExprValue::Int(n) => n,
                            other => {
                                let s = other.to_display_string();
                                s.trim().parse::<i64>().map_err(|_| {
                                    ModelError::Expression(ExpressionError::new(format!(
                                        "chunks.targetRuntimeSeconds: '{s}' is not a valid integer"
                                    )))
                                })?
                            }
                        };
                        // §3.4.1.5 sets a minimum of 0; same deferral as
                        // defaultTaskCount above, and the same reason to reject
                        // rather than clamp.
                        if seconds < 0 {
                            return Err(ModelError::Expression(ExpressionError::new(format!(
                                "chunks.targetRuntimeSeconds: resolved to {seconds}, but must be >= 0"
                            ))));
                        }
                        Ok(Some(seconds as usize))
                    }
                })
                .transpose()
                .map_err(on_chunks)?
                .flatten();
            let chunks = job::ResolvedChunks {
                default_task_count,
                target_runtime_seconds,
                range_constraint: p.chunks.range_constraint.clone(),
            };
            Ok(job::TaskParameter::ChunkInt { range, chunks })
        }
    }
}

fn resolve_int_range(
    range: &template::IntRange,
    symtab: &SymbolTable,
    param_name: &str,
    limits: &EffectiveLimits,
    budgets: super::EvalBudgets,
    rules: RangeRules,
) -> Result<job::TaskParamRange<i64>, ModelError> {
    match range {
        template::IntRange::List(items) => {
            let mut ints: Vec<i64> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    template::IntRangeItem::Int(i) => ints.push(*i),
                    template::IntRangeItem::FormatString(fs) => {
                        resolve_int_list_item(fs, symtab, budgets, rules, &mut ints)?
                    }
                }
            }
            rules.check_list_literal_not_emptied(ints.len(), param_name)?;
            rules.check_len(ints.len(), param_name, limits)?;
            Ok(job::TaskParamRange::List(ints))
        }
        template::IntRange::Expression(expr) => {
            // Try typed evaluation first — may directly yield a RangeExpr or list[int].
            // For multi-segment format strings (e.g., "1-{{Param.Count}}"), typed
            // evaluation fails and we fall through to string resolution, which
            // concatenates segments and parses the result as a range expression.
            // Any real evaluation errors (division by zero, type errors) will be
            // caught by the string resolution fallback path.
            if let Ok(val) = expr.resolve_with(symtab, &budgets.fs_options()) {
                rules.check_not_null(&val, param_name)?;
                match val {
                    // Range expressions are not length-capped; only the list
                    // forms are. See `EffectiveLimits::max_task_param_range_len`.
                    ExprValue::RangeExpr(r) => {
                        return Ok(job::TaskParamRange::RangeExpr(r));
                    }
                    val if val.is_list() => {
                        let elements = val.list_elements().unwrap();
                        let ints: Result<Vec<i64>, _> = elements
                            .iter()
                            .map(|e| match e {
                                ExprValue::Int(i) => Ok(*i),
                                other => Err(ModelError::Expression(ExpressionError::new(
                                    format!("Expected int in range, got {}", other.type_name()),
                                ))),
                            })
                            .collect();
                        let ints = ints?;
                        rules.check_len(ints.len(), param_name, limits)?;
                        return Ok(job::TaskParamRange::List(ints));
                    }
                    _ => {}
                }
            }
            let resolved = expr
                .resolve_string_with(symtab, &budgets.fs_options())
                .map_err(ModelError::Expression)?;
            let range_expr: RangeExpr =
                resolved
                    .parse()
                    .map_err(|e: openjd_expr::ExpressionError| {
                        if rules.control_flow {
                            range_string_error(expr, &resolved, symtab, &e)
                        } else {
                            ModelError::Expression(e)
                        }
                    })?;
            Ok(job::TaskParamRange::RangeExpr(range_expr))
        }
    }
}

/// A range string that does not parse once its expressions are evaluated,
/// under `CONTROL_FLOW`: the message names the expression as written and the
/// text it resolved to (RFC 0010 §How a Step is bound item 3), and each
/// symbol it reads whose value is `null`, such as a skipped producer's output
/// in `"1-{{Steps.P.Output.N}}"`.
fn range_string_error(
    expr: &openjd_expr::FormatString,
    resolved: &str,
    symtab: &SymbolTable,
    e: &openjd_expr::ExpressionError,
) -> ModelError {
    let mut nulls: Vec<String> = expr
        .accessed_symbols()
        .into_iter()
        .filter(|s| matches!(symtab.get_value(s), Some(ExprValue::Null)))
        .collect();
    nulls.sort();
    nulls.dedup();
    let hint = match nulls.as_slice() {
        [] => String::new(),
        [one] => format!(" ('{one}' is null)"),
        many => format!(
            " ({} are null)",
            many.iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    ModelError::DecodeValidation(format!(
        "'{}' resolved to '{resolved}'{hint}, which is not a range expression: {e}",
        expr.raw()
    ))
}

/// Resolve one format-string item of an `<IntRangeList>` into `out`.
///
/// As a list item it resolves with target type `int? | list[int]`
/// (Expression Language §1.3.2): `null` skips the item, a list is flattened
/// inline, and an integer is one value. Without `EXPR` the item is a
/// `<TaskParameterStringValue>`, which must resolve to the string form of an
/// integer (Template Schemas §3.4.1.1).
fn resolve_int_list_item(
    fs: &openjd_expr::FormatString,
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
    rules: RangeRules,
    out: &mut Vec<i64>,
) -> Result<(), ModelError> {
    let parse = |s: String| {
        s.trim().parse::<i64>().map_err(|_| {
            ModelError::Expression(ExpressionError::new(format!(
                "Cannot parse '{s}' as integer"
            )))
        })
    };
    if !rules.expr {
        let s = fs
            .resolve_string_with(symtab, &budgets.fs_options())
            .map_err(ModelError::Expression)?;
        out.push(parse(s)?);
        return Ok(());
    }
    let target = openjd_expr::ExprType::union(vec![
        openjd_expr::ExprType::NULLTYPE,
        openjd_expr::ExprType::INT,
        openjd_expr::ExprType::list(openjd_expr::ExprType::INT),
    ]);
    let value = fs
        .resolve_with(symtab, &budgets.fs_options().with_target_type(&target))
        .map_err(ModelError::Expression)?;
    match value {
        ExprValue::Null => {}
        ExprValue::Int(i) => out.push(i),
        val if val.is_list() => {
            for e in val.list_elements().unwrap_or_default() {
                match e {
                    ExprValue::Int(i) => out.push(i),
                    other => {
                        return Err(ModelError::Expression(ExpressionError::new(format!(
                            "Expected int in range, got {}",
                            other.type_name()
                        ))))
                    }
                }
            }
        }
        other => out.push(parse(other.to_display_string())?),
    }
    Ok(())
}

/// A `Float64` with no preserved spelling — a `<float>` literal, or a value whose
/// text could not be carried. Renders through `format_float`.
fn float64(value: f64, param_name: &str) -> Result<Float64, ModelError> {
    Float64::new(value).map_err(|_| {
        ModelError::Expression(ExpressionError::new(format!(
            "FLOAT parameter '{param_name}' range value {value} is not finite"
        )))
    })
}

/// Template Schemas §7.5 rule 1, keeping the sign and one digit: `'02.50'` ->
/// `2.50`, `'007'` -> `7`, `'000'` -> `0`, `'0.50'` unchanged. Textual, so it
/// cannot lose precision or change notation: `'01E+2'` -> `1E+2`.
pub(crate) fn strip_redundant_leading_zeros(text: &str) -> Cow<'_, str> {
    let sign_len = usize::from(text.starts_with(['+', '-']));
    let digits = &text[sign_len..];
    let zeros = digits.bytes().take_while(|b| *b == b'0').count();
    // Redundant only when another digit follows. Otherwise the last zero is the
    // integer part, as in '0.50' or '000'.
    let strip = if digits.as_bytes().get(zeros).is_some_and(u8::is_ascii_digit) {
        zeros
    } else {
        zeros.saturating_sub(1)
    };
    if strip == 0 {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() - strip);
    out.push_str(&text[..sign_len]);
    out.push_str(&digits[strip..]);
    Cow::Owned(out)
}

/// Resolve one format-string item of a FLOAT range list into `out`.
///
/// As a list item it has target type `float? | list[float]` (Expression
/// Language §1.3.2): `null` skips the item and a list is flattened inline,
/// each element taking its value only (as in a `<ListExpressionString>`).
/// Any other result is resolved as text and parsed, so the item keeps its
/// written spelling (§7.5 rule 2).
fn resolve_float_list_item(
    fs: &openjd_expr::FormatString,
    symtab: &SymbolTable,
    param_name: &str,
    limits: &EffectiveLimits,
    budgets: super::EvalBudgets,
    rules: RangeRules,
    out: &mut Vec<Float64>,
) -> Result<(), ModelError> {
    let target = openjd_expr::ExprType::union(vec![
        openjd_expr::ExprType::NULLTYPE,
        openjd_expr::ExprType::FLOAT,
        openjd_expr::ExprType::list(openjd_expr::ExprType::FLOAT),
    ]);
    let typed = fs.resolve_with(symtab, &budgets.fs_options().with_target_type(&target));
    let typed_error = typed
        .as_ref()
        .err()
        .map(|e| ModelError::Expression(e.clone()));
    match typed {
        Ok(ExprValue::Null) => return Ok(()),
        Ok(val) if val.is_list() => {
            for e in val.list_elements().unwrap_or_default() {
                out.push(match e {
                    ExprValue::Float(f) => float64(f.value(), param_name)?,
                    ExprValue::Int(i) => float64(i as f64, param_name)?,
                    other => {
                        return Err(ModelError::Expression(ExpressionError::new(format!(
                            "Expected float in range, got {}",
                            other.type_name()
                        ))))
                    }
                });
            }
            return Ok(());
        }
        // A scalar, or an error the text path reports in its own words.
        _ => {}
    }
    let resolved = fs
        .resolve_string_with(symtab, &budgets.fs_options())
        .map_err(ModelError::Expression)?;
    let trimmed = resolved.trim();
    // With CONTROL_FLOW, when the typed resolution failed too, its message
    // (the type and a caret) says more than "cannot parse"; without it the
    // earlier message is kept.
    let typed_error = typed_error.filter(|_| rules.control_flow);
    let value = trimmed.parse::<f64>().map_err(|_| {
        typed_error.unwrap_or_else(|| {
            ModelError::Expression(ExpressionError::new(format!(
                "Cannot parse '{}' as float",
                resolved
            )))
        })
    })?;
    if !value.is_finite() {
        return Err(ModelError::Expression(ExpressionError::new(format!(
            "FLOAT parameter '{param_name}' range value '{resolved}' is not finite"
        ))));
    }
    // §7.5 rule 2: the f64 cannot carry the decimal places, so
    // the text rides alongside it.
    let text = strip_redundant_leading_zeros(trimmed);
    // Same per-element cap resolve_string_range applies, for the
    // same reason: a <FormatString> resolves to arbitrary length,
    // and this text is what lands on a command line.
    //
    // Over the cap this downgrades to value-only where the STRING
    // and PATH paths error. Deliberate: before this change a long
    // <floatstring> was parsed to an f64 and its text discarded,
    // so erroring would reject templates that were valid, whereas
    // a STRING element over the cap was always an error.
    if text.chars().count() > limits.max_task_param_string_len {
        out.push(float64(value, param_name)?);
        return Ok(());
    }
    out.push(Float64::with_str(value, text.into_owned()).map_err(ModelError::Expression)?);
    Ok(())
}

fn resolve_float_range(
    range: &template::FloatRange,
    symtab: &SymbolTable,
    param_name: &str,
    limits: &EffectiveLimits,
    budgets: super::EvalBudgets,
    rules: RangeRules,
) -> Result<Vec<Float64>, ModelError> {
    let floats: Vec<Float64> = match range {
        template::FloatRange::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for v in items {
                match v {
                    // `2.50` and `2.5` are the same literal after parsing, so a
                    // `<float>` makes no request about how it renders.
                    template::FloatRangeItem::Float(f) => out.push(float64(*f, param_name)?),
                    template::FloatRangeItem::FormatString(fs) => resolve_float_list_item(
                        fs, symtab, param_name, limits, budgets, rules, &mut out,
                    )?,
                }
            }
            rules.check_list_literal_not_emptied(out.len(), param_name)?;
            out
        }
        template::FloatRange::Expression(expr) => {
            // Typed evaluation — must yield a list. Propagate the actual error
            // if evaluation fails.
            match expr.resolve_with(symtab, &budgets.fs_options()) {
                Ok(val) if val.is_list() => {
                    let elements = val.list_elements().unwrap();
                    elements
                        .iter()
                        .map(|e| match e {
                            // No text: an expression element came from a literal
                            // or an int, so `{{ [2.50] }}` is 2.5 (§7.5).
                            ExprValue::Float(f) => float64(f.value(), param_name),
                            ExprValue::Int(i) => float64(*i as f64, param_name),
                            other => Err(ModelError::Expression(ExpressionError::new(format!(
                                "Expected float in range, got {}",
                                other.type_name()
                            )))),
                        })
                        .collect::<Result<Vec<_>, _>>()?
                }
                Ok(val) => {
                    rules.check_not_null(&val, param_name)?;
                    return Err(ModelError::Expression(ExpressionError::new(
                        "Float range expression must evaluate to a list",
                    )));
                }
                Err(e) => {
                    return Err(ModelError::Expression(ExpressionError::new(format!(
                        "Float range expression: {e}"
                    ))));
                }
            }
        }
    };
    rules.check_len(floats.len(), param_name, limits)?;
    Ok(floats)
}

fn resolve_string_range(
    range: &template::StringRange,
    symtab: &SymbolTable,
    param_name: &str,
    is_path: bool,
    limits: &EffectiveLimits,
    budgets: super::EvalBudgets,
    rules: RangeRules,
) -> Result<Vec<String>, ModelError> {
    let resolved: Vec<String> = match range {
        // Each range element is a list item, so per Expression Language
        // §1.3.2 a whole-field expression resolves with target type
        // `string? | list[string]`: a `null` result skips the element, a
        // list result flattens inline (one range element per list
        // element), and a string is a single element. This is what lets a
        // template mix literal elements with expansions, e.g.
        // `range: ["first", "{{ RawParam.Paths }}", "last"]`; a range
        // that is entirely one expression can also use the whole-field
        // `<ListExpressionString>` form (§1.3.12) handled below.
        template::StringRange::List(items) => {
            let target = openjd_expr::ExprType::union(vec![
                openjd_expr::ExprType::NULLTYPE,
                openjd_expr::ExprType::STRING,
                openjd_expr::ExprType::list(openjd_expr::ExprType::STRING),
            ]);
            let mut out = Vec::new();
            for fs in items {
                let value = fs
                    .resolve_with(symtab, &budgets.fs_options().with_target_type(&target))
                    .map_err(ModelError::Expression)?;
                match value {
                    openjd_expr::ExprValue::Null => continue,
                    val if val.is_list() => {
                        for elem in val.list_elements().unwrap_or_default() {
                            out.push(elem.to_display_string());
                        }
                    }
                    openjd_expr::ExprValue::String(s) => out.push(s),
                    other => out.push(other.to_display_string()),
                }
            }
            // The ≥1-element rule is checked on field presence at decode,
            // but null-skips can empty the list after resolution.
            rules.check_list_literal_not_emptied(out.len(), param_name)?;
            out
        }
        template::StringRange::Expression(expr) => {
            // Typed evaluation — must yield a list. Propagate the actual error
            // if evaluation fails (e.g., division by zero, undefined variable).
            match expr.resolve_with(symtab, &budgets.fs_options()) {
                Ok(val) if val.is_list() => {
                    let elements = val.list_elements().unwrap();
                    elements.iter().map(|e| e.to_display_string()).collect()
                }
                Ok(val) => {
                    rules.check_not_null(&val, param_name)?;
                    return Err(ModelError::Expression(ExpressionError::new(
                        "String range expression must evaluate to a list",
                    )));
                }
                Err(e) => {
                    return Err(ModelError::Expression(ExpressionError::new(format!(
                        "String range expression: {e}"
                    ))));
                }
            }
        }
    };
    rules.check_len(resolved.len(), param_name, limits)?;
    for (i, s) in resolved.iter().enumerate() {
        let char_count = s.chars().count();
        if char_count > limits.max_task_param_string_len {
            return Err(ModelError::DecodeValidation(format!(
                "Task parameter '{}' range[{}]: resolved value exceeds {} characters ({} chars)",
                param_name, i, limits.max_task_param_string_len, char_count
            )));
        }
        // §3.4.2 sets a minimum length of 1 on <TaskParameterStringValue>,
        // but the reference implementation enforces it only for PATH ("an
        // empty string is not a valid path on any OS") and accepts empty
        // STRING elements end to end. Match that for compatibility; an
        // interpolated PATH element is only known here.
        if is_path && s.is_empty() {
            return Err(ModelError::DecodeValidation(format!(
                "Task parameter '{}' range[{}]: value must not resolve to an empty string",
                param_name, i
            )));
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::strip_redundant_leading_zeros as strip;

    /// §7.5 rule 1. openjd-model-for-python asserts the same cases against its
    /// regex form, `^([+-]?)0+(?=[0-9])`.
    #[test]
    fn strips_only_the_redundant_leading_zeros() {
        // Redundant: another digit follows.
        assert_eq!(strip("02.50"), "2.50");
        assert_eq!(strip("007"), "7");
        assert_eq!(strip("0007"), "7");
        assert_eq!(strip("01E+2"), "1E+2");
        assert_eq!(strip("-02.50"), "-2.50");
        assert_eq!(strip("+02.50"), "+2.50");

        // Not redundant: the zero is the integer part, so one digit stays.
        assert_eq!(strip("0.50"), "0.50");
        assert_eq!(strip("00.50"), "0.50");
        assert_eq!(strip("0"), "0");
        assert_eq!(strip("000"), "0");
        assert_eq!(strip("-0.0"), "-0.0");
        assert_eq!(strip("0e5"), "0e5");
        assert_eq!(strip("00e5"), "0e5");

        // Nothing to do.
        assert_eq!(strip("1.5"), "1.5");
        assert_eq!(strip("100"), "100");
        assert_eq!(strip("3.500"), "3.500");

        // §7.5 rule 2 is not this function's job: it never touches the fraction.
        assert_eq!(strip("2.50"), "2.50");
        assert_eq!(strip("0.0000001"), "0.0000001");
    }

    /// Not just an optimization: evidence that an unchanged element is returned
    /// untouched rather than rebuilt.
    #[test]
    fn borrows_when_there_is_nothing_to_strip() {
        assert!(matches!(strip("2.50"), Cow::Borrowed(_)));
        assert!(matches!(strip("0.50"), Cow::Borrowed(_)));
        assert!(matches!(strip("02.50"), Cow::Owned(_)));
    }
}
