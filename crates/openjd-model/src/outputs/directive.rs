// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The `openjd_output_var` directive grammar (RFC 0010 §The
//! `openjd_output_var` directive):
//!
//! ```text
//! <Directive>    ::= <WS>* <Identifier> <TaskSelector>? "=" <WS>* <Value> <WS>*
//! <TaskSelector> ::= "[" <Int> "]"
//! <Value>        ::= <JSON> | "@" <FilePath>
//! <WS>           ::= " " | "\t"
//! ```

use super::{quote, shorten, OutputError, OutputErrorKind};

/// One parsed `openjd_output_var` directive: the text after
/// `openjd_output_var:`, with the line terminator already removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputDirective<'a> {
    /// `<Name>[<Int>]=<JSON>`. `json` is the value's text without the spaces
    /// and tabs around it; it is not parsed yet.
    Inline {
        /// The output's name.
        name: &'a str,
        /// The task selector's value, if the directive has one.
        task: Option<i64>,
        /// The JSON text.
        json: &'a str,
    },
    /// `<Name>[<Int>]=@<FilePath>`. `path` is the text after `@` without
    /// trailing spaces and tabs, and is never empty.
    File {
        /// The output's name.
        name: &'a str,
        /// The task selector's value, if the directive has one.
        task: Option<i64>,
        /// The file path, as written.
        path: &'a str,
    },
}

impl<'a> OutputDirective<'a> {
    /// The output's name.
    pub fn name(&self) -> &'a str {
        match self {
            Self::Inline { name, .. } | Self::File { name, .. } => name,
        }
    }

    /// The task selector's value, if the directive has one.
    pub fn task(&self) -> Option<i64> {
        match self {
            Self::Inline { task, .. } | Self::File { task, .. } => *task,
        }
    }
}

fn is_ws(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn grammar_error(problem: &str, payload: &str) -> OutputError {
    OutputError::new(
        OutputErrorKind::Grammar,
        format!(
            "Invalid openjd_output_var directive: {problem}, in {}",
            quote(payload)
        ),
    )
}

/// Parse the text after `openjd_output_var:` (the line terminator removed).
///
/// This checks only the grammar. It does not parse the JSON, check the name
/// against the Step's declarations, or look at the file; the
/// [`OutputCollector`](super::OutputCollector) does those. The task
/// selector's `<Int>` (an optional `-`, then ASCII digits, so `[012]` is 12)
/// must fit in an `i64`; one that does not is a grammar error.
pub fn parse_output_directive(payload: &str) -> Result<OutputDirective<'_>, OutputError> {
    let rest = payload.trim_start_matches(is_ws);

    // <Identifier>: [A-Za-z_][A-Za-z0-9_]*
    let name_len = rest
        .char_indices()
        .find(|&(i, c)| !(c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())))
        .map_or(rest.len(), |(i, _)| i);
    if name_len == 0 {
        return Err(grammar_error(
            "expected an output name (an identifier) followed by '='",
            payload,
        ));
    }
    let name = &rest[..name_len];
    let mut rest = &rest[name_len..];
    // For messages: the name as written can be as long as the line.
    let shown_name = shorten(name);

    let task = if let Some(after_bracket) = rest.strip_prefix('[') {
        let Some(close) = after_bracket.find(']') else {
            return Err(grammar_error(
                &format!("the task selector of '{shown_name}' has no closing ']'"),
                payload,
            ));
        };
        let digits = &after_bracket[..close];
        let unsigned = digits.strip_prefix('-').unwrap_or(digits);
        if unsigned.is_empty() || !unsigned.bytes().all(|b| b.is_ascii_digit()) {
            return Err(grammar_error(
                &format!(
                    "the task selector of '{shown_name}' must be an integer (an optional '-' then digits, \
                     with no whitespace), not {}",
                    quote(digits)
                ),
                payload,
            ));
        }
        let Ok(value) = digits.parse::<i64>() else {
            return Err(grammar_error(
                &format!(
                    "the task selector of '{shown_name}', {}, is outside the 64-bit integer range",
                    quote(digits)
                ),
                payload,
            ));
        };
        rest = &after_bracket[close + 1..];
        Some(value)
    } else {
        None
    };

    let Some(after_eq) = rest.strip_prefix('=') else {
        let before = match task {
            Some(_) => &payload[..payload.len() - rest.len()],
            None => name,
        };
        let before = shorten(before.trim_start_matches(is_ws));
        let problem = match rest.chars().next() {
            Some(c) if is_ws(c) => format!("whitespace is not allowed after '{before}'"),
            Some(c) => format!(
                "expected '=' after '{before}', found {}",
                quote(&c.to_string())
            ),
            None => format!("expected '=' after '{before}'"),
        };
        return Err(grammar_error(&problem, payload));
    };

    let value = after_eq.trim_matches(is_ws);
    if let Some(path) = value.strip_prefix('@') {
        if path.is_empty() {
            return Err(grammar_error(
                &format!("the file path after '@' for '{shown_name}' is empty"),
                payload,
            ));
        }
        return Ok(OutputDirective::File { name, task, path });
    }
    if value.is_empty() {
        return Err(grammar_error(
            &format!("'{shown_name}' has no value after '='"),
            payload,
        ));
    }
    Ok(OutputDirective::Inline {
        name,
        task,
        json: value,
    })
}
