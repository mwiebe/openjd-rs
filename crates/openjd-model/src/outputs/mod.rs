// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` output values (RFC 0010 §Output emission): the
//! `openjd_output_var` directive parser and the collector that turns one
//! Action's directives into each Task's output values.
//!
//! The module does no I/O, so a session runtime, the CLI, `openjd-for-js`,
//! and a scheduler that re-validates values can all share it. The caller
//! recognizes the directive on a line, strips the line terminator and the
//! `openjd_output_var:` prefix, reads `@` files, and cancels the Action when
//! a directive is invalid. See `specs/model/outputs.md`.

mod collector;
mod convert;
mod directive;

pub use collector::{OutputCollector, TaskOutputs};
pub use directive::{parse_output_directive, OutputDirective};

/// The least number of bytes of output values per Task that a runtime must
/// accept (RFC 0010 §Limits item 2: 1 MiB). A runtime passes this, or more,
/// as `max_bytes` to [`OutputCollector::new`].
pub const MIN_OUTPUT_BYTES_LIMIT: usize = 1_048_576;

/// What made a directive invalid, or made the Action's outputs incomplete
/// when it exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OutputErrorKind {
    /// The text after `openjd_output_var:` does not match the grammar, or an
    /// inline value is not valid UTF-8.
    Grammar,
    /// The directive names an output the Step does not declare.
    Undeclared,
    /// The task selector is missing in a chunked Step, present in another
    /// Step, or names a value that is not in the chunk being run; or the
    /// chunk given to [`OutputCollector::new`] repeats a value.
    Selector,
    /// A second directive for a `collect: ONCE` output (of the same Task).
    Repeated,
    /// The value is not one valid JSON value, or holds a string with a lone
    /// UTF-16 surrogate escape.
    Json,
    /// An `@` file's content is not valid UTF-8 or contains a NUL character.
    FileContent,
    /// The value is or contains `null` or an object, nests deeper than the
    /// target type (or than any type), has a number too large to represent as
    /// a float, or does not convert to the target type.
    Conversion,
    /// A `PATH` value is relative or has no submission-context form.
    Path,
    /// The value violates a declared constraint property.
    Constraint,
    /// The value takes the Task's output values over the size limit.
    Size,
    /// At exit: a `ONCE` or `LAST` output has no value, or an `APPEND`
    /// output's list is shorter than its `minLength`.
    Missing,
    /// A declaration given to [`OutputCollector::new`] cannot be collected:
    /// `collect: APPEND` on a type that is not a list. Template validation
    /// rejects such a declaration, so only a hand-built one reaches this.
    Declaration,
}

/// An invalid directive, incomplete outputs at exit, or a refused
/// [`OutputCollector::new`].
///
/// The message names the problem, and the output (with its task selector in
/// a chunked Step) when there is one: a grammar error may have no name yet,
/// and a repeated chunk value names the value instead. It may quote an inline value's text, and the converted
/// value or element that is invalid (a `PATH` with no submission-context form,
/// a string not in `allowedValues`), but never an `@` file's text as such: a
/// JSON or conversion error for a file value gives only a position or a type.
/// Quoted text, an output name, and the EXPR coercion message appended to a
/// conversion error are cut to 200 characters with control characters
/// escaped.
/// Its `Display` is the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputError {
    kind: OutputErrorKind,
    message: String,
}

impl OutputError {
    pub(crate) fn new(kind: OutputErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// What kind of problem this is.
    pub fn kind(&self) -> OutputErrorKind {
        self.kind
    }

    /// The full message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OutputError {}

/// The most characters of a value or directive a message quotes.
const QUOTE_LIMIT: usize = 200;

/// `text` in single quotes for a message: cut to [`QUOTE_LIMIT`] characters
/// with `...` when longer, and with control characters escaped (`\n`,
/// `\u{1b}`), so a message stays on one line and cannot drive a terminal.
pub(crate) fn quote(text: &str) -> String {
    let (shown, cut) = match text.char_indices().nth(QUOTE_LIMIT) {
        None => (text, ""),
        Some((end, _)) => (&text[..end], "..."),
    };
    format!("'{}{cut}'", escape_controls(shown))
}

/// `text` for a message without quotes: cut and escaped as by [`quote`].
pub(crate) fn shorten(text: &str) -> String {
    match text.char_indices().nth(QUOTE_LIMIT) {
        None => escape_controls(text),
        Some((end, _)) => format!("{}...", escape_controls(&text[..end])),
    }
}

/// `text` with its control characters escaped (`\n`, `\u{1b}`).
pub(crate) fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}
