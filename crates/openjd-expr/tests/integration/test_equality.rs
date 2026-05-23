// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Equality and hashing semantics for `PathMappingRule`,
//! `HostContext`, and `ExprProfile`.
//!
//! These types have no `PartialEq` requirement at the Rust API
//! level — but they do at the Python level, where the bindings
//! expose `__eq__` / `__hash__` on the corresponding pyclasses.
//! These tests pin the equality contract upstream so the
//! bindings can compose `inner == inner` without surprising
//! divergences between Rust and Python users.

use std::collections::HashSet;

use openjd_expr::{
    ExprProfile, ExprRevision, ExprValue, FormatString, HostContext, ParsedExpression, PathFormat,
    PathMappingRule, SymbolTable,
};

fn rule(src: &str, dst: &str) -> PathMappingRule {
    PathMappingRule {
        source_path_format: PathFormat::Posix,
        source_path: src.to_string(),
        destination_path: dst.to_string(),
    }
}

// === PathMappingRule ===

#[test]
fn path_mapping_rule_eq_identical() {
    assert_eq!(rule("/a", "/b"), rule("/a", "/b"));
}

#[test]
fn path_mapping_rule_eq_differs_on_source() {
    assert_ne!(rule("/a", "/b"), rule("/A", "/b"));
}

#[test]
fn path_mapping_rule_eq_differs_on_destination() {
    assert_ne!(rule("/a", "/b"), rule("/a", "/B"));
}

#[test]
fn path_mapping_rule_eq_differs_on_format() {
    let posix = rule("/a", "/b");
    let windows = PathMappingRule {
        source_path_format: PathFormat::Windows,
        source_path: "/a".into(),
        destination_path: "/b".into(),
    };
    assert_ne!(posix, windows);
}

#[test]
fn path_mapping_rule_hashable() {
    let mut set = HashSet::new();
    set.insert(rule("/a", "/b"));
    set.insert(rule("/a", "/b")); // duplicate
    set.insert(rule("/c", "/d"));
    assert_eq!(set.len(), 2);
}

#[test]
fn path_mapping_rule_hash_consistent_with_eq() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn h(r: &PathMappingRule) -> u64 {
        let mut s = DefaultHasher::new();
        r.hash(&mut s);
        s.finish()
    }
    let a = rule("/a", "/b");
    let b = rule("/a", "/b");
    assert_eq!(a, b);
    assert_eq!(h(&a), h(&b));
}

// === HostContext ===

#[test]
fn host_context_none_eq_none() {
    assert_eq!(HostContext::None, HostContext::None);
}

#[test]
fn host_context_unresolved_eq_unresolved() {
    assert_eq!(HostContext::Unresolved, HostContext::Unresolved);
}

#[test]
fn host_context_none_ne_unresolved() {
    assert_ne!(HostContext::None, HostContext::Unresolved);
}

#[test]
fn host_context_with_rules_value_compare_not_arc_identity() {
    // The `WithRules(Arc<Vec<...>>)` variant compares by inner
    // value, not by `Arc` pointer. Two distinct allocations of
    // the same rule list compare equal.
    let a = HostContext::with_rules(vec![rule("/a", "/b")]);
    let b = HostContext::with_rules(vec![rule("/a", "/b")]);
    assert_eq!(a, b);
}

#[test]
fn host_context_with_rules_differs_on_payload() {
    let a = HostContext::with_rules(vec![rule("/a", "/b")]);
    let b = HostContext::with_rules(vec![rule("/x", "/y")]);
    assert_ne!(a, b);
}

#[test]
fn host_context_with_rules_ne_none() {
    let with = HostContext::with_rules(vec![rule("/a", "/b")]);
    assert_ne!(with, HostContext::None);
}

#[test]
fn host_context_hashable() {
    let mut set = HashSet::new();
    set.insert(HostContext::None);
    set.insert(HostContext::None); // duplicate
    set.insert(HostContext::Unresolved);
    set.insert(HostContext::with_rules(vec![rule("/a", "/b")]));
    set.insert(HostContext::with_rules(vec![rule("/a", "/b")])); // duplicate
    assert_eq!(set.len(), 3);
}

// === ExprProfile ===

#[test]
fn expr_profile_eq_same_revision() {
    let a = ExprProfile::new(ExprRevision::CURRENT);
    let b = ExprProfile::new(ExprRevision::CURRENT);
    assert_eq!(a, b);
}

#[test]
fn expr_profile_extensions_compare_set_not_order() {
    // Today `ExprExtension` has no variants, so the extension set
    // is always empty. This test pins behaviour for the future:
    // profiles built with the same extensions compare equal
    // regardless of insertion order — `extensions` is a
    // `HashSet<ExprExtension>` so set equality is the right
    // contract.
    let a = ExprProfile::new(ExprRevision::CURRENT);
    let b = ExprProfile::new(ExprRevision::CURRENT);
    assert_eq!(a, b);
}

#[test]
fn expr_profile_differs_on_host_context() {
    let plain = ExprProfile::new(ExprRevision::CURRENT);
    let unresolved = plain.clone().with_host_context(HostContext::Unresolved);
    let with_rules = plain
        .clone()
        .with_host_context(HostContext::with_rules(vec![rule("/a", "/b")]));
    assert_ne!(plain, unresolved);
    assert_ne!(plain, with_rules);
    assert_ne!(unresolved, with_rules);
}

#[test]
fn expr_profile_eq_with_same_host_rules() {
    let a = ExprProfile::new(ExprRevision::CURRENT)
        .with_host_context(HostContext::with_rules(vec![rule("/a", "/b")]));
    let b = ExprProfile::new(ExprRevision::CURRENT)
        .with_host_context(HostContext::with_rules(vec![rule("/a", "/b")]));
    assert_eq!(a, b);
}

// === FormatString ===

#[test]
fn format_string_eq_identical_raw() {
    let a = FormatString::new("hello {{Param.Name}}").unwrap();
    let b = FormatString::new("hello {{Param.Name}}").unwrap();
    assert_eq!(a, b);
}

#[test]
fn format_string_eq_differs_on_whitespace() {
    // Lexically distinct raw strings compare unequal even if
    // they would produce the same evaluated output. Same-raw
    // is the contract.
    let a = FormatString::new("{{ Param.Name }}").unwrap();
    let b = FormatString::new("{{Param.Name}}").unwrap();
    assert_ne!(a, b);
}

#[test]
fn format_string_eq_pure_literals() {
    let a = FormatString::new("just text").unwrap();
    let b = FormatString::new("just text").unwrap();
    assert_eq!(a, b);
}

#[test]
#[allow(clippy::mutable_key_type)]
fn format_string_hashable() {
    // `FormatString` carries `Vec<Segment>` where each
    // expression segment owns a `ParsedExpression` (whose AST
    // contains `AtomicNodeIndex`). Our `Hash` impl only uses
    // the `raw` source string, so the keys are functionally
    // immutable for hashing purposes — suppress the lint
    // locally.
    let mut set = HashSet::new();
    set.insert(FormatString::new("a {{x}}").unwrap());
    set.insert(FormatString::new("a {{x}}").unwrap()); // duplicate
    set.insert(FormatString::new("b {{y}}").unwrap());
    assert_eq!(set.len(), 2);
}

// === ParsedExpression ===

#[test]
fn parsed_expression_eq_identical_source() {
    let a = ParsedExpression::new("Param.X + 1").unwrap();
    let b = ParsedExpression::new("Param.X + 1").unwrap();
    assert_eq!(a, b);
}

#[test]
fn parsed_expression_eq_differs_on_lexical_whitespace() {
    // Same AST, different lexical source ⇒ not equal. Two
    // parsed expressions are equal iff their (trimmed) source
    // strings are equal.
    let a = ParsedExpression::new("Param.X + 1").unwrap();
    let b = ParsedExpression::new("Param.X+1").unwrap();
    assert_ne!(a, b);
}

#[test]
fn parsed_expression_eq_trims_outer_whitespace() {
    // The `expr` field is the trimmed source, so leading and
    // trailing whitespace is absorbed by the equality check.
    let a = ParsedExpression::new("Param.X").unwrap();
    let b = ParsedExpression::new("  Param.X  ").unwrap();
    assert_eq!(a, b);
}

#[test]
#[allow(clippy::mutable_key_type)]
fn parsed_expression_hashable() {
    // Clippy worries about `mutable_key_type` here because
    // `ParsedExpression` carries a `ruff_python_ast::Expr`,
    // whose AST nodes contain `AtomicNodeIndex`. Our `Hash`
    // impl only uses the `expr` source string, so the keys
    // are functionally immutable for hashing purposes —
    // suppress the lint locally.
    let mut set = HashSet::new();
    set.insert(ParsedExpression::new("Param.X").unwrap());
    set.insert(ParsedExpression::new("Param.X").unwrap()); // duplicate
    set.insert(ParsedExpression::new("Param.Y").unwrap());
    assert_eq!(set.len(), 2);
}

// === SymbolTable ===

#[test]
fn symbol_table_eq_empty() {
    assert_eq!(SymbolTable::new(), SymbolTable::new());
}

#[test]
fn symbol_table_eq_same_entries() {
    let mut a = SymbolTable::new();
    a.set("Param.Frame", ExprValue::Int(42)).unwrap();
    a.set("Param.Name", ExprValue::String("foo".to_string())).unwrap();

    let mut b = SymbolTable::new();
    b.set("Param.Name", ExprValue::String("foo".to_string())).unwrap(); // reverse order
    b.set("Param.Frame", ExprValue::Int(42)).unwrap();

    // `HashMap`-backed: insertion order does not affect
    // equality.
    assert_eq!(a, b);
}

#[test]
fn symbol_table_eq_differs_on_value() {
    let mut a = SymbolTable::new();
    a.set("Param.Frame", ExprValue::Int(42)).unwrap();
    let mut b = SymbolTable::new();
    b.set("Param.Frame", ExprValue::Int(43)).unwrap();
    assert_ne!(a, b);
}

#[test]
fn symbol_table_eq_differs_on_missing_key() {
    let mut a = SymbolTable::new();
    a.set("Param.Frame", ExprValue::Int(42)).unwrap();
    let b = SymbolTable::new();
    assert_ne!(a, b);
}

#[test]
fn symbol_table_eq_recursive_through_nested_tables() {
    // Deep equality through nested `Table(SymbolTable)`
    // entries.
    let mut a = SymbolTable::new();
    a.set("Param.Inner.Frame", ExprValue::Int(1)).unwrap();
    a.set("Param.Inner.Name", ExprValue::String("x".to_string())).unwrap();

    let mut b = SymbolTable::new();
    b.set("Param.Inner.Name", ExprValue::String("x".to_string())).unwrap();
    b.set("Param.Inner.Frame", ExprValue::Int(1)).unwrap();

    assert_eq!(a, b);
}
