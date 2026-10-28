// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Regex function implementations.

use crate::budgeted_vec::BudgetedVec;
use crate::error::ExpressionError;
use crate::function_library::EvalContext;
use crate::types::ExprType;
use crate::value::ExprValue;

type R = Result<ExprValue, ExpressionError>;
type Ctx<'a> = &'a mut dyn EvalContext;

fn get_two_strings(a: &[ExprValue], name: &str) -> Result<(String, String), ExpressionError> {
    let s = match &a[0] {
        ExprValue::String(s) => s.clone(),
        _ => {
            return Err(ExpressionError::new(format!(
                "{name}() requires string arguments"
            )))
        }
    };
    let p = match &a[1] {
        ExprValue::String(s) => s.clone(),
        _ => {
            return Err(ExpressionError::new(format!(
                "{name}() requires string arguments"
            )))
        }
    };
    Ok((s, p))
}

fn validate_regex_pattern(pattern: &str) -> Result<(), ExpressionError> {
    if pattern.is_empty() {
        return Err(ExpressionError::new("Empty regex pattern is not allowed"));
    }

    // Reject Rust-only constructs that `regex_syntax` happily parses but
    // that the spec (§2.2.5) forbids for Python/Rust cross-engine
    // compatibility:
    //   - `\z`           — Rust-only end-of-string anchor
    //   - `\x{HHHH}`     — Rust-only Unicode brace syntax
    //   - `\u{HHHH}`     — Rust-only Unicode brace syntax
    //   - `\U{HHHH}`     — Rust-only Unicode brace syntax
    //
    // The source scan walks the pattern character by character, tracking
    // whether a backslash is active (i.e., the previous char was an
    // unescaped `\`). This correctly handles escape parity — e.g.,
    // `\\z` contains a literal backslash followed by a `z`, not the `\z`
    // anchor — without re-implementing the full regex grammar.
    reject_rust_only_features(pattern)?;

    // Parse the pattern into its AST first and walk it. Several Rust-only
    // constructs are erased (or normalized away) by the AST→HIR translation
    // — `(?<name>...)` vs `(?P<name>...)`, POSIX classes, `\p{...}`, and
    // character class set operators all become plain captures/classes in
    // HIR — so they can only be rejected at the AST level. Using
    // `regex_syntax` (instead of a pure substring scan) means we correctly
    // ignore lookaround-shaped sequences inside character classes, escapes,
    // or regex comments. `regex_syntax` rejects lookaround, backreferences,
    // and `\Z` at parse time.
    let ast = regex_syntax::ast::parse::Parser::new()
        .parse(pattern)
        .map_err(|e| translate_parse_error(e.into()))?;
    check_ast_portability(pattern, &ast)?;

    // Translate the already-parsed AST to HIR (equivalent to
    // `regex_syntax::Parser::new().parse(pattern)` but without re-parsing)
    // for the belt-and-braces HIR walk below.
    let hir = regex_syntax::hir::translate::Translator::new()
        .translate(pattern, &ast)
        .map_err(|e| translate_parse_error(e.into()))?;
    check_hir_portability(&hir)
}

/// Walk the pattern AST and reject constructs that Rust's `regex` crate
/// accepts but that fall outside the spec's Python/Rust intersection dialect
/// (§2.2.5). These are invisible after AST→HIR translation, so they must be
/// caught here:
///
/// - `\p{...}` / `\P{...}` — Unicode property classes (Rust-only; Python
///   `re` rejects them as a bad escape)
/// - `(?<name>...)` — Rust's capture group name spelling (Python requires
///   `(?P<name>...)`)
/// - `[[:alpha:]]` — POSIX character classes (Python parses these as
///   ordinary bracket expressions with silently different semantics)
/// - `--` / `&&` / `~~` — character class set operators (Python parses
///   these as ordinary ranges/literals with silently different semantics)
/// - `[a[b]]` — nested character classes (the enabling construct behind the
///   set operators; Python treats the inner `[` as a literal)
/// - `(?U)` / `(?R)` — Rust-only inline flags (swap greed, CRLF mode);
///   Python rejects them as unknown flags. Negated Unicode mode (`-u`) and
///   bare global negation `(?-...)` are likewise Rust-only. Shared flags
///   `i`, `m`, `s`, `x` (and positive `u`) stay allowed.
/// - `a(?i)b` — bare inline flags anywhere but the start of the pattern.
///   Python 3.11+ rejects "global flags not at the start of the
///   expression" (3.6–3.10 applied them to the whole pattern, unlike
///   Rust's forward-only application). Consecutive leading flag groups
///   (`(?i)(?s)ab`) are accepted by both engines and stay allowed.
/// - `(?x)[a b]` — verbose mode combined with unescaped whitespace or `#`
///   inside a character class. Rust strips whitespace/comments inside
///   classes under `x`; Python's VERBOSE mode explicitly keeps them as
///   literals, so such patterns match differently with no error on either
///   side.
/// - `(?P<a.b>...)` — capture group names that are not valid Python
///   identifiers. `regex_syntax` also permits `.`, `[`, and `]` in names;
///   Python raises "bad character in group name".
/// - `\b{start}`, `\b{end}`, `\b{start-half}`, `\b{end-half}`, `\<`, `\>` —
///   Rust-only word boundary spellings (Python reads `\b{start}` as `\b`
///   followed by the literal `{start}`, silently diverging)
fn check_ast_portability(
    pattern: &str,
    ast: &regex_syntax::ast::Ast,
) -> Result<(), ExpressionError> {
    use regex_syntax::ast;

    struct PortabilityVisitor<'a> {
        /// The pattern source, for inspecting character class bodies.
        pattern: &'a str,
        /// True if any positive `x` (verbose / ignore-whitespace) flag
        /// appears anywhere in the pattern, bare or scoped. Scope is not
        /// tracked: a class outside an `(?x:...)` group's scope may be
        /// flagged too, which over-rejects but never diverges silently.
        uses_verbose: bool,
        /// Source spans of every bracketed character class.
        class_spans: Vec<ast::Span>,
        /// Source spans of every bare `(?flags)` group.
        bare_flag_spans: Vec<ast::Span>,
    }

    impl ast::Visitor for PortabilityVisitor<'_> {
        type Output = ();
        type Err = ExpressionError;

        fn finish(self) -> Result<(), ExpressionError> {
            // Bare `(?flags)` groups must form a contiguous run from the
            // start of the pattern. Python 3.11+ rejects global flags
            // anywhere else, and pre-3.11 Pythons applied them to the whole
            // pattern where Rust applies them only forward.
            let mut spans = self.bare_flag_spans;
            spans.sort_by_key(|s| s.start.offset);
            let mut expected = 0;
            for s in &spans {
                if s.start.offset != expected {
                    return Err(ExpressionError::new(
                        "Unsupported regex feature: bare inline flags not at the start \
                         of the pattern; use a scoped group like (?i:...)",
                    ));
                }
                expected = s.end.offset;
            }

            // Under verbose mode, Rust strips unescaped whitespace and `#`
            // comments inside character classes while Python keeps them as
            // literals — a silent divergence.
            if self.uses_verbose {
                for span in &self.class_spans {
                    let body = &self.pattern[span.start.offset..span.end.offset];
                    let mut escaped = false;
                    for c in body.chars() {
                        if escaped {
                            escaped = false;
                        } else if c == '\\' {
                            escaped = true;
                        } else if c.is_whitespace() || c == '#' {
                            return Err(ExpressionError::new(
                                "Unsupported regex feature: verbose mode (?x) with \
                                 whitespace or '#' in a character class; Python treats \
                                 them as literals",
                            ));
                        }
                    }
                }
            }
            Ok(())
        }

        fn visit_pre(&mut self, node: &ast::Ast) -> Result<(), ExpressionError> {
            match node {
                ast::Ast::ClassUnicode(_) => Err(unicode_property_error()),
                ast::Ast::ClassBracketed(c) => {
                    self.class_spans.push(c.span);
                    Ok(())
                }
                ast::Ast::Flags(set) => {
                    self.bare_flag_spans.push(set.span);
                    self.uses_verbose |= check_flags(&set.flags, false)?;
                    Ok(())
                }
                ast::Ast::Assertion(a) => check_assertion(&a.kind),
                ast::Ast::Group(g) => match &g.kind {
                    ast::GroupKind::CaptureName {
                        starts_with_p: false,
                        ..
                    } => Err(ExpressionError::new(
                        "Unsupported regex feature: (?<name>...) capture group; use (?P<name>...)",
                    )),
                    ast::GroupKind::CaptureName { name, .. } => check_capture_name(&name.name),
                    ast::GroupKind::NonCapturing(flags) => {
                        self.uses_verbose |= check_flags(flags, true)?;
                        Ok(())
                    }
                    _ => Ok(()),
                },
                _ => Ok(()),
            }
        }

        fn visit_class_set_item_pre(
            &mut self,
            item: &ast::ClassSetItem,
        ) -> Result<(), ExpressionError> {
            match item {
                ast::ClassSetItem::Ascii(c) => Err(ExpressionError::new(format!(
                    "Unsupported regex feature: POSIX character class [[:{}{}:]]",
                    if c.negated { "^" } else { "" },
                    posix_class_name(&c.kind),
                ))),
                ast::ClassSetItem::Unicode(_) => Err(unicode_property_error()),
                ast::ClassSetItem::Bracketed(_) => Err(ExpressionError::new(
                    "Unsupported regex feature: nested character class",
                )),
                _ => Ok(()),
            }
        }

        fn visit_class_set_binary_op_pre(
            &mut self,
            op: &ast::ClassSetBinaryOp,
        ) -> Result<(), ExpressionError> {
            let feature = match op.kind {
                ast::ClassSetBinaryOpKind::Difference => "character class difference --",
                ast::ClassSetBinaryOpKind::Intersection => "character class intersection &&",
                ast::ClassSetBinaryOpKind::SymmetricDifference => {
                    "character class symmetric difference ~~"
                }
            };
            Err(ExpressionError::new(format!(
                "Unsupported regex feature: {feature}"
            )))
        }
    }

    fn unicode_property_error() -> ExpressionError {
        ExpressionError::new("Unsupported regex feature: Unicode property class \\p{...}")
    }

    /// Validate an inline flag sequence against the Python/Rust
    /// intersection. `scoped` is true for `(?flags:...)` groups and false
    /// for bare `(?flags)` settings. Returns true if a positive `x`
    /// (verbose / ignore-whitespace) flag is present, so the caller can run
    /// the whitespace-in-class check.
    ///
    /// - `i`, `m`, `s`, `x` are shared by both engines and always allowed.
    /// - Positive `u` is allowed: both engines accept it and both default
    ///   to Unicode mode.
    /// - `U` (swap greed) and `R` (CRLF mode) are Rust-only; Python rejects
    ///   them as unknown flags.
    /// - Negation is only allowed in the scoped form `(?-imsx:...)`; Python
    ///   has no bare `(?-...)` global negation.
    /// - Negated `u` is Rust-only: Python never allows negating Unicode
    ///   mode, and in Rust it silently changes `\w`/`\d`/`.` semantics.
    fn check_flags(flags: &ast::Flags, scoped: bool) -> Result<bool, ExpressionError> {
        let mut negated = false;
        let mut verbose = false;
        for item in &flags.items {
            match &item.kind {
                ast::FlagsItemKind::Negation => {
                    if !scoped {
                        return Err(ExpressionError::new(
                            "Unsupported regex feature: global flag negation (?-...); \
                             use a scoped group (?-i:...)",
                        ));
                    }
                    negated = true;
                }
                ast::FlagsItemKind::Flag(f) => match f {
                    ast::Flag::CaseInsensitive
                    | ast::Flag::MultiLine
                    | ast::Flag::DotMatchesNewLine => {}
                    ast::Flag::IgnoreWhitespace => {
                        if !negated {
                            verbose = true;
                        }
                    }
                    ast::Flag::Unicode => {
                        if negated {
                            return Err(ExpressionError::new(
                                "Unsupported regex feature: negated Unicode flag -u",
                            ));
                        }
                    }
                    ast::Flag::SwapGreed => {
                        return Err(ExpressionError::new(
                            "Unsupported regex feature: inline flag U (swap greed)",
                        ));
                    }
                    ast::Flag::CRLF => {
                        return Err(ExpressionError::new(
                            "Unsupported regex feature: inline flag R (CRLF mode)",
                        ));
                    }
                },
            }
        }
        Ok(verbose)
    }

    /// Require capture group names to be valid Python identifiers.
    /// `regex_syntax` is more permissive: it allows `.`, `[`, and `]` in
    /// names (non-first position) and, more broadly, checks Rust's
    /// `char::is_alphanumeric` (general categories) rather than Python's
    /// XID_Start/XID_Continue — e.g. `²` (category No) is alphanumeric but
    /// not XID_Continue. CPython's `re` validates names with
    /// `str.isidentifier()` and raises "bad character in group name", so a
    /// pattern accepted here with a looser rule would fail outright under
    /// the Python implementation. The IDENT_START/IDENT_CONTINUE tables are
    /// generated from CPython's `str.isidentifier()` itself.
    fn check_capture_name(name: &str) -> Result<(), ExpressionError> {
        use super::unicode_tables::{in_table, IDENT_CONTINUE, IDENT_START};
        let valid = name.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                in_table(IDENT_START, c)
            } else {
                in_table(IDENT_CONTINUE, c)
            }
        });
        if valid {
            Ok(())
        } else {
            Err(ExpressionError::new(format!(
                "Unsupported regex feature: capture group name '{name}' is not a valid \
                 Python identifier"
            )))
        }
    }

    /// Reject the Rust-only word boundary spellings. Python's `re` does not
    /// error on these — it reads `\b{start}` as `\b` followed by the
    /// literal characters `{start}` — so they diverge silently.
    fn check_assertion(kind: &ast::AssertionKind) -> Result<(), ExpressionError> {
        use ast::AssertionKind::*;
        let spelling = match kind {
            WordBoundaryStart => "\\b{start}",
            WordBoundaryEnd => "\\b{end}",
            WordBoundaryStartAngle => "\\<",
            WordBoundaryEndAngle => "\\>",
            WordBoundaryStartHalf => "\\b{start-half}",
            WordBoundaryEndHalf => "\\b{end-half}",
            _ => return Ok(()),
        };
        Err(ExpressionError::new(format!(
            "Unsupported regex feature: word boundary assertion {spelling}"
        )))
    }

    fn posix_class_name(kind: &regex_syntax::ast::ClassAsciiKind) -> &'static str {
        use regex_syntax::ast::ClassAsciiKind::*;
        match kind {
            Alnum => "alnum",
            Alpha => "alpha",
            Ascii => "ascii",
            Blank => "blank",
            Cntrl => "cntrl",
            Digit => "digit",
            Graph => "graph",
            Lower => "lower",
            Print => "print",
            Punct => "punct",
            Space => "space",
            Upper => "upper",
            Word => "word",
            Xdigit => "xdigit",
        }
    }

    ast::visit(
        ast,
        PortabilityVisitor {
            pattern,
            uses_verbose: false,
            class_spans: Vec::new(),
            bare_flag_spans: Vec::new(),
        },
    )
}

/// Scan the pattern source for Rust-only escape sequences that
/// `regex_syntax` accepts but the spec forbids. Respects backslash-escape
/// parity so `\\z` (literal backslash + `z`) isn't mistaken for the `\z`
/// anchor.
fn reject_rust_only_features(pattern: &str) -> Result<(), ExpressionError> {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            // Consume the backslash; the next byte (if any) is escaped.
            let next = match bytes.get(i + 1) {
                Some(&b) => b,
                None => break, // trailing backslash — let regex_syntax diagnose
            };
            match next {
                b'z' => {
                    return Err(ExpressionError::new(
                        "Unsupported regex feature: end-of-string anchor \\z",
                    ));
                }
                // `\x{`, `\u{`, `\U{` are the Rust-only Unicode brace
                // escapes. Plain `\xHH`, `\uHHHH`, `\UHHHHHHHH` without
                // braces are portable and must still pass.
                b'x' | b'u' | b'U' => {
                    if matches!(bytes.get(i + 2), Some(b'{')) {
                        return Err(ExpressionError::new(format!(
                            "Unsupported regex feature: Unicode brace syntax \\{}{{...}}",
                            next as char
                        )));
                    }
                }
                _ => {}
            }
            // Skip the backslash *and* the escaped character so a later
            // `\` in this iteration's tail isn't mistaken for a fresh
            // escape introducer.
            i += 2;
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Translate a `regex_syntax` parse error into a stable user-facing message
/// that names the offending feature where possible.
fn translate_parse_error(err: regex_syntax::Error) -> ExpressionError {
    let msg = err.to_string();
    let lower = msg.to_lowercase();
    // Detect specific unsupported features and use canonical names. The
    // substring checks are on the formatted error message; message text is
    // stable given the regex-syntax version pin in Cargo.toml.
    let feature = if lower.contains("look-around") || lower.contains("lookaround") {
        // regex-syntax reports all four lookaround variants as "look-around";
        // distinguish via the pattern flavor surfaced in the message.
        if lower.contains("negative lookahead") || lower.contains("(?!") {
            "negative lookahead"
        } else if lower.contains("positive lookahead") || lower.contains("(?=") {
            "lookahead"
        } else if lower.contains("negative lookbehind") || lower.contains("(?<!") {
            "negative lookbehind"
        } else if lower.contains("positive lookbehind") || lower.contains("(?<=") {
            "lookbehind"
        } else {
            "look-around"
        }
    } else if lower.contains("backreference") || lower.contains("back reference") {
        "backreferences"
    } else if lower.contains("unrecognized escape") && msg.contains("\\Z") {
        // `\Z` is a Python-style end-of-string anchor. The Rust regex
        // grammar does not support it and reports it as an unknown escape.
        // (The complementary `\z` case is rejected earlier by
        // `reject_rust_only_features`.)
        "end-of-string anchor \\Z"
    } else {
        return ExpressionError::new(format!("Invalid regex pattern: {msg}"));
    };
    ExpressionError::new(format!("Unsupported regex feature: {feature}"))
}

/// Walk the parsed HIR and reject forbidden constructs that `regex_syntax`
/// itself accepts.
///
/// `regex_syntax` already rejects lookaround, backreferences, and `\Z`/`\z`
/// at parse time, so in practice this walker is a belt-and-braces guard for
/// any future grammar additions that expose these constructs via HIR.
///
/// **Known divergence — `$` (`Look::End`):** without MULTILINE, Python's
/// `$` matches at end of string *or immediately before a trailing newline*
/// (`re.search(r"a$", "a\n")` matches), while Rust's `$` is end-of-haystack
/// only (Python's `$` is closer to Rust's `(?:\n?\z)`). The spec's
/// intersection dialect allows `$`, so it stays accepted here even though
/// results differ on newline-terminated input. This is the same divergence
/// family as `\Z` vs `\z`, both of which are rejected outright.
fn check_hir_portability(hir: &regex_syntax::hir::Hir) -> Result<(), ExpressionError> {
    use regex_syntax::hir::{HirKind, Look};
    match hir.kind() {
        HirKind::Look(l) => match l {
            // These anchors are in the spec's allowed subset.
            Look::Start
            | Look::End
            | Look::StartLF
            | Look::EndLF
            | Look::StartCRLF
            | Look::EndCRLF
            | Look::WordAscii
            | Look::WordAsciiNegate
            | Look::WordUnicode
            | Look::WordUnicodeNegate => Ok(()),
            // Rust-only word boundary spellings (`\b{start}`, `\<`, ...)
            // lower to these. They are rejected with precise messages by
            // `check_ast_portability`; this arm is the belt-and-braces
            // backstop.
            Look::WordStartAscii
            | Look::WordEndAscii
            | Look::WordStartUnicode
            | Look::WordEndUnicode
            | Look::WordStartHalfAscii
            | Look::WordEndHalfAscii
            | Look::WordStartHalfUnicode
            | Look::WordEndHalfUnicode => Err(ExpressionError::new(
                "Unsupported regex feature: Rust-only word boundary assertion",
            )),
        },
        HirKind::Capture(c) => check_hir_portability(&c.sub),
        HirKind::Repetition(r) => check_hir_portability(&r.sub),
        HirKind::Concat(parts) | HirKind::Alternation(parts) => {
            for p in parts {
                check_hir_portability(p)?;
            }
            Ok(())
        }
        HirKind::Empty | HirKind::Literal(_) | HirKind::Class(_) => Ok(()),
    }
}

/// Compile limit for the longest-match regex built by
/// [`EmptyMatchCheck::non_empty_match_at`], the same NFA size limit the
/// default [`EvalContext::get_or_compile_regex`] uses.
const LONGEST_MATCH_SIZE_LIMIT: usize = 1 << 20;

/// Call `f` on each match of `re` in `s`, at most `limit` times, stepping
/// through matches the way CPython's `re.split`, `re.findall` and `re.sub`
/// do. `func` and `pattern` are the calling function's name and the
/// pattern `re` was compiled from, for the divergence check and its error.
///
/// Each search resumes where the previous match ended. An empty match is
/// allowed anywhere except where the previous match was also empty and
/// ended, so an empty match right after a non-empty match counts:
/// `re_split('a,b', ',?')` is `["", "a", "", "b", ""]`. The `regex` crate's
/// own iterators skip that empty match, and that is why they cannot be used.
///
/// Where an empty match would repeat, CPython backtracks into the pattern
/// for a non-empty match at the same position, and if there is none,
/// searches again one character on. The `regex` crate reports only the
/// pattern's preferred match at a position, so it cannot find that
/// non-empty match. Instead, [`EmptyMatchCheck`] checks whether one exists,
/// and this returns an error if it does (`a??`, `|a`), because the result
/// would differ from CPython's. Otherwise it searches again one character
/// on, as CPython does.
fn for_each_python_match<'h>(
    func: &str,
    re: &regex::Regex,
    pattern: &str,
    s: &'h str,
    limit: usize,
    mut f: impl FnMut(&regex::Captures<'h>) -> Result<(), ExpressionError>,
) -> Result<(), ExpressionError> {
    let mut pos = 0;
    // Where the previous match ended, if it was empty.
    let mut empty_end = None;
    let mut found = 0;
    let mut check = EmptyMatchCheck::Unknown;
    while found < limit {
        let Some(caps) = re.captures_at(s, pos) else {
            break;
        };
        let m = caps.get(0).expect("group 0 always takes part in a match");
        if m.is_empty() && empty_end == Some(m.start()) {
            if check.non_empty_match_at(func, pattern, s, pos)? {
                return Err(ExpressionError::new(format!(
                    "{func}(): at index {} the pattern prefers an empty match where a \
                     non-empty match is also possible, and Python's re would take the \
                     non-empty one. Make the empty match the pattern's last choice, for \
                     example 'a?' instead of 'a??', or 'a|' instead of '|a'",
                    s[..pos].chars().count()
                )));
            }
            let Some(c) = s[pos..].chars().next() else {
                break;
            };
            pos += c.len_utf8();
            empty_end = None;
            continue;
        }
        f(&caps)?;
        found += 1;
        empty_end = m.is_empty().then_some(m.end());
        pos = m.end();
    }
    Ok(())
}

/// Answers, for one call of [`for_each_python_match`], whether the pattern
/// has a non-empty match at a position where its preferred match is empty.
enum EmptyMatchCheck {
    /// Not asked yet.
    Unknown,
    /// The pattern cannot prefer an empty match where a non-empty one is
    /// possible (see [`can_prefer_empty_match`]), so the answer is always
    /// no.
    NotNeeded,
    /// The pattern compiled for an anchored longest-match search.
    Longest(regex_automata::meta::Regex),
}

impl EmptyMatchCheck {
    /// Whether `pattern` has a non-empty match that starts at byte offset
    /// `pos` of `s`, whatever its priority.
    ///
    /// The first call decides from the pattern's structure whether the
    /// question can have a yes answer at all, and if it can, compiles the
    /// pattern for the search. The `regex` crate has no anchored search at
    /// an offset and no longest-match mode, so the search uses
    /// `regex_automata`, the engine `regex` is built on. With
    /// [`MatchKind::All`](regex_automata::MatchKind::All), a search reports
    /// the last possible match end, so an anchored search finds the longest
    /// match that starts at `pos`.
    fn non_empty_match_at(
        &mut self,
        func: &str,
        pattern: &str,
        s: &str,
        pos: usize,
    ) -> Result<bool, ExpressionError> {
        use regex_automata::{meta, Anchored, Input, MatchKind};
        if let EmptyMatchCheck::Unknown = self {
            let compile_error = |e: &dyn std::fmt::Display| {
                ExpressionError::new(format!(
                    "{func}(): could not compile the pattern to check its empty matches \
                     against Python's re: {e}"
                ))
            };
            let hir = regex_syntax::Parser::new()
                .parse(pattern)
                .map_err(|e| compile_error(&e))?;
            *self = if can_prefer_empty_match(&hir) {
                #[cfg(test)]
                tests::LONGEST_COMPILES.with(|n| n.set(n.get() + 1));
                EmptyMatchCheck::Longest(
                    meta::Regex::builder()
                        .configure(
                            meta::Config::new()
                                .match_kind(MatchKind::All)
                                .nfa_size_limit(Some(LONGEST_MATCH_SIZE_LIMIT)),
                        )
                        .build_from_hir(&hir)
                        .map_err(|e| match e.size_limit() {
                            Some(limit) => compile_error(&format_args!(
                                "the compiled pattern exceeds the size limit of {limit} bytes"
                            )),
                            None => compile_error(&e),
                        })?,
                )
            } else {
                EmptyMatchCheck::NotNeeded
            };
        }
        match self {
            EmptyMatchCheck::Longest(re) => {
                let input = Input::new(s).range(pos..).anchored(Anchored::Yes);
                Ok(re.search(&input).is_some_and(|m| m.end() > pos))
            }
            _ => Ok(false),
        }
    }
}

/// Whether a regex can have an empty preferred match at a position where a
/// non-empty match is also possible. If this is `false`, the preferred match
/// at a position is non-empty whenever any non-empty match exists there.
///
/// Leftmost-first matching takes the first path in priority order that
/// succeeds. Take a preferred path that matches empty and a path that
/// matches non-empty, and look at the first choice where they part:
///
/// - Alternation: the preferred path took an earlier branch, which matched
///   empty. If the other path's later branch can only match empty too, both
///   paths are in the same place after the alternation, so compare them
///   again from there.
/// - Greedy repetition: the preferred path took one more iteration, which
///   matched empty, so again both paths are in the same place afterwards.
/// - Lazy repetition: the preferred path took fewer iterations. If the
///   body can only match empty, the extra iterations leave both paths in
///   the same place.
///
/// So the answer can be `true` only for an alternation with a branch that
/// can match empty before a branch that can match non-empty (`|a`, `a*|b`,
/// `\b|a`), or a lazy repetition that can match empty and whose body can
/// match non-empty (`a??`, `a*?`). The check is conservative: `(?:\b|a)`
/// counts, though at most positions the two engines agree on it.
fn can_prefer_empty_match(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::HirKind;
    let can_be_empty = |h: &regex_syntax::hir::Hir| h.properties().minimum_len() == Some(0);
    let can_be_non_empty = |h: &regex_syntax::hir::Hir| h.properties().maximum_len() != Some(0);
    match hir.kind() {
        HirKind::Repetition(r) => {
            (!r.greedy && can_be_empty(hir) && can_be_non_empty(&r.sub))
                || can_prefer_empty_match(&r.sub)
        }
        HirKind::Alternation(branches) => {
            let empty_then_non_empty = branches
                .iter()
                .enumerate()
                .any(|(i, b)| can_be_empty(b) && branches[i + 1..].iter().any(can_be_non_empty));
            empty_then_non_empty || branches.iter().any(can_prefer_empty_match)
        }
        HirKind::Capture(c) => can_prefer_empty_match(&c.sub),
        HirKind::Concat(parts) => parts.iter().any(can_prefer_empty_match),
        HirKind::Empty | HirKind::Literal(_) | HirKind::Class(_) | HirKind::Look(_) => false,
    }
}

#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub fn re_escape_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let s = match &a[0] {
        ExprValue::String(s) => s.clone(),
        _ => return Err(ExpressionError::new("re_escape() requires string")),
    };
    ctx.count_string_ops(s.len())?;
    Ok(ExprValue::String(regex::escape(&s)))
}

#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub fn re_match_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let (s, pat) = get_two_strings(a, "re_match")?;
    ctx.count_string_ops(s.len())?;
    validate_regex_pattern(&pat)?;
    let re = ctx.get_or_compile_regex(&format!("^(?:{})", pat))?;
    match re.captures(&s) {
        None => Ok(ExprValue::Null),
        Some(caps) => {
            let groups: Vec<ExprValue> = (0..caps.len())
                .map(|i| {
                    ExprValue::String(
                        caps.get(i)
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            Ok(ExprValue::make_list_checked(ctx, groups, ExprType::STRING)?)
        }
    }
}

#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub fn re_search_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let (s, pat) = get_two_strings(a, "re_search")?;
    ctx.count_string_ops(s.len())?;
    validate_regex_pattern(&pat)?;
    let re = ctx.get_or_compile_regex(&pat)?;
    match re.captures(&s) {
        None => Ok(ExprValue::Null),
        Some(caps) => {
            let groups: Vec<ExprValue> = (0..caps.len())
                .map(|i| {
                    ExprValue::String(
                        caps.get(i)
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            Ok(ExprValue::make_list_checked(ctx, groups, ExprType::STRING)?)
        }
    }
}

#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub fn re_findall_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    let (s, pat) = get_two_strings(a, "re_findall")?;
    ctx.count_string_ops(s.len())?;
    validate_regex_pattern(&pat)?;
    let re = ctx.get_or_compile_regex(&pat)?;
    let num_groups = re.captures_len() - 1;
    // A group that does not take part in a match is "", as in CPython's
    // re.findall.
    let group = |c: &regex::Captures<'_>, i: usize| {
        ExprValue::String(c.get(i).map(|m| m.as_str().to_string()).unwrap_or_default())
    };
    let mut matches = BudgetedVec::new();
    for_each_python_match("re_findall", &re, &pat, &s, usize::MAX, |c| {
        let value = match num_groups {
            0 => group(c, 0),
            1 => group(c, 1),
            // Build inner lists without per-element ctx-checking: each
            // row has a bounded number of groups (num_groups, fixed by
            // the pattern), so the inner allocations are small. The outer
            // list is the one that scales with the input; it is checked
            // as it grows.
            _ => ExprValue::make_list(
                (1..=num_groups).map(|i| group(c, i)).collect(),
                ExprType::STRING,
            )?,
        };
        matches.push(ctx, value)
    })?;
    let elem_type = if num_groups > 1 {
        ExprType::list(ExprType::STRING)
    } else {
        ExprType::STRING
    };
    ExprValue::make_list_checked(ctx, matches.into_vec(), elem_type)
}

#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub fn re_replace_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    if a.len() != 3 {
        return Err(ExpressionError::new("re_replace() takes 3 arguments"));
    }
    let s = match &a[0] {
        ExprValue::String(s) => s.clone(),
        _ => return Err(ExpressionError::new("re_replace() requires strings")),
    };
    ctx.count_string_ops(s.len())?;
    let pat = match &a[1] {
        ExprValue::String(s) => s.clone(),
        _ => return Err(ExpressionError::new("re_replace() requires strings")),
    };
    let repl = match &a[2] {
        ExprValue::String(s) => s.clone(),
        _ => return Err(ExpressionError::new("re_replace() requires strings")),
    };
    validate_regex_pattern(&pat)?;
    validate_regex_replacement(&repl)?;
    let re = ctx.get_or_compile_regex(&pat)?;
    // The replacement is literal text (group references were rejected
    // above). Check the growing result against the memory limit, since an
    // empty match at every character inserts `repl` len(s) + 1 times.
    let mut result = String::new();
    let mut last = 0;
    for_each_python_match("re_sub", &re, &pat, &s, usize::MAX, |c| {
        let m = c.get(0).expect("group 0 always takes part in a match");
        ctx.check_memory(result.len() + (m.start() - last) + repl.len())?;
        result.push_str(&s[last..m.start()]);
        result.push_str(&repl);
        last = m.end();
        Ok(())
    })?;
    result.push_str(&s[last..]);
    Ok(ExprValue::String(result))
}

fn validate_regex_replacement(repl: &str) -> Result<(), ExpressionError> {
    let bytes = repl.as_bytes();
    for i in 0..bytes.len() {
        // Check for \1-\9 and \g<...> backreferences
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            if bytes[i + 1].is_ascii_digit() {
                return Err(ExpressionError::new(
                    "Group references in replacement strings are not supported",
                ));
            }
            if bytes[i + 1] == b'g' && i + 2 < bytes.len() && bytes[i + 2] == b'<' {
                return Err(ExpressionError::new(
                    "Group references in replacement strings are not supported",
                ));
            }
        }
        // Check for $1, $2, ${1}, ${name} references
        if bytes[i] == b'$'
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_digit() || bytes[i + 1] == b'{')
        {
            return Err(ExpressionError::new(
                "Group references in replacement strings are not supported",
            ));
        }
    }
    Ok(())
}

#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub fn re_split_fn(ctx: Ctx, a: &[ExprValue]) -> R {
    if a.len() < 2 || a.len() > 3 {
        return Err(ExpressionError::new("re_split() takes 2-3 arguments"));
    }
    let (s, pat) = get_two_strings(a, "re_split")?;
    ctx.count_string_ops(s.len())?;
    validate_regex_pattern(&pat)?;
    let maxsplit = a.get(2).and_then(|v| match v {
        ExprValue::Int(n) => Some(*n),
        _ => None,
    });
    let re = ctx.get_or_compile_regex(&pat)?;
    // Python re.split semantics (unlike str.split): negative maxsplit means
    // no splits at all; 0 means unlimited.
    // The `i64 -> usize` conversion saturates rather than casting: on 32-bit
    // targets a plain `as` cast would wrap a large maxsplit down to a small one
    // (or to 0) and silently split too few times. See `maxsplit_arg` in
    // `functions/string.rs`.
    let limit = match maxsplit {
        Some(n) if n < 0 => 0,
        Some(n) if n > 0 => usize::try_from(n).unwrap_or(usize::MAX),
        _ => usize::MAX,
    };
    let mut parts = BudgetedVec::new();
    let mut last = 0;
    for_each_python_match("re_split", &re, &pat, &s, limit, |c| {
        let m = c.get(0).expect("group 0 always takes part in a match");
        parts.push(ctx, ExprValue::String(s[last..m.start()].to_string()))?;
        // Capture groups go between the pieces, as in CPython. A group
        // that did not take part in the match (CPython's None) is left out.
        for g in c.iter().skip(1).flatten() {
            parts.push(ctx, ExprValue::String(g.as_str().to_string()))?;
        }
        last = m.end();
        Ok(())
    })?;
    parts.push(ctx, ExprValue::String(s[last..].to_string()))?;
    ExprValue::make_list_checked(ctx, parts.into_vec(), ExprType::STRING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_mapping::PathFormat;
    use std::cell::{Cell, RefCell};

    thread_local! {
        /// How many times this thread compiled a longest-match regex.
        pub(super) static LONGEST_COMPILES: Cell<usize> = const { Cell::new(0) };
    }

    fn longest_compiles() -> usize {
        LONGEST_COMPILES.with(Cell::get)
    }

    /// Records every memory check. `regex_size_limit` stands in for a host
    /// whose `get_or_compile_regex` compiles with a larger limit than the
    /// default.
    #[derive(Default)]
    struct TestContext {
        checks: RefCell<Vec<usize>>,
        regex_size_limit: Option<usize>,
    }

    impl EvalContext for TestContext {
        fn path_format(&self) -> PathFormat {
            PathFormat::host()
        }
        fn count_op(&mut self) -> Result<(), ExpressionError> {
            Ok(())
        }
        fn count_ops(&mut self, _n: usize) -> Result<(), ExpressionError> {
            Ok(())
        }
        fn count_string_ops(&mut self, _len: usize) -> Result<(), ExpressionError> {
            Ok(())
        }
        fn check_memory(&self, bytes: usize) -> Result<(), ExpressionError> {
            self.checks.borrow_mut().push(bytes);
            Ok(())
        }
        fn get_or_compile_regex(&mut self, pattern: &str) -> Result<regex::Regex, ExpressionError> {
            regex::RegexBuilder::new(pattern)
                .size_limit(self.regex_size_limit.unwrap_or(1 << 20))
                .build()
                .map_err(|e| ExpressionError::new(format!("Invalid regex: {e}")))
        }
    }

    fn strs(args: &[&str]) -> Vec<ExprValue> {
        args.iter()
            .map(|s| ExprValue::String(s.to_string()))
            .collect()
    }

    #[test]
    fn can_prefer_empty_match_cases() {
        let cases = [
            // Never prefers an empty match over a non-empty one.
            (",", false),
            (",?", false),
            ("x*", false),
            ("a|", false),
            ("(?:a|)", false),
            ("a+?", false),
            ("(?:a?)*", false),
            (r"\b", false),
            (r"(?:\w{0,12}|)x?", false),
            ("(?:|)a", false),
            (r"(?:\b)??", false),
            // An empty-capable branch before a non-empty-capable one.
            ("|a", true),
            ("a*|b", true),
            (r"\b|a", true),
            ("(?:(?:|a)b)", true),
            ("x(|a)", true),
            // Either rule inside a later branch or a repetition.
            ("b|a??", true),
            ("b|(?:|a)", true),
            ("(?:b|a*?)+", true),
            // A lazy repetition that can match empty.
            ("a??", true),
            ("a*?", true),
            ("(?:a?)+?", true),
            ("(?:b*?)+", true),
        ];
        for (pattern, want) in cases {
            let hir = regex_syntax::Parser::new().parse(pattern).unwrap();
            assert_eq!(can_prefer_empty_match(&hir), want, "pattern {pattern:?}");
        }
    }

    #[test]
    fn only_flagged_patterns_run_the_longest_match_search() {
        let mut ctx = TestContext::default();
        let before = longest_compiles();
        // Each of these repeats an empty match at every position.
        for pattern in ["x*", ",?", "(?:a|)", r"\b", r"(?:\w{0,12}|)x?"] {
            re_split_fn(&mut ctx, &strs(&["a,b c", pattern])).unwrap();
            re_findall_fn(&mut ctx, &strs(&["a,b c", pattern])).unwrap();
            re_replace_fn(&mut ctx, &strs(&["a,b c", pattern, "-"])).unwrap();
        }
        assert_eq!(longest_compiles(), before);
        // A flagged pattern compiles it once per call, on the first repeat.
        let v = re_findall_fn(&mut ctx, &strs(&["bb", "a??"])).unwrap();
        assert_eq!(v.list_len(), Some(3));
        assert_eq!(longest_compiles(), before + 1);
        // A flagged pattern that cannot match empty never asks.
        re_findall_fn(&mut ctx, &strs(&["abb", "a??b"])).unwrap();
        re_findall_fn(&mut ctx, &strs(&["abb", "(?:|a)b"])).unwrap();
        assert_eq!(longest_compiles(), before + 1);
    }

    #[test]
    fn re_split_checks_memory_for_each_piece() {
        // ['', 'a', 'a', 'a', 'a', ''] is six pushes, then the final list.
        let mut ctx = TestContext::default();
        let v = re_split_fn(&mut ctx, &strs(&["aaaa", "x*"])).unwrap();
        assert_eq!(v.list_len(), Some(6));
        assert_eq!(ctx.checks.borrow().len(), 7);
        // Groups are pushed (and checked) one at a time too.
        let mut ctx = TestContext::default();
        let v = re_split_fn(&mut ctx, &strs(&["a,b", "(,)"])).unwrap();
        assert_eq!(v.list_len(), Some(3));
        assert_eq!(ctx.checks.borrow().len(), 4);
    }

    #[test]
    fn re_findall_checks_memory_for_each_match() {
        // ['', '', '', '', ''] is five pushes, then the final list.
        let mut ctx = TestContext::default();
        let v = re_findall_fn(&mut ctx, &strs(&["aaaa", "x*"])).unwrap();
        assert_eq!(v.list_len(), Some(5));
        assert_eq!(ctx.checks.borrow().len(), 6);
    }

    #[test]
    fn re_sub_checks_memory_before_each_replacement() {
        // Matches at 0, 1 and 2 give "--a--b--": the projected lengths are
        // 0+0+2, 2+1+2 and 5+1+2.
        let mut ctx = TestContext::default();
        let v = re_replace_fn(&mut ctx, &strs(&["ab", "x*", "--"])).unwrap();
        assert_eq!(v.to_display_string(), "--a--b--");
        assert_eq!(*ctx.checks.borrow(), vec![2, 5, 8]);
    }

    #[test]
    fn divergence_check_compile_failure_names_the_check() {
        // A host that compiles with a larger size limit accepts the pattern,
        // but the divergence check compiles it with the default limit.
        let mut ctx = TestContext {
            regex_size_limit: Some(1 << 28),
            ..TestContext::default()
        };
        let err = re_findall_fn(&mut ctx, &strs(&["b", r"x*|\w{100}"])).unwrap_err();
        let msg = err.to_string();
        assert_eq!(
            msg,
            "re_findall(): could not compile the pattern to check its empty matches \
             against Python's re: the compiled pattern exceeds the size limit of 1048576 bytes"
        );
    }
}
