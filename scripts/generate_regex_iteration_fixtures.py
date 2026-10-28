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

Each case also records whether CPython took a non-empty match at the
position right after an empty match. CPython does that by backtracking past
the empty match the pattern prefers there (for example `a??`, `|a`); the
Rust implementation cannot reproduce that and reports an error instead, so
those cases expect an error. The flag is derived from CPython's own match
list: consecutive matches m1, m2 with m1 empty and m2.start() == m1.end().
For re_split only the first maxsplit matches count, because the search stops
there.

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


def diverges(matches):
    for m1, m2 in zip(matches, matches[1:]):
        if m1.start() == m1.end() and m2.start() == m1.end():
            return True
    return False


def main():
    split_rows = []
    findall_rows = []
    sub_rows = []
    for pattern in PATTERNS:
        compiled = re.compile(pattern)
        for haystack in HAYSTACKS:
            if skip(pattern, haystack):
                continue
            matches = list(compiled.finditer(haystack))
            for maxsplit in MAXSPLITS:
                if maxsplit is None:
                    result = compiled.split(haystack)
                    counted = matches
                else:
                    result = compiled.split(haystack, maxsplit=maxsplit)
                    if maxsplit < 0:
                        counted = []
                    elif maxsplit == 0:
                        counted = matches
                    else:
                        counted = matches[:maxsplit]
                split_rows.append(
                    "    SplitCase { s: %s, pattern: %s, maxsplit: %s, error: %s, expected: &[%s] },"
                    % (
                        rust_str(haystack),
                        rust_str(pattern),
                        "None" if maxsplit is None else "Some(%d)" % maxsplit,
                        "true" if diverges(counted) else "false",
                        ", ".join(opt_str(x) for x in result),
                    )
                )
            found = compiled.findall(haystack)
            if compiled.groups <= 1:
                expected = "FindAll::Flat(&[%s])" % ", ".join(rust_str(x) for x in found)
            else:
                expected = "FindAll::Groups(&[%s])" % ", ".join(
                    "&[%s]" % ", ".join(rust_str(g) for g in row) for row in found
                )
            findall_rows.append(
                "    FindAllCase { s: %s, pattern: %s, error: %s, expected: %s },"
                % (
                    rust_str(haystack),
                    rust_str(pattern),
                    "true" if diverges(matches) else "false",
                    expected,
                )
            )
            sub_rows.append(
                "    SubCase { s: %s, pattern: %s, error: %s, expected: %s },"
                % (
                    rust_str(haystack),
                    rust_str(pattern),
                    "true" if diverges(matches) else "false",
                    rust_str(compiled.sub("-", haystack)),
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
        "//! not take part in a match is `None`. `error` is true where CPython took",
        "//! a non-empty match right after an empty match at the same position,",
        "//! which the Rust implementation reports as an error.",
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
    print(
        "wrote %s: %d split, %d findall, %d sub cases (%s)"
        % (OUT_PATH, len(split_rows), len(findall_rows), len(sub_rows), version),
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
