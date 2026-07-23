// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Repr function implementations (repr_py, repr_json, repr_sh, repr_cmd, repr_pwsh).

use crate::error::ExpressionError;
use crate::function_library::EvalContext;
use crate::value::ExprValue;

type R = Result<ExprValue, ExpressionError>;
type Ctx<'a> = &'a mut dyn EvalContext;

#[derive(Clone, Copy)]
enum ReprStyle {
    Py,
    Json,
    Sh,
    Cmd,
    Pwsh,
}

// ─── Phase 1: count list items (O(1) per list, no string scanning) ───

/// Count the total number of list items recursively. This is O(1) per list
/// node (reads `.len()` from the Vec), so it completes quickly even for large
/// nested structures. The result is charged to `count_ops` *before* any
/// character-scanning work begins.
fn count_list_items(value: &ExprValue) -> usize {
    match value {
        ExprValue::ListBool(v) => v.len(),
        ExprValue::ListInt(v) => v.len(),
        ExprValue::ListFloat(v) => v.len(),
        ExprValue::ListString(v, _) | ExprValue::ListPath(v, _, _) => v.len(),
        ExprValue::ListList(v, _, _) => v
            .len()
            .saturating_add(v.iter().map(count_list_items).sum::<usize>()),
        _ => 0,
    }
}

// ─── Phase 2: compute output size (string scanning, bounded by phase 1) ───

fn escaped_string_len(s: &str, style: ReprStyle) -> usize {
    let body = match style {
        ReprStyle::Py => s.bytes().fold(0usize, |n, b| {
            n.saturating_add(if matches!(b, b'\\' | b'\'') { 2 } else { 1 })
        }),
        ReprStyle::Json => s.chars().fold(0usize, |n, c| {
            let encoded = match c {
                '"' | '\\' | '\x08' | '\x0c' | '\n' | '\r' | '\t' => 2,
                c if (c as u32) < 0x20 => 6,
                c if c.is_ascii() => 1,
                c => c.len_utf16() * 6,
            };
            n.saturating_add(encoded)
        }),
        // shlex single-quote escaping replaces each quote with '\"'\"'",
        // adding four bytes. Unsafe strings also gain surrounding quotes.
        ReprStyle::Sh => s
            .len()
            .saturating_add(s.bytes().filter(|b| *b == b'\'').count().saturating_mul(4)),
        ReprStyle::Cmd => s.chars().fold(0usize, |n, c| {
            let encoded = match c {
                '\n' | '\r' => 0,
                '^' | '"' | '%' => 2,
                '!' => 3,
                _ => c.len_utf8(),
            };
            n.saturating_add(encoded)
        }),
        ReprStyle::Pwsh => s
            .len()
            .saturating_add(s.bytes().filter(|b| *b == b'\'').count()),
    };
    body.saturating_add(match style {
        ReprStyle::Sh => 2, // conservative: shlex omits quotes for safe strings
        _ => 2,
    })
}

fn decimal_len(value: i64) -> usize {
    if value == 0 {
        1
    } else {
        let sign = usize::from(value < 0);
        sign + value.unsigned_abs().ilog10() as usize + 1
    }
}

// Per-element bound helpers that account for style-specific quoting.

fn scalar_item_bound(b: bool, style: ReprStyle) -> usize {
    let base: usize = match (style, b) {
        (ReprStyle::Py, true) => 4,
        (ReprStyle::Py, false) => 5,
        (ReprStyle::Pwsh, true) => 5,
        (ReprStyle::Pwsh, false) => 6,
        (_, true) => 4,
        (_, false) => 5,
    };
    match style {
        // repr_sh shell-quotes each element's display string
        ReprStyle::Sh => base.saturating_add(2),
        _ => base,
    }
}

fn int_item_bound(v: i64, _style: ReprStyle) -> usize {
    decimal_len(v)
}

fn float_item_bound(v: &crate::value::Float64, style: ReprStyle) -> usize {
    let base = v.display_len().saturating_add(2);
    match style {
        // Floats with '.' might get quoted by shlex (the '.' is fine, but
        // a leading '-' or '+' might trigger quoting in some shlex impls).
        // Conservatively add 2 for possible quotes.
        ReprStyle::Sh => base.saturating_add(2),
        _ => base,
    }
}

/// For nested lists, repr_sh takes each element's `to_display_string()` and
/// shell-quotes it. The display string of an inner list includes brackets,
/// commas, and quoted strings — all of which shlex will re-quote. We
/// pessimistically estimate: the inner display length × 5 (worst case for
/// shlex: every char is a single-quote needing `'"'"'`).
fn nested_list_item_bound(v: &ExprValue, style: ReprStyle) -> usize {
    match style {
        ReprStyle::Sh => {
            // The inner element gets to_display_string() called, then shlex
            // quotes the result. Upper bound on shlex quoting: add 2 for
            // surrounding quotes + 4 per single-quote in content.
            // The display string length is bounded by the non-sh output_bound
            // (which uses Py-like rendering as a conservative proxy).
            let inner_display = output_bound(v, ReprStyle::Py);
            // shlex worst case: each `'` in the display becomes 4 extra
            // bytes. Conservatively assume up to 1/4 of chars are quotes.
            inner_display.saturating_mul(2).saturating_add(2)
        }
        _ => output_bound(v, style),
    }
}

/// Compute the upper bound on output bytes by scanning strings. Called only
/// after `count_ops` has passed (phase 1), so the expensive character scan
/// is budget-bounded.
fn output_bound(value: &ExprValue, style: ReprStyle) -> usize {
    match value {
        ExprValue::String(s) | ExprValue::Path { value: s, .. } => escaped_string_len(s, style),
        ExprValue::ListBool(values) => list_output_bound(
            values.iter().map(|b| scalar_item_bound(*b, style)),
            values.len(),
            style,
        ),
        ExprValue::ListInt(values) => list_output_bound(
            values.iter().map(|v| int_item_bound(*v, style)),
            values.len(),
            style,
        ),
        ExprValue::ListFloat(values) => list_output_bound(
            values.iter().map(|v| float_item_bound(v, style)),
            values.len(),
            style,
        ),
        ExprValue::ListString(values, _) | ExprValue::ListPath(values, _, _) => list_output_bound(
            values.iter().map(|v| escaped_string_len(v, style)),
            values.len(),
            style,
        ),
        ExprValue::ListList(values, _, _) => list_output_bound(
            values.iter().map(|v| nested_list_item_bound(v, style)),
            values.len(),
            style,
        ),
        ExprValue::Null => match style {
            ReprStyle::Py => 4,
            ReprStyle::Json => 4,
            ReprStyle::Sh | ReprStyle::Cmd => 2,
            ReprStyle::Pwsh => 5,
        },
        ExprValue::Bool(value) => match (style, value) {
            (ReprStyle::Py, true) => 4,
            (ReprStyle::Py, false) => 5,
            (ReprStyle::Pwsh, true) => 5,
            (ReprStyle::Pwsh, false) => 6,
            (_, true) => 4,
            (_, false) => 5,
        },
        ExprValue::Int(value) => decimal_len(*value),
        ExprValue::Float(value) => value.display_len().saturating_add(2),
        ExprValue::RangeExpr(range) => range.ranges().len().saturating_mul(64).saturating_add(2),
        ExprValue::Unresolved(_) => 64,
    }
}

fn list_output_bound(
    item_sizes: impl Iterator<Item = usize>,
    len: usize,
    style: ReprStyle,
) -> usize {
    let wrapper: usize = match style {
        ReprStyle::Pwsh => 3, // @( + )
        _ => 2,               // [ + ] or bare
    };
    let sep: usize = match style {
        ReprStyle::Sh | ReprStyle::Cmd => 1, // space
        _ => 2,                              // ", "
    };
    let mut total = wrapper.saturating_add(len.saturating_sub(1).saturating_mul(sep));
    for sz in item_sizes {
        total = total.saturating_add(sz);
    }
    total
}

// ─── Preflight: charge budgets in order ───

/// Two-phase preflight:
/// 1. Count list items (O(1) per list node) and charge `count_ops` — this
///    rejects inputs with too many elements before any string scanning.
/// 2. Scan strings to compute output bound, charge `count_string_ops` and
///    `check_memory`.
fn preflight_repr(ctx: Ctx, value: &ExprValue, style: ReprStyle) -> Result<usize, ExpressionError> {
    let items = count_list_items(value);
    ctx.count_ops(items)?;
    let bound = output_bound(value, style);
    ctx.count_string_ops(bound)?;
    ctx.check_memory(bound)?;
    Ok(bound)
}

// ─── Public entry points ───

pub fn repr_py_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let cap = preflight_repr(ctx, &a[0], ReprStyle::Py)?;
    let mut buf = String::with_capacity(cap);
    write_repr_py(&a[0], &mut buf);
    Ok(ExprValue::String(buf))
}

pub fn repr_json_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let cap = preflight_repr(ctx, &a[0], ReprStyle::Json)?;
    let mut buf = String::with_capacity(cap);
    write_repr_json(&a[0], &mut buf);
    Ok(ExprValue::String(buf))
}

pub fn repr_sh_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    preflight_repr(ctx, &a[0], ReprStyle::Sh)?;
    repr_sh(&a[0]).map(ExprValue::String)
}

pub fn repr_cmd_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let cap = preflight_repr(ctx, &a[0], ReprStyle::Cmd)?;
    let mut buf = String::with_capacity(cap);
    write_repr_cmd(&a[0], &mut buf);
    Ok(ExprValue::String(buf))
}

pub fn repr_pwsh_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let cap = preflight_repr(ctx, &a[0], ReprStyle::Pwsh)?;
    let mut buf = String::with_capacity(cap);
    write_repr_pwsh(&a[0], &mut buf);
    Ok(ExprValue::String(buf))
}

// ─── Renderers: write directly into a single buffer ───

fn write_repr_py(val: &ExprValue, buf: &mut String) {
    match val {
        ExprValue::String(s) | ExprValue::Path { value: s, .. } => {
            buf.push('\'');
            for c in s.chars() {
                match c {
                    '\\' => buf.push_str("\\\\"),
                    '\'' => buf.push_str("\\'"),
                    _ => buf.push(c),
                }
            }
            buf.push('\'');
        }
        ExprValue::Bool(b) => buf.push_str(if *b { "True" } else { "False" }),
        ExprValue::Null => buf.push_str("None"),
        ExprValue::Int(i) => {
            use std::fmt::Write;
            let _ = write!(buf, "{i}");
        }
        ExprValue::Float(f) => {
            if f.value().fract() == 0.0 {
                use std::fmt::Write;
                let _ = write!(buf, "{f:.1}");
            } else {
                buf.push_str(&f.to_display_string());
            }
        }
        val if val.is_list() => {
            buf.push('[');
            let iter = val.list_iter().expect("is_list() was true");
            let mut first = true;
            for e in iter {
                if !first {
                    buf.push_str(", ");
                }
                first = false;
                write_repr_py(&e, buf);
            }
            buf.push(']');
        }
        ExprValue::RangeExpr(r) => {
            use std::fmt::Write;
            let _ = write!(buf, "'{r}'");
        }
        _ => buf.push_str(&val.to_display_string()),
    }
}

fn write_repr_json(val: &ExprValue, buf: &mut String) {
    use std::fmt::Write;
    match val {
        ExprValue::String(s) | ExprValue::Path { value: s, .. } => {
            buf.push('"');
            write_json_escape(s, buf);
            buf.push('"');
        }
        ExprValue::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
        ExprValue::Null => buf.push_str("null"),
        ExprValue::Int(i) => {
            let _ = write!(buf, "{i}");
        }
        ExprValue::Float(f) => {
            if f.value().fract() == 0.0 {
                let _ = write!(buf, "{f:.1}");
            } else {
                buf.push_str(&f.to_display_string());
            }
        }
        val if val.is_list() => {
            buf.push('[');
            let iter = val.list_iter().expect("guard ensures list");
            let mut first = true;
            for e in iter {
                if !first {
                    buf.push_str(", ");
                }
                first = false;
                write_repr_json(&e, buf);
            }
            buf.push(']');
        }
        ExprValue::RangeExpr(r) => {
            let _ = write!(buf, "\"{r}\"");
        }
        _ => buf.push_str(&val.to_display_string()),
    }
}

/// Escape a string for JSON output, matching Python's `json.dumps(ensure_ascii=True)`.
/// All non-ASCII characters are encoded as `\uXXXX` (with surrogate pairs for chars > U+FFFF).
fn write_json_escape(s: &str, buf: &mut String) {
    use std::fmt::Write;
    for c in s.chars() {
        match c {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\x08' => buf.push_str("\\b"),
            '\x0c' => buf.push_str("\\f"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c if c.is_ascii() => buf.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(buf, "\\u{:04x}", unit);
                }
            }
        }
    }
}

fn repr_sh(val: &ExprValue) -> Result<String, ExpressionError> {
    match val {
        ExprValue::String(s) => shlex_quote(s),
        ExprValue::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
        ExprValue::Null => Ok("''".to_string()),
        ExprValue::Int(i) => Ok(i.to_string()),
        ExprValue::Float(f) => Ok(if f.value().fract() == 0.0 {
            format!("{:.1}", f)
        } else {
            f.to_string()
        }),
        ExprValue::Path { value, .. } => shlex_quote(value),
        val if val.is_list() => {
            let strs: Vec<String> = val
                .list_iter()
                .expect("guard ensures list")
                .map(|e| match &e {
                    ExprValue::String(s) | ExprValue::Path { value: s, .. } => s.clone(),
                    other => other.to_display_string(),
                })
                .collect();
            shlex::try_join(strs.iter().map(|s| s.as_str()))
                .map_err(|e| ExpressionError::new(format!("Cannot shell-quote list: {e}")))
        }
        _ => Ok(val.to_display_string()),
    }
}

/// Shell-quote a single string, returning an error on null bytes.
fn shlex_quote(s: &str) -> Result<String, ExpressionError> {
    shlex::try_quote(s)
        .map(|c| c.into_owned())
        .map_err(|e| ExpressionError::new(format!("Cannot shell-quote string: {e}")))
}

fn write_repr_cmd(val: &ExprValue, buf: &mut String) {
    match val {
        ExprValue::String(s) | ExprValue::Path { value: s, .. } => cmd_quote_into(s, buf),
        ExprValue::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
        ExprValue::Null => buf.push_str("\"\""),
        ExprValue::Int(i) => {
            use std::fmt::Write;
            let _ = write!(buf, "{i}");
        }
        ExprValue::Float(f) => {
            if f.value().fract() == 0.0 {
                use std::fmt::Write;
                let _ = write!(buf, "{f:.1}");
            } else {
                buf.push_str(&f.to_display_string());
            }
        }
        val if val.is_list() => {
            let iter = val.list_iter().expect("guard ensures list");
            let mut first = true;
            for e in iter {
                if !first {
                    buf.push(' ');
                }
                first = false;
                write_repr_cmd(&e, buf);
            }
        }
        _ => buf.push_str(&val.to_display_string()),
    }
}

fn write_repr_pwsh(val: &ExprValue, buf: &mut String) {
    match val {
        ExprValue::String(s) | ExprValue::Path { value: s, .. } => {
            buf.push('\'');
            for c in s.chars() {
                if c == '\'' {
                    buf.push_str("''");
                } else {
                    buf.push(c);
                }
            }
            buf.push('\'');
        }
        ExprValue::Bool(b) => buf.push_str(if *b { "$true" } else { "$false" }),
        ExprValue::Null => buf.push_str("$null"),
        ExprValue::Int(i) => {
            use std::fmt::Write;
            let _ = write!(buf, "{i}");
        }
        ExprValue::Float(f) => {
            if f.value().fract() == 0.0 {
                use std::fmt::Write;
                let _ = write!(buf, "{f:.1}");
            } else {
                buf.push_str(&f.to_display_string());
            }
        }
        val if val.is_list() => {
            buf.push_str("@(");
            let iter = val.list_iter().expect("guard ensures list");
            let mut first = true;
            for e in iter {
                if !first {
                    buf.push_str(", ");
                }
                first = false;
                write_repr_pwsh(&e, buf);
            }
            buf.push(')');
        }
        ExprValue::RangeExpr(r) => {
            use std::fmt::Write;
            let _ = write!(buf, "'{r}'");
        }
        _ => buf.push_str(&val.to_display_string()),
    }
}

/// Quote a string for cmd.exe, writing directly into the buffer.
fn cmd_quote_into(s: &str, buf: &mut String) {
    const NEEDS_QUOTING: &str = " \t&|<>^\"()%!";
    // Strip newlines first (cmd.exe has no escape for literal newlines),
    // then decide quoting based on the stripped content.
    let has_newlines = s.chars().any(|c| c == '\n' || c == '\r');
    let has_special = s
        .chars()
        .any(|c| c != '\n' && c != '\r' && NEEDS_QUOTING.contains(c));
    let stripped_empty = !s.chars().any(|c| c != '\n' && c != '\r');
    // An empty result (original was empty or all-newlines) gets quoted.
    if (s.is_empty() || stripped_empty) || has_special {
        buf.push('"');
        for c in s.chars() {
            match c {
                '\n' | '\r' => {}
                '^' | '"' => {
                    buf.push('^');
                    buf.push(c);
                }
                '%' => buf.push_str("%%"),
                '!' => buf.push_str("^^!"),
                c => buf.push(c),
            }
        }
        buf.push('"');
    } else if has_newlines {
        // Has non-special content plus newlines — strip newlines, no quoting.
        for c in s.chars() {
            if c != '\n' && c != '\r' {
                buf.push(c);
            }
        }
    } else {
        // No newlines, no special chars — pass through.
        buf.push_str(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ExprType;

    /// Helper that computes output_bound (the value charged to check_memory)
    /// and verifies it covers the actual rendered output.
    fn check_bound(value: &ExprValue) {
        let styles = [
            (ReprStyle::Py, "py"),
            (ReprStyle::Json, "json"),
            (ReprStyle::Cmd, "cmd"),
            (ReprStyle::Pwsh, "pwsh"),
        ];
        for (style, name) in styles {
            let bound = output_bound(value, style);
            let mut buf = String::new();
            match style {
                ReprStyle::Py => write_repr_py(value, &mut buf),
                ReprStyle::Json => write_repr_json(value, &mut buf),
                ReprStyle::Cmd => write_repr_cmd(value, &mut buf),
                ReprStyle::Pwsh => write_repr_pwsh(value, &mut buf),
                ReprStyle::Sh => {}
            }
            assert!(
                bound >= buf.len(),
                "repr_{name}: bound {bound} < rendered {} for {buf:?}",
                buf.len()
            );
        }
        // sh is fallible, check separately
        let sh_bound = output_bound(value, ReprStyle::Sh);
        if let Ok(sh_out) = repr_sh(value) {
            assert!(
                sh_bound >= sh_out.len(),
                "repr_sh: bound {sh_bound} < rendered {} for {sh_out:?}",
                sh_out.len()
            );
        }
    }

    #[test]
    fn preflight_output_bounds_cover_rendered_representations() {
        let inner = ExprValue::make_list(
            vec![
                ExprValue::String("plain".into()),
                ExprValue::String("quote'\\\n\u{1f600}".into()),
            ],
            ExprType::STRING,
        )
        .unwrap();
        let value = ExprValue::make_list(
            vec![inner, ExprValue::String("!%^\"".into())],
            ExprType::list(ExprType::STRING),
        )
        .unwrap();
        check_bound(&value);
    }

    #[test]
    fn scalar_bounds() {
        check_bound(&ExprValue::Int(0));
        check_bound(&ExprValue::Int(-1234567890));
        check_bound(&ExprValue::Bool(true));
        check_bound(&ExprValue::Null);
    }
}
