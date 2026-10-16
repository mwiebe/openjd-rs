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
    /// The text after `openjd_output_var:` does not match the grammar.
    Grammar,
    /// The directive names an output the Step does not declare.
    Undeclared,
    /// The task selector is missing in a chunked Step, present in another
    /// Step, or names a value that is not in the chunk being run; or the
    /// chunk given to [`OutputCollector::new`] repeats a value.
    Selector,
    /// A second directive for a `collect: ONCE` output (of the same Task).
    Repeated,
    /// The value is not one valid JSON value.
    Json,
    /// An `@` file's content is not valid UTF-8 or contains a NUL character.
    FileContent,
    /// The value is or contains `null` or an object, nests deeper than the
    /// target type, or does not convert to it.
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
}

/// An invalid directive, or incomplete outputs at exit.
///
/// The message names the output (with its task selector in a chunked Step)
/// and the problem. It quotes an inline value's text, shortened when long, and
/// never quotes an `@` file's text. Its `Display` is the message.
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

/// `text` in single quotes, cut to [`QUOTE_LIMIT`] characters with `...`
/// when longer.
pub(crate) fn quote(text: &str) -> String {
    match text.char_indices().nth(QUOTE_LIMIT) {
        None => format!("'{text}'"),
        Some((end, _)) => format!("'{}...'", &text[..end]),
    }
}
