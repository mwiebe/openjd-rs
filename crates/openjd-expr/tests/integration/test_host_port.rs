// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Tests for the host/port string functions added by the `SERVICE`
//! extension (Expression Language §2.2.4): `join_host_port`,
//! `split_host_port`, `is_ipv4`, `is_ipv6`.

use openjd_expr::{ExprType, ExprValue, ParsedExpression, SymbolTable};

fn eval(expr: &str) -> ExprValue {
    ParsedExpression::new(expr)
        .and_then(|p| p.evaluate(&SymbolTable::new()))
        .unwrap()
}

fn eval_str(expr: &str) -> String {
    eval(expr).to_display_string()
}

fn eval_bool(expr: &str) -> bool {
    match eval(expr) {
        ExprValue::Bool(b) => b,
        other => panic!("{expr}: expected bool, got {other:?}"),
    }
}

fn assert_err(expr: &str, expected: &[&str]) {
    let e = ParsedExpression::new(expr)
        .and_then(|p| p.evaluate(&SymbolTable::new()))
        .unwrap_err()
        .to_string();
    let joined = expected.concat();
    assert!(e.contains(&joined), "got:\n{e}\nexpected:\n{joined}");
}

fn st_unresolved(pairs: &[(&str, &str)]) -> SymbolTable {
    let mut st = SymbolTable::new();
    for (k, t) in pairs {
        st.set(k, ExprValue::unresolved(ExprType::parse(t).unwrap()))
            .unwrap();
    }
    st
}

fn eval_type(expr: &str, st: &SymbolTable) -> ExprType {
    ParsedExpression::new(expr)
        .and_then(|p| p.evaluate(st))
        .unwrap()
        .expr_type()
}

// ══════════════════════════════════════════════════════════════
// join_host_port
// ══════════════════════════════════════════════════════════════

#[test]
fn join_hostname() {
    // RFC 0009 / wiki §2.2.4 example.
    assert_eq!(
        eval_str("join_host_port('cache.example', 6379)"),
        "cache.example:6379"
    );
}

#[test]
fn join_ipv4() {
    assert_eq!(eval_str("join_host_port('10.0.0.1', 80)"), "10.0.0.1:80");
}

#[test]
fn join_ipv6_brackets() {
    // RFC 0009 / wiki §2.2.4 example.
    assert_eq!(
        eval_str("join_host_port('2001:db8::5', 6379)"),
        "[2001:db8::5]:6379"
    );
}

#[test]
fn join_already_bracketed_not_rebracketed() {
    // wiki §2.2.4: `join_host_port("[2001:db8::5]", 6379)` gives the same
    // result as the unbracketed form.
    assert_eq!(
        eval_str("join_host_port('[2001:db8::5]', 6379)"),
        "[2001:db8::5]:6379"
    );
}

#[test]
fn join_zone_identifier_verbatim() {
    // RFC 0009 / wiki §2.2.4 example: zone id carried through as Go's
    // net.JoinHostPort produces.
    assert_eq!(
        eval_str("join_host_port('fe80::1%eth0', 80)"),
        "[fe80::1%eth0]:80"
    );
}

#[test]
fn join_loopback_and_unspecified() {
    assert_eq!(eval_str("join_host_port('::1', 8080)"), "[::1]:8080");
    assert_eq!(eval_str("join_host_port('::', 8080)"), "[::]:8080");
}

#[test]
fn join_empty_host() {
    // Go's net.JoinHostPort("", "80") is ":80".
    assert_eq!(eval_str("join_host_port('', 80)"), ":80");
}

#[test]
fn join_method_syntax() {
    // RFC 0009: "Under method syntax, `addr.join_host_port(port)`."
    assert_eq!(
        eval_str("'2001:db8::5'.join_host_port(6379)"),
        "[2001:db8::5]:6379"
    );
    assert_eq!(
        eval_str("'cache.example'.join_host_port(6379)"),
        "cache.example:6379"
    );
}

#[test]
fn join_port_from_expression() {
    assert_eq!(eval_str("join_host_port('h', 8000 + 80)"), "h:8080");
}

#[test]
fn join_result_is_string_type() {
    assert_eq!(eval("join_host_port('h', 1)").expr_type(), ExprType::STRING);
}

#[test]
fn join_rejects_non_string_host() {
    assert_err(
        "join_host_port(1, 2)",
        &[
            "No matching signature for join_host_port(int, int)\n",
            "  join_host_port(1, 2)\n",
            "  ^~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn join_rejects_string_port() {
    assert_err(
        "join_host_port('h', '80')",
        &[
            "No matching signature for join_host_port(string, string)\n",
            "  join_host_port('h', '80')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn join_rejects_wrong_arity() {
    assert_err(
        "join_host_port('h')",
        &[
            "join_host_port() takes 2 argument(s), but 1 were given\n",
            "  join_host_port('h')\n",
            "  ^~~~~~~~~~~~~~~~~~~",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// split_host_port
// ══════════════════════════════════════════════════════════════

#[test]
fn split_bracketed_ipv6() {
    // RFC 0009 / wiki §2.2.4 example.
    assert_eq!(
        eval_str("split_host_port('[2001:db8::5]:6379')"),
        r#"["2001:db8::5", "6379"]"#
    );
}

#[test]
fn split_hostname() {
    assert_eq!(
        eval_str("split_host_port('cache.example:6379')"),
        r#"["cache.example", "6379"]"#
    );
}

#[test]
fn split_ipv4() {
    assert_eq!(
        eval_str("split_host_port('10.0.0.1:80')"),
        r#"["10.0.0.1", "80"]"#
    );
}

#[test]
fn split_no_port_is_null() {
    // wiki §2.2.4: `split_host_port("cache.example")` returns null.
    assert_eq!(eval("split_host_port('cache.example')"), ExprValue::Null);
}

#[test]
fn split_bare_ipv6_is_null() {
    // RFC 0009 / wiki §2.2.4: an unbracketed IPv6 literal is read as
    // having no port and returns null (Go would error).
    assert_eq!(eval("split_host_port('2001:db8::5')"), ExprValue::Null);
    assert_eq!(eval("split_host_port('::1')"), ExprValue::Null);
    assert_eq!(eval("split_host_port('fe80::1%eth0')"), ExprValue::Null);
}

#[test]
fn split_bracketed_without_port_is_null() {
    assert_eq!(eval("split_host_port('[2001:db8::5]')"), ExprValue::Null);
}

#[test]
fn split_empty_string_is_null() {
    assert_eq!(eval("split_host_port('')"), ExprValue::Null);
}

#[test]
fn split_port_is_string() {
    // wiki §2.2.4: the port element is a string; `int(...)` converts it.
    let r = eval("split_host_port('cache.example:6379')[1]");
    assert_eq!(r, ExprValue::String("6379".to_string()));
    assert_eq!(
        eval("int(split_host_port('cache.example:6379')[1])"),
        ExprValue::Int(6379)
    );
}

#[test]
fn split_result_type_is_list_string() {
    assert_eq!(
        eval("split_host_port('h:1')").expr_type(),
        ExprType::list(ExprType::STRING)
    );
}

#[test]
fn split_zone_identifier_kept_on_host() {
    // wiki §2.2.4: `split_host_port` returns the host with the zone attached.
    assert_eq!(
        eval_str("split_host_port('[fe80::1%eth0]:80')"),
        r#"["fe80::1%eth0", "80"]"#
    );
}

#[test]
fn split_roundtrips_join() {
    assert_eq!(
        eval_str("split_host_port(join_host_port('2001:db8::5', 6379))"),
        r#"["2001:db8::5", "6379"]"#
    );
    assert_eq!(
        eval_str(
            "join_host_port(split_host_port('[::1]:443')[0], int(split_host_port('[::1]:443')[1]))"
        ),
        "[::1]:443"
    );
}

#[test]
fn split_empty_port_follows_go() {
    // Go's net.SplitHostPort("host:") returns ("host", "") without error.
    assert_eq!(eval_str("split_host_port('host:')"), r#"["host", ""]"#);
}

#[test]
fn split_empty_bracketed_host_follows_go() {
    assert_eq!(eval_str("split_host_port('[]:80')"), r#"["", "80"]"#);
}

#[test]
fn split_method_syntax() {
    assert_eq!(
        eval_str("'[2001:db8::5]:6379'.split_host_port()"),
        r#"["2001:db8::5", "6379"]"#
    );
}

#[test]
fn split_null_result_in_conditional() {
    assert_eq!(
        eval_str("'none' if split_host_port('cache.example') == null else 'some'"),
        "none"
    );
}

#[test]
fn split_missing_closing_bracket_is_error() {
    // RFC 0009 / wiki §2.2.4 example: `"[::1"` is an error.
    assert_err(
        "split_host_port('[::1')",
        &[
            "split_host_port failed: missing ']' in '[::1'\n",
            "  split_host_port('[::1')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_characters_after_bracket_is_error() {
    // RFC 0009 / wiki §2.2.4 example: `"[::1]x:80"` is an error.
    assert_err(
        "split_host_port('[::1]x:80')",
        &[
            "split_host_port failed: unexpected characters after ']' in '[::1]x:80'\n",
            "  split_host_port('[::1]x:80')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_too_many_colons_after_bracket_is_error() {
    assert_err(
        "split_host_port('[::1]:80:90')",
        &[
            "split_host_port failed: too many colons in '[::1]:80:90'\n",
            "  split_host_port('[::1]:80:90')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_stray_open_bracket_is_error() {
    assert_err(
        "split_host_port('a[b:80')",
        &[
            "split_host_port failed: unexpected '[' or ']' in 'a[b:80'\n",
            "  split_host_port('a[b:80')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_stray_close_bracket_is_error() {
    assert_err(
        "split_host_port('::1]:80')",
        &[
            "split_host_port failed: unexpected '[' or ']' in '::1]:80'\n",
            "  split_host_port('::1]:80')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_stray_bracket_without_colon_is_error() {
    assert_err(
        "split_host_port('ab]')",
        &[
            "split_host_port failed: unexpected '[' or ']' in 'ab]'\n",
            "  split_host_port('ab]')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_doubled_open_bracket_is_error() {
    assert_err(
        "split_host_port('[[::1]:80')",
        &[
            "split_host_port failed: unexpected '[' or ']' in '[[::1]:80'\n",
            "  split_host_port('[[::1]:80')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_bracket_in_port_is_error() {
    assert_err(
        "split_host_port('[::1]:8]0')",
        &[
            "split_host_port failed: unexpected '[' or ']' in '[::1]:8]0'\n",
            "  split_host_port('[::1]:8]0')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_error_caret_in_method_syntax() {
    assert_err(
        "'[::1'.split_host_port()",
        &[
            "split_host_port failed: missing ']' in '[::1'\n",
            "  '[::1'.split_host_port()\n",
            "  ~~~~~~~^~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn split_rejects_non_string() {
    assert_err(
        "split_host_port(80)",
        &[
            "No matching signature for split_host_port(int)\n",
            "  split_host_port(80)\n",
            "  ^~~~~~~~~~~~~~~~~~~",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// is_ipv4 / is_ipv6
// ══════════════════════════════════════════════════════════════

#[test]
fn ipv4_literal() {
    // wiki §2.2.4 example.
    assert!(eval_bool("is_ipv4('10.0.0.1')"));
    assert!(eval_bool("is_ipv4('0.0.0.0')"));
    assert!(eval_bool("is_ipv4('255.255.255.255')"));
}

#[test]
fn ipv4_rejects_non_literals() {
    assert!(!eval_bool("is_ipv4('cache.example')"));
    assert!(!eval_bool("is_ipv4('::1')"));
    assert!(!eval_bool("is_ipv4('10.0.0')"));
    assert!(!eval_bool("is_ipv4('10.0.0.256')"));
    assert!(!eval_bool("is_ipv4('10.0.0.1:80')"));
    assert!(!eval_bool("is_ipv4(' 10.0.0.1')"));
    assert!(!eval_bool("is_ipv4('')"));
}

#[test]
fn ipv4_rejects_leading_zeros() {
    // Octets with leading zeros are ambiguous (octal in some parsers)
    // and are rejected, as Go's netip.ParseAddr does.
    assert!(!eval_bool("is_ipv4('01.2.3.4')"));
}

#[test]
fn ipv4_method_syntax() {
    assert!(eval_bool("'10.0.0.1'.is_ipv4()"));
}

#[test]
fn ipv6_literal_forms() {
    // wiki §2.2.4 examples.
    assert!(eval_bool("is_ipv6('::1')"));
    assert!(eval_bool("is_ipv6('[::1]')"));
    assert!(eval_bool("is_ipv6('fe80::1%eth0')"));
    // RFC 0009 table: bracketed or not, with or without a zone id.
    assert!(eval_bool("is_ipv6('[fe80::1%eth0]')"));
    assert!(eval_bool("is_ipv6('2001:db8::5')"));
    assert!(eval_bool("is_ipv6('::')"));
    assert!(eval_bool(
        "is_ipv6('2001:0db8:0000:0000:0000:0000:0000:0005')"
    ));
    assert!(eval_bool("is_ipv6('::ffff:10.0.0.1')"));
}

#[test]
fn ipv6_rejects_non_literals() {
    assert!(!eval_bool("is_ipv6('10.0.0.1')"));
    assert!(!eval_bool("is_ipv6('cache.example')"));
    assert!(!eval_bool("is_ipv6('[::1]:80')"));
    assert!(!eval_bool("is_ipv6('[::1')"));
    assert!(!eval_bool("is_ipv6('::1]')"));
    assert!(!eval_bool("is_ipv6('fe80::1%')"));
    assert!(!eval_bool("is_ipv6('2001:db8::5::1')"));
    assert!(!eval_bool("is_ipv6('')"));
    assert!(!eval_bool("is_ipv6('[]')"));
}

#[test]
fn ipv6_method_syntax() {
    assert!(eval_bool("'::1'.is_ipv6()"));
}

#[test]
fn ip_predicates_return_bool_type() {
    assert_eq!(eval("is_ipv4('1.2.3.4')").expr_type(), ExprType::BOOL);
    assert_eq!(eval("is_ipv6('::1')").expr_type(), ExprType::BOOL);
}

#[test]
fn ip_predicates_reject_non_string() {
    assert_err(
        "is_ipv4(5)",
        &[
            "No matching signature for is_ipv4(int)\n",
            "  is_ipv4(5)\n",
            "  ^~~~~~~~~~",
        ],
    );
    assert_err(
        "is_ipv6(5)",
        &[
            "No matching signature for is_ipv6(int)\n",
            "  is_ipv6(5)\n",
            "  ^~~~~~~~~~",
        ],
    );
}

#[test]
fn address_family_flag_pattern() {
    // RFC 0009 motivation: pick an address-family flag from the literal.
    assert_eq!(eval_str("'-6' if is_ipv6('2001:db8::5') else '-4'"), "-6");
    assert_eq!(eval_str("'-6' if is_ipv6('10.0.0.1') else '-4'"), "-4");
}

// ══════════════════════════════════════════════════════════════
// Static typing with unresolved arguments
// ══════════════════════════════════════════════════════════════

#[test]
fn unresolved_join_host_port_types() {
    let st = st_unresolved(&[("S", "string"), ("P", "int")]);
    let want = ExprType::parse("unresolved[string]").unwrap();
    assert_eq!(eval_type("join_host_port(S, 80)", &st), want);
    assert_eq!(eval_type("join_host_port('h', P)", &st), want);
    assert_eq!(eval_type("join_host_port(S, P)", &st), want);
    assert_eq!(eval_type("S.join_host_port(P)", &st), want);
}

#[test]
fn unresolved_split_host_port_type_is_optional_list() {
    let st = st_unresolved(&[("S", "string")]);
    assert_eq!(
        eval_type("split_host_port(S)", &st),
        ExprType::parse("unresolved[list[string]?]").unwrap()
    );
    assert_eq!(
        eval_type("S.split_host_port()", &st),
        ExprType::parse("unresolved[list[string]?]").unwrap()
    );
}

#[test]
fn unresolved_ip_predicate_types() {
    let st = st_unresolved(&[("S", "string")]);
    let want = ExprType::parse("unresolved[bool]").unwrap();
    assert_eq!(eval_type("is_ipv4(S)", &st), want);
    assert_eq!(eval_type("is_ipv6(S)", &st), want);
    assert_eq!(eval_type("S.is_ipv6()", &st), want);
}

#[test]
fn unresolved_join_with_wrong_port_type_is_static_error() {
    let st = st_unresolved(&[("S", "string")]);
    let e = ParsedExpression::new("join_host_port(S, '80')")
        .and_then(|p| p.evaluate(&st))
        .unwrap_err()
        .to_string();
    let expected = concat!(
        "No matching signature for join_host_port(string, string)\n",
        "  join_host_port(S, '80')\n",
        "  ^~~~~~~~~~~~~~~~~~~~~~~",
    );
    assert!(e.contains(expected), "got:\n{e}\nexpected:\n{expected}");
}
