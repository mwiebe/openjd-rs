// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Host and port string functions (`join_host_port`, `split_host_port`,
//! `is_ipv4`, `is_ipv6`).
//!
//! These follow Go's `net.JoinHostPort` and `net.SplitHostPort`, with two
//! deliberate differences required by the Expression Language specification
//! (§2.2.4, added by the `SERVICE` extension): a bare IPv6 literal such as
//! `2001:db8::5` splits to `null` rather than an error, and an
//! already-bracketed host passed to `join_host_port` is not bracketed again.

use super::StringOutputBudget;
use crate::error::ExpressionError;
use crate::function_library::EvalContext;
use crate::types::ExprType;
use crate::value::ExprValue;
use std::net::{Ipv4Addr, Ipv6Addr};

type R = Result<ExprValue, ExpressionError>;
type Ctx<'a> = &'a mut dyn EvalContext;

fn get_str<'a>(a: &'a ExprValue, name: &str) -> Result<&'a str, ExpressionError> {
    match a {
        ExprValue::String(s) => Ok(s),
        _ => Err(ExpressionError::new(format!(
            "{name}() requires a string argument, got {}",
            a.expr_type()
        ))),
    }
}

/// True when `host` is already enclosed in a matching pair of square
/// brackets (`[...]`).
fn is_bracketed(host: &str) -> bool {
    host.len() >= 2 && host.starts_with('[') && host.ends_with(']')
}

/// Strip one enclosing pair of square brackets from an IPv6 literal, if
/// present. Returns the input unchanged otherwise.
fn strip_brackets(s: &str) -> &str {
    if is_bracketed(s) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// True when `s` parses as an IPv6 address once an optional `%zone`
/// suffix is removed. The zone identifier, when present, must be
/// non-empty; its contents are not otherwise validated.
fn is_ipv6_literal(s: &str) -> bool {
    let addr = match s.split_once('%') {
        Some((addr, zone)) => {
            if zone.is_empty() {
                return false;
            }
            addr
        }
        None => s,
    };
    addr.parse::<Ipv6Addr>().is_ok()
}

/// `join_host_port(host: string, port: int) -> string`
///
/// Join `host` and `port` as `host:port`. When `host` contains a colon
/// (an IPv6 literal, with or without a zone identifier) and is not
/// already enclosed in square brackets, it is bracketed: `[host]:port`.
/// An already-bracketed host is emitted as-is.
pub fn join_host_port_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let host = get_str(&a[0], "join_host_port")?;
    let port = match &a[1] {
        ExprValue::Int(p) => *p,
        other => {
            return Err(ExpressionError::new(format!(
                "join_host_port() port must be int, got {}",
                other.expr_type()
            )))
        }
    };
    let needs_brackets = host.contains(':') && !is_bracketed(host);
    // Output is at most host + 2 brackets + ':' + a 20-byte i64.
    let output_bytes = host.len().saturating_add(2 + 1 + 20);
    let budget = StringOutputBudget::reserve(ctx, host.len(), output_bytes)?;
    let out = if needs_brackets {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Ok(budget.finish(out))
}

/// `split_host_port(s: string) -> list[string]?`
///
/// Split `host:port` or `[host]:port` into `[host, port]`, removing the
/// square brackets from an IPv6 literal. Returns `null` when `s` has no
/// port: no colon at all, a bracketed host with nothing after the `]`,
/// or an unbracketed IPv6 literal (more than one colon, no brackets).
/// Malformed brackets — a `[` without its `]`, characters between `]`
/// and the port's `:`, or stray brackets anywhere else — are an error.
/// The port element is returned as a string without validation.
pub fn split_host_port_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let s = get_str(&a[0], "split_host_port")?;
    ctx.count_string_ops(s.len())?;
    let fail = |why: &str| -> R {
        Err(ExpressionError::new(format!(
            "split_host_port failed: {why} in '{s}'"
        )))
    };

    // The port starts after the last colon.
    let Some(last_colon) = s.rfind(':') else {
        // No colon at all: no port. Stray brackets are still malformed.
        if s.contains('[') || s.contains(']') {
            return fail("unexpected '[' or ']'");
        }
        return Ok(ExprValue::Null);
    };

    let (host, after_host) = if let Some(rest) = s.strip_prefix('[') {
        // Expect the first ']' immediately before the last ':'.
        let Some(end) = rest.find(']') else {
            return fail("missing ']'");
        };
        let end = end + 1; // index of ']' in `s`
        let host = &s[1..end];
        if end + 1 == s.len() {
            // `[host]` with nothing after the bracket: no port.
            if host.contains('[') {
                return fail("unexpected '[' or ']'");
            }
            return Ok(ExprValue::Null);
        }
        if end + 1 != last_colon {
            if s.as_bytes()[end + 1] == b':' {
                return fail("too many colons");
            }
            return fail("unexpected characters after ']'");
        }
        (host, &s[end + 1..])
    } else {
        let host = &s[..last_colon];
        if host.contains(':') {
            // Unbracketed IPv6 literal: read as having no port.
            if s.contains('[') || s.contains(']') {
                return fail("unexpected '[' or ']'");
            }
            return Ok(ExprValue::Null);
        }
        (host, &s[last_colon..])
    };

    // No further brackets may appear anywhere in the host or the port.
    if [host, after_host]
        .iter()
        .any(|part| part.contains('[') || part.contains(']'))
    {
        return fail("unexpected '[' or ']'");
    }

    let port = &after_host[1..];
    let parts = vec![
        ExprValue::String(host.to_string()),
        ExprValue::String(port.to_string()),
    ];
    ExprValue::make_list_checked(ctx, parts, ExprType::STRING)
}

/// `is_ipv4(s: string) -> bool`
///
/// True when `s` is a dotted-quad IPv4 literal such as `10.0.0.1`.
pub fn is_ipv4_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let s = get_str(&a[0], "is_ipv4")?;
    ctx.count_string_ops(s.len())?;
    Ok(ExprValue::Bool(s.parse::<Ipv4Addr>().is_ok()))
}

/// `is_ipv6(s: string) -> bool`
///
/// True when `s` is an IPv6 literal, bracketed (`[::1]`) or not, with or
/// without a zone identifier (`fe80::1%eth0`).
pub fn is_ipv6_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let s = get_str(&a[0], "is_ipv6")?;
    ctx.count_string_ops(s.len())?;
    Ok(ExprValue::Bool(is_ipv6_literal(strip_brackets(s))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_detection() {
        assert!(is_bracketed("[::1]"));
        assert!(is_bracketed("[]"));
        assert!(!is_bracketed("["));
        assert!(!is_bracketed("]"));
        assert!(!is_bracketed("[::1"));
        assert!(!is_bracketed("::1]"));
        assert!(!is_bracketed("::1"));
    }

    #[test]
    fn strip_brackets_only_when_matched() {
        assert_eq!(strip_brackets("[::1]"), "::1");
        assert_eq!(strip_brackets("[::1"), "[::1");
        assert_eq!(strip_brackets("::1"), "::1");
        assert_eq!(strip_brackets(""), "");
    }

    #[test]
    fn ipv6_literal_with_and_without_zone() {
        assert!(is_ipv6_literal("::1"));
        assert!(is_ipv6_literal("2001:db8::5"));
        assert!(is_ipv6_literal("fe80::1%eth0"));
        assert!(is_ipv6_literal("::ffff:10.0.0.1"));
        assert!(!is_ipv6_literal("fe80::1%"));
        assert!(!is_ipv6_literal("10.0.0.1"));
        assert!(!is_ipv6_literal("cache.example"));
        assert!(!is_ipv6_literal(""));
        assert!(!is_ipv6_literal("[::1]"));
    }
}
