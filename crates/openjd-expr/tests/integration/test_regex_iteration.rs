// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Match iteration in re_split, re_findall and re_sub: empty matches,
//! maxsplit, and capture groups, checked against CPython's `re`.
//!
//! See "Regex match iteration" in specs/expr/function-library.md. The
//! differential tests at the bottom compare every case in
//! `regex_iteration_fixtures.rs`, generated from CPython by
//! `scripts/generate_regex_iteration_fixtures.py`.

use openjd_expr::{ExprValue, ParsedExpression, SymbolTable, DEFAULT_OPERATION_LIMIT};

#[rustfmt::skip]
#[path = "regex_iteration_fixtures.rs"]
mod fixtures;

pub(crate) struct SplitCase {
    pub s: &'static str,
    pub pattern: &'static str,
    pub maxsplit: Option<i64>,
    pub expected: &'static [Option<&'static str>],
    pub divergent: Option<&'static [&'static str]>,
}

pub(crate) enum FindAll {
    Flat(&'static [&'static str]),
    Groups(&'static [&'static [&'static str]]),
}

pub(crate) struct FindAllCase {
    pub s: &'static str,
    pub pattern: &'static str,
    pub expected: FindAll,
    pub divergent: Option<FindAll>,
}

pub(crate) struct SubCase {
    pub s: &'static str,
    pub pattern: &'static str,
    pub expected: &'static str,
    pub divergent: Option<&'static str>,
}

fn eval(expr: &str) -> ExprValue {
    ParsedExpression::new(expr)
        .and_then(|p| p.evaluate(&SymbolTable::new()))
        .unwrap()
}

fn eval_err(expr: &str) -> String {
    ParsedExpression::new(expr)
        .and_then(|p| p.evaluate(&SymbolTable::new()))
        .unwrap_err()
        .to_string()
}

fn assert_err(expr: &str, expected: &[&str]) {
    let e = eval_err(expr);
    let joined = expected.concat();
    assert!(e.contains(&joined), "got:\n{e}\nexpected:\n{joined}");
}

fn display(expr: &str) -> String {
    eval(expr).to_display_string()
}

fn strings(v: &ExprValue) -> Vec<String> {
    match v {
        ExprValue::ListString(items, _) => items.clone(),
        other if other.list_len() == Some(0) => Vec::new(),
        other => panic!("expected list[string], got {other:?}"),
    }
}

fn eval_with(
    expr: &str,
    s: &str,
    pattern: &str,
    maxsplit: Option<i64>,
) -> Result<ExprValue, String> {
    let mut st = SymbolTable::new();
    st.set("Param.S", s).unwrap();
    st.set("Param.P", pattern).unwrap();
    if let Some(n) = maxsplit {
        st.set("Param.M", n).unwrap();
    }
    ParsedExpression::new(expr)
        .and_then(|p| p.evaluate(&st))
        .map_err(|e| e.to_string())
}

// === The reported cases ===

#[test]
fn re_split_splits_at_empty_match_after_separator() {
    assert_eq!(
        display("re_split('a,b,c', ',?')"),
        r#"["", "a", "", "b", "", "c", ""]"#
    );
}

#[test]
fn re_split_maxsplit_counts_empty_matches() {
    assert_eq!(
        display("re_split(',a,,b,', ',?', 2)"),
        r#"["", "", "a,,b,"]"#
    );
}

// === Empty and zero-width patterns ===

#[test]
fn re_split_empty_pattern_is_an_error() {
    assert_err(
        "re_split('abc', '', 2)",
        &[
            "Empty regex pattern is not allowed\n",
            "  re_split('abc', '', 2)\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn re_split_empty_group_splits_at_every_position() {
    // `(?:)` is the non-empty spelling of the empty pattern.
    assert_eq!(
        display("re_split('abc', '(?:)')"),
        r#"["", "a", "b", "c", ""]"#
    );
}

#[test]
fn re_split_lookahead_is_an_error() {
    // Lookaround is outside the regex dialect (spec §2.2.5); \b, ^ and $
    // are the zero-width patterns it has.
    assert_err(
        "re_split('abc', '(?=b)')",
        &[
            "Unsupported regex feature: lookahead\n",
            "  re_split('abc', '(?=b)')\n",
            "  ^~~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn re_split_word_boundary() {
    assert_eq!(
        display(r"re_split('ab cd', '\\b')"),
        r#"["", "ab", " ", "cd", ""]"#
    );
}

#[test]
fn re_split_anchors() {
    assert_eq!(display("re_split('abc', '^')"), r#"["", "abc"]"#);
    assert_eq!(display("re_split('abc', '$')"), r#"["abc", ""]"#);
    assert_eq!(
        display(r"re_split('a\nb', '(?m)^')"),
        "[\"\", \"a\\n\", \"b\"]"
    );
}

#[test]
fn re_split_pattern_matching_at_start_and_end() {
    assert_eq!(display("re_split(',a,', ',?')"), r#"["", "", "a", "", ""]"#);
    assert_eq!(display("re_split(',a,', ',')"), r#"["", "a", ""]"#);
}

#[test]
fn re_split_non_ascii_steps_by_character() {
    assert_eq!(display("re_split('日本', 'x*')"), r#"["", "日", "本", ""]"#);
}

// === maxsplit mixed with empty matches ===

#[test]
fn re_split_maxsplit_with_empty_matches() {
    assert_eq!(
        display("re_split('a,b', ',?', 0)"),
        r#"["", "a", "", "b", ""]"#
    );
    assert_eq!(display("re_split('a,b', ',?', 1)"), r#"["", "a,b"]"#);
    assert_eq!(display("re_split('a,b', ',?', 2)"), r#"["", "a", "b"]"#);
    assert_eq!(display("re_split('a,b', ',?', 3)"), r#"["", "a", "", "b"]"#);
    assert_eq!(display("re_split('a,b', ',?', -1)"), r#"["a,b"]"#);
    assert_eq!(display("'a,b'.re_split(',?', 2)"), r#"["", "a", "b"]"#);
}

// === Capture groups ===

#[test]
fn re_split_includes_capture_groups() {
    assert_eq!(display(r"re_split('a1b', '(\\d)')"), r#"["a", "1", "b"]"#);
    assert_eq!(display("re_split('a,b', '(,)')"), r#"["a", ",", "b"]"#);
}

#[test]
fn re_split_includes_every_participating_group() {
    assert_eq!(
        display("re_split('xaby', '(a)(b)')"),
        r#"["x", "a", "b", "y"]"#
    );
}

#[test]
fn re_split_drops_groups_that_did_not_take_part() {
    // CPython: ['a', ',', None, 'b', None, ';', 'c']
    assert_eq!(
        display("re_split('a,b;c', '(,)|(;)')"),
        r#"["a", ",", "b", ";", "c"]"#
    );
    // CPython: ['a', None, 'b', '-', 'c']. A group that always takes part
    // keeps one entry per match.
    assert_eq!(
        display("re_split('a,b-,c', '(-)?,')"),
        r#"["a", "b", "-", "c"]"#
    );
    assert_eq!(
        display("re_split('a,b-,c', '(-?),')"),
        r#"["a", "", "b", "-", "c"]"#
    );
}

#[test]
fn re_split_groups_with_empty_matches() {
    // CPython: ['', None, 'a', '1', '', None, 'b', '2', '', None, 'c', None, '']
    assert_eq!(
        display(r"re_split('a1b22c', '(\\d)*')"),
        r#"["", "a", "1", "", "b", "2", "", "c", ""]"#
    );
    assert_eq!(
        display("re_split('a,b', '(,?)')"),
        r#"["", "", "a", ",", "", "", "b", "", ""]"#
    );
}

// === re_findall and re_sub share the iteration ===

#[test]
fn re_findall_reports_empty_match_after_non_empty_match() {
    assert_eq!(
        display("re_findall('baaac', 'a*')"),
        r#"["", "aaa", "", ""]"#
    );
    assert_eq!(display("re_findall('a,b', ',?')"), r#"["", ",", "", ""]"#);
}

#[test]
fn re_sub_replaces_empty_match_after_non_empty_match() {
    assert_eq!(display("re_sub('baaac', 'a*', '-')"), "-b--c-");
    assert_eq!(display("re_sub('a,b', ',?', '-')"), "-a--b-");
}

#[test]
fn re_findall_empty_result_keeps_its_element_type() {
    assert_eq!(display("re_findall('', 'a') + ['x']"), r#"["x"]"#);
    assert_eq!(display("re_findall('', '(a)') + ['x']"), r#"["x"]"#);
    assert_eq!(display("re_findall('', '(a)(b)') + [['x']]"), r#"[["x"]]"#);
}

// === Known divergence: repeated capture groups ===

#[test]
fn re_split_repeated_group_keeps_last_non_empty_iteration() {
    // The regex crate keeps a repeated group's last non-empty iteration;
    // CPython keeps the last iteration even when it is empty, and gives
    // ['', '', '', '', '']. Documented in function-library.md.
    assert_eq!(
        display("re_split('a', '(a?)*')"),
        r#"["", "a", "", "", ""]"#
    );
}

// === Known divergence: lazy and empty-first patterns ===

#[test]
fn re_findall_lazy_pattern_moves_on_after_repeated_empty_match() {
    // CPython: ['', 'a', '', 'a', '']. Documented in function-library.md.
    assert_eq!(display("re_findall('aa', 'a??')"), r#"["", "", ""]"#);
}

#[test]
fn re_split_empty_first_alternative_moves_on_after_repeated_empty_match() {
    // CPython: ['', 'x', '', '', ''].
    assert_eq!(display("re_split('xa', '|a')"), r#"["", "x", "a", ""]"#);
}

#[test]
fn re_sub_lazy_pattern_moves_on_by_one_character() {
    // CPython: '-日---'.
    assert_eq!(display("re_sub('日a', 'a*?', '-')"), "-日-a-");
}

#[test]
fn lazy_pattern_agrees_with_cpython_without_a_non_empty_alternative() {
    assert_eq!(display("re_findall('bb', 'a??')"), r#"["", "", ""]"#);
    assert_eq!(display("re_split('aa', 'a??', 1)"), r#"["", "aa"]"#);
}

// === Bounds ===

/// Evaluate `expr` over 2,000 `a`s under a 10,000-byte memory limit, and
/// return the error.
fn memory_error(expr: &str) -> String {
    let mut st = SymbolTable::new();
    st.set("Param.Text", "a".repeat(2_000)).unwrap();
    ParsedExpression::new(expr)
        .and_then(|p| {
            p.with_memory_limit(10_000)
                .with_operation_limit(DEFAULT_OPERATION_LIMIT)
                .evaluate_with_metrics(&[&st])
        })
        .unwrap_err()
        .to_string()
}

#[test]
fn re_split_empty_match_at_every_character_respects_memory_limit() {
    assert_eq!(
        memory_error("re_split(Param.Text, 'x*')"),
        "Expression memory usage (16512 bytes) exceeded limit (10000 bytes)\n\
         \x20 re_split(Param.Text, 'x*')\n\
         \x20 ^~~~~~~~~~~~~~~~~~~~~~~~~~"
    );
}

#[test]
fn re_findall_empty_match_at_every_character_respects_memory_limit() {
    assert_eq!(
        memory_error("re_findall(Param.Text, 'x*')"),
        "Expression memory usage (16384 bytes) exceeded limit (10000 bytes)\n\
         \x20 re_findall(Param.Text, 'x*')\n\
         \x20 ^~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    );
}

#[test]
fn re_sub_empty_match_at_every_character_respects_memory_limit() {
    // The result string is checked before it grows past the limit.
    assert_eq!(
        memory_error("re_sub(Param.Text, 'x*', '----------')"),
        "Expression memory usage (10009 bytes) exceeded limit (10000 bytes)\n\
         \x20 re_sub(Param.Text, 'x*', '----------')\n\
         \x20 ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    );
}

#[test]
fn empty_matches_charge_the_same_operations() {
    // The operation charge depends on the input length only, as it did
    // before empty matches counted: one empty match per character costs no
    // more than a pattern that never matches.
    let ops = |expr: &str, pattern: &str| {
        let mut st = SymbolTable::new();
        st.set("Param.Text", "a".repeat(10_000)).unwrap();
        st.set("Param.P", pattern).unwrap();
        ParsedExpression::new(expr)
            .and_then(|p| p.evaluate_with_metrics(&[&st]))
            .unwrap()
            .operation_count
    };
    for expr in [
        "re_split(Param.Text, Param.P)",
        "re_findall(Param.Text, Param.P)",
        "re_sub(Param.Text, Param.P, '-')",
    ] {
        assert_eq!(ops(expr, "x*"), ops(expr, ","), "{expr}");
    }
}

// === Differential check against CPython ===

#[test]
fn fixtures_come_from_cpython() {
    assert!(fixtures::PYTHON_VERSION.starts_with("CPython 3."));
    assert_eq!(fixtures::SPLIT_CASES.len(), 6496);
    assert_eq!(fixtures::FINDALL_CASES.len(), 812);
    assert_eq!(fixtures::SUB_CASES.len(), 812);
    let divergent = fixtures::SPLIT_CASES
        .iter()
        .filter(|c| c.divergent.is_some())
        .count()
        + fixtures::FINDALL_CASES
            .iter()
            .filter(|c| c.divergent.is_some())
            .count()
        + fixtures::SUB_CASES
            .iter()
            .filter(|c| c.divergent.is_some())
            .count();
    assert_eq!(divergent, 450);
}

fn report(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} mismatches, first 20:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Matches CPython's re.split, with None (a group that did not take part)
/// dropped, except where the fixture records the documented divergence.
#[test]
fn re_split_matches_cpython() {
    let mut failures = Vec::new();
    for c in fixtures::SPLIT_CASES {
        let (expr, what) = match c.maxsplit {
            None => (
                "re_split(Param.S, Param.P)",
                format!("re_split({:?}, {:?})", c.s, c.pattern),
            ),
            Some(n) => (
                "re_split(Param.S, Param.P, Param.M)",
                format!("re_split({:?}, {:?}, {n})", c.s, c.pattern),
            ),
        };
        let want: Vec<String> = match c.divergent {
            Some(ours) => ours.iter().map(|s| s.to_string()).collect(),
            None => c.expected.iter().flatten().map(|s| s.to_string()).collect(),
        };
        match eval_with(expr, c.s, c.pattern, c.maxsplit) {
            Ok(v) if strings(&v) == want => {}
            other => failures.push(format!("{what}: expected {want:?}, got {other:?}")),
        }
    }
    report(failures);
}

fn findall_ok(v: &ExprValue, want: &FindAll) -> bool {
    if v.list_len() == Some(0) {
        let elem = match want {
            FindAll::Flat(w) => w.is_empty().then_some("list[string]"),
            FindAll::Groups(w) => w.is_empty().then_some("list[list[string]]"),
        };
        return elem == Some(v.expr_type().to_string().as_str());
    }
    match (v, want) {
        (v, FindAll::Flat(w)) => strings(v) == *w,
        (ExprValue::ListList(rows, _, _), FindAll::Groups(w)) => {
            rows.len() == w.len() && rows.iter().zip(w.iter()).all(|(r, w)| strings(r) == *w)
        }
        _ => false,
    }
}

#[test]
fn re_findall_matches_cpython() {
    let mut failures = Vec::new();
    for c in fixtures::FINDALL_CASES {
        let what = format!("re_findall({:?}, {:?})", c.s, c.pattern);
        let want = c.divergent.as_ref().unwrap_or(&c.expected);
        match eval_with("re_findall(Param.S, Param.P)", c.s, c.pattern, None) {
            Ok(v) if findall_ok(&v, want) => {}
            other => failures.push(format!("{what}: got {other:?}")),
        }
    }
    report(failures);
}

#[test]
fn re_sub_matches_cpython() {
    let mut failures = Vec::new();
    for c in fixtures::SUB_CASES {
        let what = format!("re_sub({:?}, {:?}, '-')", c.s, c.pattern);
        let want = c.divergent.unwrap_or(c.expected);
        match eval_with("re_sub(Param.S, Param.P, '-')", c.s, c.pattern, None) {
            Ok(ExprValue::String(s)) if s == want => {}
            other => failures.push(format!("{what}: expected {want:?}, got {other:?}")),
        }
    }
    report(failures);
}
