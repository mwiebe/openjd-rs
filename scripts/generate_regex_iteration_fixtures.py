#!/usr/bin/env python3
# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Generate the CPython fixture table for regex match iteration.

re_split, re_findall and re_sub follow CPython's re.split, re.findall and
re.sub in how they step through matches, including empty matches (see
"Regex match iteration" in specs/expr/function-library.md). This script runs
every (pattern, haystack[, maxsplit]) combination below through CPython and
writes the results to

    crates/openjd-expr/tests/integration/regex_iteration_fixtures.rs

which tests/integration/test_regex_iteration.rs compares the Rust
implementation against. The table is checked in, so the tests do not need
Python. Regenerate it with:

    python3 scripts/generate_regex_iteration_fixtures.py

Where an empty match would repeat, CPython backtracks into the pattern for
a non-empty match at the same position, and the Rust implementation instead
searches again one character on (the accepted divergence for lazy and
empty-first patterns such as `a??` and `|a`, see the spec). For those
cases the table also records the Rust implementation's documented result,
computed here by running the same loop on CPython's own preferred matches
(`pattern.search(s, pos)`), so a change to the divergent results is caught
too. Every other case must match CPython exactly; for re_split, a group
that did not take part (CPython's None) is dropped.

Combinations whose results differ between Python's `re` and Rust's `regex`
for reasons other than match iteration are left out:
- `$` on a haystack containing a newline (the accepted `$` divergence in
  specs/expr/function-library.md);
- `\\B` on the empty haystack (CPython before 3.14 never matches `\\B` on
  an empty string).
"""

import platform
import re
import sys

OUT_PATH = "crates/openjd-expr/tests/integration/regex_iteration_fixtures.rs"

PATTERNS = [
    # Non-empty-only matches (behaviour unchanged from Rust's own iterators).
    ",",
    ",+",
    r"\s+",
    r"\d",
    # Greedy patterns that can match empty.
    ",?",
    ",*",
    "x*",
    "a*",
    "a?",
    r"\d*",
    r"\s*",
    r"\W*",
    "(?:a|)",
    "(?:,|)",
    "a|",
    "(?i)A*",
    # Zero-width patterns.
    r"\b",
    r"\B",
    "^",
    "$",
    "(?m)^",
    "(?m)$",
    "(?:)",
    # Capture groups, some of which do not take part in every match.
    "()",
    "(,)",
    "(,)?",
    "(a*)",
    r"(\d)*",
    "(a)|b",
    "(a)?",
    "(,)|(;)",
    "(-)?,",
    "(-?),",
    # Lazy and empty-first patterns: the top-priority match can be empty
    # where a non-empty match is also possible.
    "a+?",
    "a*?",
    "a??",
    "|a",
    "a*|b",
    "b*?a?",
    "b|a??",
    "a|x*",
    "(a)(b)",
    r"(,)(\s*)",
]

HAYSTACKS = [
    "",
    "a",
    "aa",
    "aaa",
    "abc",
    "bab",
    "baaac",
    "a,b,c",
    ",a,,b,",
    ",,",
    ",a,",
    "a b  c",
    "x1y22z",
    "ab;cd,ef",
    "a-,b,c-,",
    "\u00e9,\u00fc",
    "\u65e5\u672c,\u8a9e",
    "a\nb\n",
    "\n",
]

MAXSPLITS = [None, 0, 1, 2, 3, 100, -1, -5]


def skip(pattern, haystack):
    if "$" in pattern and "\n" in haystack:
        return True
    if pattern == r"\B" and haystack == "":
        return True
    return False


def rust_str(s):
    out = []
    for ch in s:
        cp = ord(ch)
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif cp < 0x20 or 0x7F <= cp < 0xA0:
            out.append("\\u{%x}" % cp)
        else:
            out.append(ch)
    return '"' + "".join(out) + '"'


def opt_str(v):
    return "None" if v is None else "Some(%s)" % rust_str(v)


def our_matches(compiled, haystack, limit):
    """The Rust implementation's match loop, on CPython's preferred matches."""
    out = []
    pos = 0
    empty_end = None
    while len(out) < limit:
        m = compiled.search(haystack, pos)
        if m is None:
            break
        if m.start() == m.end() and empty_end == m.start():
            if pos >= len(haystack):
                break
            pos += 1
            empty_end = None
            continue
        out.append(m)
        empty_end = m.end() if m.start() == m.end() else None
        pos = m.end()
    return out


def our_split(compiled, haystack, maxsplit):
    if maxsplit is None or maxsplit == 0:
        limit = float("inf")
    elif maxsplit < 0:
        limit = 0
    else:
        limit = maxsplit
    out = []
    last = 0
    for m in our_matches(compiled, haystack, limit):
        out.append(haystack[last : m.start()])
        out.extend(g for g in m.groups() if g is not None)
        last = m.end()
    out.append(haystack[last:])
    return out


def our_findall(compiled, haystack):
    def group(m, i):
        g = m.group(i)
        return "" if g is None else g

    out = []
    for m in our_matches(compiled, haystack, float("inf")):
        if compiled.groups == 0:
            out.append(m.group(0))
        elif compiled.groups == 1:
            out.append(group(m, 1))
        else:
            out.append(tuple(group(m, i) for i in range(1, compiled.groups + 1)))
    return out


def our_sub(compiled, haystack, repl):
    out = []
    last = 0
    for m in our_matches(compiled, haystack, float("inf")):
        out.append(haystack[last : m.start()])
        out.append(repl)
        last = m.end()
    out.append(haystack[last:])
    return "".join(out)


def findall_expr(compiled, found):
    if compiled.groups <= 1:
        return "FindAll::Flat(&[%s])" % ", ".join(rust_str(x) for x in found)
    return "FindAll::Groups(&[%s])" % ", ".join(
        "&[%s]" % ", ".join(rust_str(g) for g in row) for row in found
    )


def main():
    split_rows = []
    findall_rows = []
    sub_rows = []
    for pattern in PATTERNS:
        compiled = re.compile(pattern)
        for haystack in HAYSTACKS:
            if skip(pattern, haystack):
                continue
            for maxsplit in MAXSPLITS:
                if maxsplit is None:
                    result = compiled.split(haystack)
                else:
                    result = compiled.split(haystack, maxsplit=maxsplit)
                ours = our_split(compiled, haystack, maxsplit)
                same = ours == [x for x in result if x is not None]
                split_rows.append(
                    "    SplitCase { s: %s, pattern: %s, maxsplit: %s, expected: &[%s], divergent: %s },"
                    % (
                        rust_str(haystack),
                        rust_str(pattern),
                        "None" if maxsplit is None else "Some(%d)" % maxsplit,
                        ", ".join(opt_str(x) for x in result),
                        "None" if same else "Some(&[%s])" % ", ".join(rust_str(x) for x in ours),
                    )
                )
            found = compiled.findall(haystack)
            ours = our_findall(compiled, haystack)
            findall_rows.append(
                "    FindAllCase { s: %s, pattern: %s, expected: %s, divergent: %s },"
                % (
                    rust_str(haystack),
                    rust_str(pattern),
                    findall_expr(compiled, found),
                    "None" if ours == found else "Some(%s)" % findall_expr(compiled, ours),
                )
            )
            subbed = compiled.sub("-", haystack)
            ours = our_sub(compiled, haystack, "-")
            sub_rows.append(
                "    SubCase { s: %s, pattern: %s, expected: %s, divergent: %s },"
                % (
                    rust_str(haystack),
                    rust_str(pattern),
                    rust_str(subbed),
                    "None" if ours == subbed else "Some(%s)" % rust_str(ours),
                )
            )

    version = "%s %s" % (platform.python_implementation(), platform.python_version())
    lines = [
        "// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.",
        "// Copyright by contributors to this project.",
        "// SPDX-License-Identifier: (Apache-2.0 OR MIT)",
        "",
        "// @generated by scripts/generate_regex_iteration_fixtures.py from %s." % version,
        "// Do not edit by hand; rerun the script instead.",
        "",
        "//! CPython results for re.split, re.findall and re.sub (replacement `-`).",
        "//!",
        "//! `expected` is CPython's output verbatim, so a re.split group that did",
        "//! not take part in a match is `None`. `divergent` is the Rust",
        "//! implementation's documented result where it differs from CPython (lazy",
        "//! and empty-first patterns), and `None` where it must match CPython.",
        "",
        "use super::{FindAll, FindAllCase, SplitCase, SubCase};",
        "",
        "pub(super) const PYTHON_VERSION: &str = %s;" % rust_str(version),
        "",
        "pub(super) const SPLIT_CASES: &[SplitCase] = &[",
        *split_rows,
        "];",
        "",
        "pub(super) const FINDALL_CASES: &[FindAllCase] = &[",
        *findall_rows,
        "];",
        "",
        "pub(super) const SUB_CASES: &[SubCase] = &[",
        *sub_rows,
        "];",
        "",
    ]
    with open(OUT_PATH, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines))
    divergent = sum("divergent: Some" in r for r in split_rows + findall_rows + sub_rows)
    print(
        "wrote %s: %d split, %d findall, %d sub cases, %d divergent (%s)"
        % (OUT_PATH, len(split_rows), len(findall_rows), len(sub_rows), divergent, version),
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
