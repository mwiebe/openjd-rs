// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The `openjd_output_var` directive grammar (RFC 0010 §The
//! `openjd_output_var` directive): `outputs::parse_output_directive`. See
//! `specs/model/outputs.md` § Directive grammar.

use openjd_model::outputs::{parse_output_directive, OutputDirective, OutputErrorKind};

fn inline<'a>(name: &'a str, task: Option<i64>, json: &'a str) -> OutputDirective<'a> {
    OutputDirective::Inline { name, task, json }
}

fn file<'a>(name: &'a str, task: Option<i64>, path: &'a str) -> OutputDirective<'a> {
    OutputDirective::File { name, task, path }
}

fn grammar_err(payload: &str) -> String {
    let e = parse_output_directive(payload).expect_err("expected a grammar error");
    assert_eq!(e.kind(), OutputErrorKind::Grammar);
    assert_eq!(e.to_string(), e.message());
    e.to_string()
}

// === Both forms ===

#[test]
fn inline_form() {
    assert_eq!(
        parse_output_directive(" FrameCount=1440"),
        Ok(inline("FrameCount", None, "1440"))
    );
}

#[test]
fn file_form() {
    assert_eq!(
        parse_output_directive(" Keys=@keys.json"),
        Ok(file("Keys", None, "keys.json"))
    );
}

#[test]
fn no_space_after_colon() {
    assert_eq!(parse_output_directive("N=1"), Ok(inline("N", None, "1")));
}

#[test]
fn json_text_is_not_parsed() {
    // The grammar only delimits the JSON; the collector parses it.
    assert_eq!(
        parse_output_directive(" S={not json"),
        Ok(inline("S", None, "{not json"))
    );
}

#[test]
fn accessors() {
    let d = parse_output_directive("R[3]=@f").unwrap();
    assert_eq!(d.name(), "R");
    assert_eq!(d.task(), Some(3));
    let d = parse_output_directive("S=\"x\"").unwrap();
    assert_eq!(d.name(), "S");
    assert_eq!(d.task(), None);
}

// === Whitespace ===

#[test]
fn leading_spaces_and_tabs() {
    assert_eq!(
        parse_output_directive(" \t  N=1"),
        Ok(inline("N", None, "1"))
    );
}

#[test]
fn whitespace_before_equals_is_rejected() {
    assert_eq!(
        grammar_err(" N =1"),
        "Invalid openjd_output_var directive: whitespace is not allowed between 'N' and '=', \
         in ' N =1'"
    );
    assert_eq!(
        grammar_err(" N[1]\t=1"),
        "Invalid openjd_output_var directive: whitespace is not allowed between 'N' and '=', \
         in ' N[1]\t=1'"
    );
}

#[test]
fn whitespace_after_equals_is_accepted() {
    assert_eq!(parse_output_directive(" N= 1"), Ok(inline("N", None, "1")));
    assert_eq!(parse_output_directive(" N=\t1"), Ok(inline("N", None, "1")));
    assert_eq!(parse_output_directive(" N= @f"), Ok(file("N", None, "f")));
}

#[test]
fn trailing_spaces_and_tabs_are_removed() {
    assert_eq!(
        parse_output_directive(" N=[1, 2] \t "),
        Ok(inline("N", None, "[1, 2]"))
    );
    assert_eq!(
        parse_output_directive(" N=@out dir/f.json \t"),
        Ok(file("N", None, "out dir/f.json"))
    );
}

#[test]
fn file_path_keeps_inner_and_leading_text() {
    // The path is the rest of the line after `@`.
    assert_eq!(parse_output_directive(" N=@ f"), Ok(file("N", None, " f")));
    assert_eq!(
        parse_output_directive(" N=@@f=1"),
        Ok(file("N", None, "@f=1"))
    );
}

#[test]
fn whitespace_inside_selector_is_rejected() {
    assert_eq!(
        grammar_err(" N[ 1]=1"),
        "Invalid openjd_output_var directive: the task selector of 'N' must be an integer \
         (an optional '-' then digits, with no whitespace), not ' 1', in ' N[ 1]=1'"
    );
    assert_eq!(
        grammar_err(" N[1 ]=1"),
        "Invalid openjd_output_var directive: the task selector of 'N' must be an integer \
         (an optional '-' then digits, with no whitespace), not '1 ', in ' N[1 ]=1'"
    );
}

#[test]
fn whitespace_between_name_and_selector_is_rejected() {
    assert_eq!(
        grammar_err(" N [1]=1"),
        "Invalid openjd_output_var directive: whitespace is not allowed between 'N' and '=', \
         in ' N [1]=1'"
    );
}

// === Values ===

#[test]
fn empty_file_path() {
    assert_eq!(
        grammar_err(" N=@"),
        "Invalid openjd_output_var directive: the file path after '@' for 'N' is empty, \
         in ' N=@'"
    );
    assert_eq!(
        grammar_err(" N=@ \t"),
        "Invalid openjd_output_var directive: the file path after '@' for 'N' is empty, \
         in ' N=@ \t'"
    );
}

#[test]
fn no_value() {
    assert_eq!(
        grammar_err(" N="),
        "Invalid openjd_output_var directive: 'N' has no value after '=', in ' N='"
    );
    assert_eq!(
        grammar_err(" N=  "),
        "Invalid openjd_output_var directive: 'N' has no value after '=', in ' N=  '"
    );
}

// === Names ===

#[test]
fn missing_name() {
    for payload in ["", " ", " =1", " 1N=1", " -N=1", " [1]=1", " é=1"] {
        assert_eq!(
            grammar_err(payload),
            format!(
                "Invalid openjd_output_var directive: expected an output name (an identifier) \
                 followed by '=', in '{payload}'"
            )
        );
    }
}

#[test]
fn identifier_characters() {
    assert_eq!(
        parse_output_directive(" _a9_Z=1"),
        Ok(inline("_a9_Z", None, "1"))
    );
}

#[test]
fn missing_equals() {
    assert_eq!(
        grammar_err(" N"),
        "Invalid openjd_output_var directive: expected '=' after 'N', in ' N'"
    );
    assert_eq!(
        grammar_err(" N:1"),
        "Invalid openjd_output_var directive: expected '=' after 'N', in ' N:1'"
    );
    assert_eq!(
        grammar_err(" Né=1"),
        "Invalid openjd_output_var directive: expected '=' after 'N', in ' Né=1'"
    );
    assert_eq!(
        grammar_err(" N[1]x=1"),
        "Invalid openjd_output_var directive: expected '=' after 'N', in ' N[1]x=1'"
    );
}

// === Task selector ===

#[test]
fn selector() {
    assert_eq!(
        parse_output_directive(" R[12]=1.5"),
        Ok(inline("R", Some(12), "1.5"))
    );
    assert_eq!(
        parse_output_directive(" R[-3]=@t.json"),
        Ok(file("R", Some(-3), "t.json"))
    );
}

#[test]
fn selector_is_matched_by_integer_value() {
    assert_eq!(
        parse_output_directive(" R[012]=1"),
        Ok(inline("R", Some(12), "1"))
    );
    assert_eq!(
        parse_output_directive(" R[-0]=1"),
        Ok(inline("R", Some(0), "1"))
    );
}

#[test]
fn selector_at_i64_bounds() {
    assert_eq!(
        parse_output_directive(" R[9223372036854775807]=1"),
        Ok(inline("R", Some(i64::MAX), "1"))
    );
    assert_eq!(
        parse_output_directive(" R[-9223372036854775808]=1"),
        Ok(inline("R", Some(i64::MIN), "1"))
    );
}

#[test]
fn selector_overflow_is_a_grammar_error() {
    assert_eq!(
        grammar_err(" R[9223372036854775808]=1"),
        "Invalid openjd_output_var directive: the task selector of 'R', \
         '9223372036854775808', is outside the 64-bit integer range, \
         in ' R[9223372036854775808]=1'"
    );
    assert_eq!(
        grammar_err(" R[-9223372036854775809]=1"),
        "Invalid openjd_output_var directive: the task selector of 'R', \
         '-9223372036854775809', is outside the 64-bit integer range, \
         in ' R[-9223372036854775809]=1'"
    );
}

#[test]
fn selector_not_an_integer() {
    for (payload, inner) in [
        (" R[]=1", ""),
        (" R[-]=1", "-"),
        (" R[+1]=1", "+1"),
        (" R[1.0]=1", "1.0"),
        (" R[x]=1", "x"),
        (" R[--1]=1", "--1"),
        (" R[1-]=1", "1-"),
    ] {
        assert_eq!(
            grammar_err(payload),
            format!(
                "Invalid openjd_output_var directive: the task selector of 'R' must be an \
                 integer (an optional '-' then digits, with no whitespace), not '{inner}', \
                 in '{payload}'"
            )
        );
    }
}

#[test]
fn selector_unclosed() {
    assert_eq!(
        grammar_err(" R[1=1"),
        "Invalid openjd_output_var directive: the task selector of 'R' has no closing ']', \
         in ' R[1=1'"
    );
}

#[test]
fn two_selectors() {
    assert_eq!(
        grammar_err(" R[1][2]=1"),
        "Invalid openjd_output_var directive: expected '=' after 'R', in ' R[1][2]=1'"
    );
}

// === Messages ===

#[test]
fn long_payload_is_shortened_in_the_message() {
    let payload = format!(" N{}", "x".repeat(300).replace('x', " "));
    let msg = grammar_err(&payload);
    let quoted: String = payload.chars().take(200).collect();
    assert_eq!(
        msg,
        format!(
            "Invalid openjd_output_var directive: whitespace is not allowed between 'N' and \
             '=', in '{quoted}...'"
        )
    );
}
