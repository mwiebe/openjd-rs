// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Comparison, containment, and slice operator implementations.

use crate::error::ExpressionError;
use crate::function_library::EvalContext;
use crate::value::ExprValue;

type R = Result<ExprValue, ExpressionError>;
type Ctx<'a> = &'a mut dyn EvalContext;

// ── Equality ──

/// Language-level value equality with operation accounting, mirroring
/// the Python reference's `_values_equal`:
///
/// - list ↔ list and list ↔ range_expr comparisons charge
///   `max(len_a, len_b)` operations *before* the length check (so a
///   comparison against a huge symbolic range trips the operation
///   limit exactly as it does in Python, where the range would be
///   materialized), and recurse element-wise with the same accounting.
/// - range_expr ↔ range_expr compares symbolically (chunk equality),
///   no per-element charge.
/// - Everything else delegates to `ExprValue::equals`.
///
/// This is deliberately distinct from `ExprValue::equals` /
/// `PartialEq`, which exclude the list ↔ range_expr cross-type rule so
/// that the `a == b ⇒ hash(a) == hash(b)` contract holds (lists and
/// ranges hash under different tags).
pub(crate) fn values_equal(
    ctx: Ctx,
    a: &ExprValue,
    b: &ExprValue,
) -> Result<bool, ExpressionError> {
    match (a, b) {
        (ExprValue::RangeExpr(x), ExprValue::RangeExpr(y)) => Ok(x == y),
        (ExprValue::RangeExpr(r), other) | (other, ExprValue::RangeExpr(r)) if other.is_list() => {
            let list_len = other.list_len().unwrap_or(0);
            ctx.count_ops(list_len.max(r.len()))?;
            if list_len != r.len() {
                return Ok(false);
            }
            let mut iter = other.list_iter().expect("is_list() was true");
            for rv in r.iter() {
                let lv = iter.next().expect("lengths match");
                if !lv.equals(&ExprValue::Int(rv)) {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ if a.is_list() && b.is_list() => {
            let (Some(a_iter), Some(b_iter)) = (a.list_iter(), b.list_iter()) else {
                return Ok(false);
            };
            let (a_len, b_len) = (a_iter.len(), b_iter.len());
            ctx.count_ops(a_len.max(b_len))?;
            if a_len != b_len {
                return Ok(false);
            }
            for (x, y) in a_iter.zip(b_iter) {
                if !values_equal(ctx, &x, &y)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(a.equals(b)),
    }
}

pub fn eq_generic(ctx: Ctx, a: &[ExprValue]) -> R {
    Ok(ExprValue::Bool(values_equal(ctx, &a[0], &a[1])?))
}

pub fn ne_generic(ctx: Ctx, a: &[ExprValue]) -> R {
    Ok(ExprValue::Bool(!values_equal(ctx, &a[0], &a[1])?))
}

// ── Ordering ──

/// Ordering with operation accounting: list ↔ list comparisons charge
/// `max(len_a, len_b)` and recurse (mirroring the Python reference's
/// `_value_compare`); scalars delegate to `ExprValue::compare`.
fn compare_counted(
    ctx: Ctx,
    a: &ExprValue,
    b: &ExprValue,
) -> Result<std::cmp::Ordering, ExpressionError> {
    if a.is_list() && b.is_list() {
        let (Some(a_iter), Some(b_iter)) = (a.list_iter(), b.list_iter()) else {
            return Err(ExpressionError::new(format!(
                "Cannot compare {} and {}",
                a.expr_type(),
                b.expr_type()
            )));
        };
        let (a_len, b_len) = (a_iter.len(), b_iter.len());
        ctx.count_ops(a_len.max(b_len))?;
        for (x, y) in a_iter.zip(b_iter) {
            match compare_counted(ctx, &x, &y) {
                Ok(std::cmp::Ordering::Equal) => continue,
                other => return other,
            }
        }
        return Ok(a_len.cmp(&b_len));
    }
    a.compare(b)
}

fn do_compare(
    ctx: Ctx,
    op_str: &str,
    a: &[ExprValue],
) -> Result<std::cmp::Ordering, ExpressionError> {
    compare_counted(ctx, &a[0], &a[1]).map_err(|e| {
        // Preserve resource-limit errors; only translate type errors
        // into the operator-specific wording.
        if matches!(
            e.kind(),
            crate::error::ExpressionErrorKind::OperationLimitExceeded { .. }
                | crate::error::ExpressionErrorKind::MemoryLimitExceeded { .. }
        ) {
            e
        } else {
            ExpressionError::type_error(format!(
                "Cannot use '{}' operator with {} and {}",
                op_str,
                a[0].expr_type(),
                a[1].expr_type()
            ))
        }
    })
}

pub fn lt_generic(ctx: Ctx, a: &[ExprValue]) -> R {
    Ok(ExprValue::Bool(do_compare(ctx, "<", a)?.is_lt()))
}

pub fn le_generic(ctx: Ctx, a: &[ExprValue]) -> R {
    Ok(ExprValue::Bool(do_compare(ctx, "<=", a)?.is_le()))
}

pub fn gt_generic(ctx: Ctx, a: &[ExprValue]) -> R {
    Ok(ExprValue::Bool(do_compare(ctx, ">", a)?.is_gt()))
}

pub fn ge_generic(ctx: Ctx, a: &[ExprValue]) -> R {
    Ok(ExprValue::Bool(do_compare(ctx, ">=", a)?.is_ge()))
}

// ── Containment ──

pub fn contains_list(ctx: Ctx, a: &[ExprValue]) -> R {
    let len = a[0].list_len().unwrap_or(0);
    ctx.count_ops(len)?;
    let found = a[0]
        .list_iter()
        .map(|mut iter| iter.any(|e| a[1].equals(&e)))
        .unwrap_or(false);
    Ok(ExprValue::Bool(found))
}

pub fn not_contains_list(ctx: Ctx, a: &[ExprValue]) -> R {
    let len = a[0].list_len().unwrap_or(0);
    ctx.count_ops(len)?;
    let found = a[0]
        .list_iter()
        .map(|mut iter| iter.any(|e| a[1].equals(&e)))
        .unwrap_or(false);
    Ok(ExprValue::Bool(!found))
}

pub fn contains_string(ctx: Ctx, a: &[ExprValue]) -> R {
    match (&a[0], &a[1]) {
        (ExprValue::String(haystack), ExprValue::String(needle)) => {
            ctx.count_string_ops(haystack.len() + needle.len())?;
            Ok(ExprValue::Bool(haystack.contains(needle.as_str())))
        }
        _ => Err(ExpressionError::type_error("type error")),
    }
}

pub fn not_contains_string(ctx: Ctx, a: &[ExprValue]) -> R {
    match (&a[0], &a[1]) {
        (ExprValue::String(haystack), ExprValue::String(needle)) => {
            ctx.count_string_ops(haystack.len() + needle.len())?;
            Ok(ExprValue::Bool(!haystack.contains(needle.as_str())))
        }
        _ => Err(ExpressionError::type_error("type error")),
    }
}

pub fn contains_range(_: Ctx, a: &[ExprValue]) -> R {
    match (&a[0], &a[1]) {
        (ExprValue::RangeExpr(r), ExprValue::Int(i)) => Ok(ExprValue::Bool(r.contains(*i))),
        _ => Err(ExpressionError::type_error("type error")),
    }
}

pub fn not_contains_range(_: Ctx, a: &[ExprValue]) -> R {
    match (&a[0], &a[1]) {
        (ExprValue::RangeExpr(r), ExprValue::Int(i)) => Ok(ExprValue::Bool(!r.contains(*i))),
        _ => Err(ExpressionError::type_error("type error")),
    }
}

// ── Slicing (4-arg __getitem__) ──

fn extract_int_or_none(v: &ExprValue) -> Option<i64> {
    match v {
        ExprValue::Int(i) => Some(*i),
        _ => None, // Null → None
    }
}

fn compute_slice_indices(len: i64, start: Option<i64>, stop: Option<i64>, step: i64) -> (i64, i64) {
    if step > 0 {
        let s = start
            .map(|i| if i < 0 { (len + i).max(0) } else { i.min(len) })
            .unwrap_or(0);
        let e = stop
            .map(|i| if i < 0 { (len + i).max(0) } else { i.min(len) })
            .unwrap_or(len);
        (s, e)
    } else {
        let s = start
            .map(|i| if i < 0 { len + i } else { i.min(len - 1) })
            .unwrap_or(len - 1);
        let e = stop.map(|i| if i < 0 { len + i } else { i }).unwrap_or(-1);
        (s, e)
    }
}

fn collect_indices(start: i64, stop: i64, step: i64) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut idx = start;
    if step > 0 {
        while idx < stop {
            if idx >= 0 {
                indices.push(idx as usize);
            }
            idx += step;
        }
    } else {
        while idx > stop {
            if idx >= 0 {
                indices.push(idx as usize);
            }
            idx += step;
        }
    }
    indices
}

pub fn slice_list(ctx: Ctx, a: &[ExprValue]) -> R {
    let step = extract_int_or_none(&a[3]).unwrap_or(1);
    if step == 0 {
        return Err(ExpressionError::new("Slice step cannot be zero"));
    }
    let elem_type = a[0].list_elem_type().unwrap();
    let len = a[0].list_len().unwrap() as i64;
    let start = extract_int_or_none(&a[1]);
    let stop = extract_int_or_none(&a[2]);
    let (s, e) = compute_slice_indices(len, start, stop, step);
    let result: Vec<ExprValue> = collect_indices(s, e, step)
        .into_iter()
        .filter_map(|i| a[0].list_get(i as i64))
        .collect();
    ctx.count_ops(result.len())?;
    ExprValue::make_list_checked(ctx, result, elem_type.clone())
}

pub fn slice_string(ctx: Ctx, a: &[ExprValue]) -> R {
    let s = match &a[0] {
        ExprValue::String(s) => s.as_str(),
        _ => return Err(ExpressionError::type_error("type error")),
    };
    ctx.count_string_ops(s.len())?;
    let step = extract_int_or_none(&a[3]).unwrap_or(1);
    if step == 0 {
        return Err(ExpressionError::new("Slice step cannot be zero"));
    }
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as i64;
    let start = extract_int_or_none(&a[1]);
    let stop = extract_int_or_none(&a[2]);
    let (sv, ev) = compute_slice_indices(len, start, stop, step);
    let result: String = collect_indices(sv, ev, step)
        .into_iter()
        .filter(|&i| i < chars.len())
        .map(|i| chars[i])
        .collect();
    Ok(ExprValue::String(result))
}

pub fn slice_range(ctx: Ctx, a: &[ExprValue]) -> R {
    let r = match &a[0] {
        ExprValue::RangeExpr(r) => r,
        _ => return Err(ExpressionError::type_error("type error")),
    };
    let step = extract_int_or_none(&a[3]).unwrap_or(1);
    if step == 0 {
        return Err(ExpressionError::new("Slice step cannot be zero"));
    }
    let len = r.len() as i64;
    let start = extract_int_or_none(&a[1]);
    let stop = extract_int_or_none(&a[2]);
    let (s, e) = compute_slice_indices(len, start, stop, step);
    if step > 0 {
        // Forward slice → return RangeExpr
        Ok(ExprValue::RangeExpr(r.slice(s, e, step)?))
    } else {
        // Reverse slice → return list (RangeExpr can't represent descending order)
        let result: Vec<ExprValue> = collect_indices(s, e, step)
            .into_iter()
            .filter_map(|i| r.get(i as i64).map(ExprValue::Int))
            .collect();
        ctx.count_ops(result.len())?;
        Ok(ExprValue::make_list_checked(
            ctx,
            result,
            crate::types::ExprType::INT,
        )?)
    }
}
