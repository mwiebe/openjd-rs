// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `openjd_output_var` directives (RFC 0010 §Output emission) in an Action's
//! output: recognizing a directive line, deciding how the Action treats it,
//! reading `@` files, and feeding each value to the model's
//! [`OutputCollector`].
//!
//! The line readers (`subprocess.rs` on the direct path, `cross_user_helper.rs`
//! on the helper path) call into an [`OutputVars`] for every line, before the
//! [`ActionFilter`](crate::action_filter::ActionFilter) sees it, and process
//! each line to completion before the next, so an `@` file is read before any
//! later line is processed (RFC 0010 §File values item 5).

use std::path::PathBuf;

use openjd_expr::ExprValue;
use openjd_model::outputs::{parse_output_directive, OutputCollector, OutputDirective};

use crate::output_file::DIRECTIVE_PREFIX;

/// How the running Action treats `openjd_output_var:` lines (RFC 0010 §The
/// `openjd_output_var` directive, "Which Actions may print it").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputTreatment {
    /// The `onRun` of a Task of a Step that declares output variables (or a
    /// wrap hook that runs in its place): directives report values.
    Allowed,
    /// Any other Action defined in a Job Template that lists `CONTROL_FLOW`:
    /// a directive is invalid.
    Forbidden,
    /// No `CONTROL_FLOW`, or an Action defined in an Environment Template:
    /// the line is ordinary output.
    Ignored,
}

/// What processing one directive line came to.
#[derive(Debug, Clone)]
pub(crate) enum LineOutcome {
    /// A valid directive; the collector accepted `value`.
    Accepted {
        task: Option<i64>,
        name: String,
        value: ExprValue,
    },
    /// An invalid directive. The Action is canceled and fails with this
    /// message.
    Invalid(String),
}

/// What [`OutputVars::begin`] needs next.
#[derive(Debug)]
pub(crate) enum Begin {
    /// Not a directive line for this Action; the line is ordinary output.
    NotDirective,
    /// The directive is processed.
    Done(LineOutcome),
    /// An `@` directive whose name passed the collector's checks: read the
    /// file, then call [`OutputVars::finish_file`].
    ReadFile(FileRequest),
}

/// An `@` file to read for a directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRequest {
    pub name: String,
    pub task: Option<i64>,
    /// The path as the directive wrote it.
    pub path: String,
    /// The Session working directory, which a relative path is resolved
    /// against and which the file must be inside.
    pub root: PathBuf,
    /// The bytes the file may have; read at most one more.
    pub max_bytes: usize,
    /// The directive's line, quoted after a collector error about the
    /// file's content so the message names the offending text.
    pub line: String,
}

impl FileRequest {
    /// Read the file as the current user (the direct path).
    pub fn read(&self) -> Result<Vec<u8>, String> {
        crate::output_file::read_output_file(&self.root, &self.path, self.max_bytes)
    }
}

/// The `openjd_output_var` state of one running Action.
#[derive(Debug)]
pub(crate) struct OutputVars {
    treatment: OutputTreatment,
    /// `Some` exactly when the treatment is `Allowed`.
    collector: Option<OutputCollector>,
    root: PathBuf,
    /// The per-Task size limit the collector was built with.
    max_bytes: usize,
    /// Set by the first invalid directive. Later directive lines are not
    /// processed: the Action is already failing.
    invalid: bool,
}

impl Default for OutputVars {
    fn default() -> Self {
        Self::ignored()
    }
}

impl OutputVars {
    /// Directive lines are ordinary output.
    pub fn ignored() -> Self {
        Self {
            treatment: OutputTreatment::Ignored,
            collector: None,
            root: PathBuf::new(),
            max_bytes: 0,
            invalid: false,
        }
    }

    /// A directive line is invalid.
    pub fn forbidden() -> Self {
        Self {
            treatment: OutputTreatment::Forbidden,
            ..Self::ignored()
        }
    }

    /// Directive lines report values to `collector`; `@` files are resolved
    /// against `root`, the Session working directory.
    pub fn allowed(collector: OutputCollector, root: PathBuf, max_bytes: usize) -> Self {
        Self {
            treatment: OutputTreatment::Allowed,
            collector: Some(collector),
            root,
            max_bytes,
            invalid: false,
        }
    }

    /// The per-Task size limit, when the treatment is `Allowed`: what the
    /// cross-user helper may read of an `@` file (plus one byte).
    pub fn max_bytes(&self) -> Option<usize> {
        (self.treatment == OutputTreatment::Allowed).then_some(self.max_bytes)
    }

    /// Whether a directive was invalid.
    #[cfg(test)]
    pub fn is_invalid(&self) -> bool {
        self.invalid
    }

    /// At a successful exit: the collector's `finish`, which checks that
    /// every Task has its `ONCE` and `LAST` outputs and its `APPEND`
    /// `minLength`. `None` when the Action reports no outputs. Takes the
    /// collector, so a second call is `None`.
    pub fn finish(&mut self) -> Option<Result<crate::action::ActionOutputs, String>> {
        let collector = self.collector.take()?;
        Some(collector.finish().map_err(|e| e.message().to_string()))
    }

    /// Start processing `line` (without its line terminator). `truncated`
    /// says the line was cut to the line limit, `lossy` that it was not valid
    /// UTF-8 and was decoded with escapes; a directive on such a line is
    /// invalid (RFC 0010 §Limits item 1, §Processing a directive item 9).
    pub fn begin(&mut self, line: &str, truncated: bool, lossy: bool) -> Begin {
        if self.treatment == OutputTreatment::Ignored || self.invalid {
            return Begin::NotDirective;
        }
        let Some(payload) = line.strip_prefix(DIRECTIVE_PREFIX) else {
            return Begin::NotDirective;
        };
        let outcome = match self.treatment {
            OutputTreatment::Forbidden => LineOutcome::Invalid(format!(
                "Invalid openjd_output_var directive: this Action can't report output values; \
                 only the onRun action of a Step that declares outputVariables can, in {}",
                quote(line)
            )),
            _ if truncated => LineOutcome::Invalid(format!(
                "Invalid openjd_output_var directive: the line is longer than the {} byte line \
                 limit, so it was cut; write a large value to a file and report it with @<file>, \
                 in {}",
                crate::subprocess::LOG_LINE_MAX_LENGTH,
                quote(line)
            )),
            _ if lossy => LineOutcome::Invalid(format!(
                "Invalid openjd_output_var directive: the line is not valid UTF-8, in {}",
                quote(line)
            )),
            _ => {
                let collector = self.collector.as_mut().expect("Allowed has a collector");
                match parse_output_directive(payload) {
                    Err(e) => LineOutcome::Invalid(e.message().to_string()),
                    Ok(OutputDirective::Inline { name, task, json }) => {
                        match collector.accept(name, task, json.as_bytes(), false) {
                            Ok(value) => LineOutcome::Accepted {
                                task,
                                name: name.to_string(),
                                value,
                            },
                            Err(e) => LineOutcome::Invalid(with_text(&e, line)),
                        }
                    }
                    Ok(OutputDirective::File { name, task, path }) => {
                        match collector.check_name(name, task) {
                            Err(e) => LineOutcome::Invalid(with_text(&e, line)),
                            Ok(()) => {
                                return Begin::ReadFile(FileRequest {
                                    name: name.to_string(),
                                    task,
                                    path: path.to_string(),
                                    root: self.root.clone(),
                                    max_bytes: collector.remaining_bytes_for(name, task),
                                    line: line.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        };
        Begin::Done(self.record(outcome))
    }

    /// Finish an `@` directive with what reading its file gave: the content
    /// (at most `request.max_bytes + 1` bytes), or a phrase that completes
    /// "the @ file '<path>' ..." (RFC 0010 §Processing a directive step 3).
    pub fn finish_file(
        &mut self,
        request: &FileRequest,
        content: Result<Vec<u8>, String>,
    ) -> LineOutcome {
        let size = content.as_ref().map_or(0, Vec::len);
        self.finish_file_sized(request, content, size)
    }

    /// [`finish_file`](Self::finish_file) with the number of bytes read given
    /// separately, for the helper path, where `content` holds only the valid
    /// UTF-8 prefix the helper sent, and a stand-in byte after it when the
    /// file is not valid UTF-8.
    fn finish_file_sized(
        &mut self,
        request: &FileRequest,
        content: Result<Vec<u8>, String>,
        size: usize,
    ) -> LineOutcome {
        let label = label(&request.name, request.task);
        let outcome = match content {
            Err(phrase) => {
                // On the helper path the phrase comes from the helper's
                // output stream, so it is cut and escaped like quoted text.
                let phrase = quote(&phrase);
                LineOutcome::Invalid(format!(
                    "Output variable '{label}': the @ file {} {}",
                    quote(&request.path),
                    &phrase[1..phrase.len() - 1]
                ))
            }
            Ok(_) if size > request.max_bytes => LineOutcome::Invalid(format!(
                "Output variable '{label}': the @ file {} is larger than the {} bytes the \
                 Task's output values have left (the limit is {} bytes per Task)",
                quote(&request.path),
                request.max_bytes,
                self.max_bytes
            )),
            Ok(content) => {
                let collector = self.collector.as_mut().expect("Allowed has a collector");
                match collector.accept(&request.name, request.task, &content, true) {
                    Ok(value) => LineOutcome::Accepted {
                        task: request.task,
                        name: request.name.clone(),
                        value,
                    },
                    // The collector's message names neither the file nor
                    // the line (it saw only the content).
                    Err(e) => LineOutcome::Invalid(format!(
                        "{}, in {}",
                        e.message(),
                        quote(&request.line)
                    )),
                }
            }
        };
        self.record(outcome)
    }

    /// Finish an `@` directive the cross-user helper resolved: `line` is the
    /// directive's line, `parts` what the helper found in it, `content` what
    /// it sent (the valid UTF-8 prefix of the file, then a stand-in byte when
    /// the file is not valid UTF-8), and `size` how many bytes it read. The
    /// line is re-parsed here, and a disagreement with the helper makes the
    /// directive invalid; the parent never reads the file.
    ///
    /// `None` when an earlier directive was already invalid: the helper does
    /// not know the Action is failing and resolves later `@` lines anyway,
    /// but they are ordinary output, as on the direct path.
    pub fn finish_helper_file(
        &mut self,
        line: &str,
        parts: &HelperFileParts,
        content: Result<Vec<u8>, String>,
        size: usize,
    ) -> Option<LineOutcome> {
        if self.invalid {
            return None;
        }
        Some(match self.begin(line, false, false) {
            Begin::ReadFile(request) => {
                let agrees = line.get(parts.name.clone()) == Some(request.name.as_str())
                    && parts.task == request.task
                    && line.get(parts.path.clone()) == Some(request.path.as_str());
                if agrees {
                    self.finish_file_sized(&request, content, size)
                } else {
                    self.record(LineOutcome::Invalid(format!(
                        "Invalid openjd_output_var directive: the cross-user helper read the \
                         directive differently from the parser, in {}",
                        quote(line)
                    )))
                }
            }
            Begin::Done(outcome) => outcome,
            Begin::NotDirective => self.record(LineOutcome::Invalid(format!(
                "Invalid openjd_output_var directive: the cross-user helper reported an @ file \
                 for a line that is not one, in {}",
                quote(line)
            ))),
        })
    }

    /// An `@` directive reached the parent as an ordinary line on the
    /// cross-user path, so the helper did not read its file as the job user.
    pub fn unresolved_on_helper_path(&mut self, request: &FileRequest) -> LineOutcome {
        self.record(LineOutcome::Invalid(format!(
            "Output variable '{}': the cross-user helper did not read the @ file {}",
            label(&request.name, request.task),
            quote(&request.path)
        )))
    }

    fn record(&mut self, outcome: LineOutcome) -> LineOutcome {
        if matches!(outcome, LineOutcome::Invalid(_)) {
            self.invalid = true;
        }
        outcome
    }
}

/// What the cross-user helper found in an `@` directive line: byte ranges of
/// the name and path, and the task selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperFileParts {
    pub name: std::ops::Range<usize>,
    pub task: Option<i64>,
    pub path: std::ops::Range<usize>,
}

/// The most characters of a line a message quotes.
const QUOTE_LIMIT: usize = 200;

/// `text` in single quotes, cut to [`QUOTE_LIMIT`] characters with `...` and
/// with control characters escaped, as `openjd_model::outputs` messages
/// quote text.
fn quote(text: &str) -> String {
    let (shown, cut) = match text.char_indices().nth(QUOTE_LIMIT) {
        None => (text, ""),
        Some((end, _)) => (&text[..end], "..."),
    };
    let mut out = String::with_capacity(shown.len() + 5);
    out.push('\'');
    for c in shown.chars() {
        if c.is_control() {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out.push_str(cut);
    out.push('\'');
    out
}

/// The collector's message for a directive on `line`, with the line quoted
/// after it unless the message always quotes the offending text itself (a
/// grammar, JSON, or path error), because RFC 0010 §Processing a directive
/// requires the failure to name it. Other kinds may not: an undeclared name,
/// a second `ONCE` value, a task selector problem, the size limit, a
/// constraint such as an `APPEND` list's `maxLength`, or a conversion of a
/// `null` or an object. (For an `@` file's content, see `finish_file_sized`,
/// which always adds the line.)
fn with_text(e: &openjd_model::outputs::OutputError, line: &str) -> String {
    use openjd_model::outputs::OutputErrorKind;
    match e.kind() {
        OutputErrorKind::Grammar | OutputErrorKind::Json | OutputErrorKind::Path => {
            e.message().to_string()
        }
        _ => format!("{}, in {}", e.message(), quote(line)),
    }
}

/// `N`, or `N[3]` in a chunked Step, with a long name cut as by [`quote`].
fn label(name: &str, task: Option<i64>) -> String {
    let quoted = quote(name);
    let name = &quoted[1..quoted.len() - 1];
    match task {
        Some(t) => format!("{name}[{t}]"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openjd_expr::PathFormat;
    use openjd_model::job::OutputVariable;

    fn decl(yaml: &str) -> Vec<OutputVariable> {
        serde_json::from_str(yaml).unwrap()
    }

    fn allowed(decls: &str, chunk: Option<Vec<i64>>) -> OutputVars {
        let declared = decl(decls);
        let collector =
            OutputCollector::new(&declared, 100, &[], PathFormat::host(), chunk).unwrap();
        OutputVars::allowed(collector, std::env::temp_dir(), 100)
    }

    fn invalid(o: Begin) -> String {
        match o {
            Begin::Done(LineOutcome::Invalid(m)) => m,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn ignored_never_recognizes() {
        let mut v = OutputVars::ignored();
        assert!(matches!(
            v.begin("openjd_output_var: N=1", false, false),
            Begin::NotDirective
        ));
    }

    #[test]
    fn forbidden_fails_any_directive_line() {
        // The helper is not asked to read @ files for a Forbidden Action.
        assert_eq!(OutputVars::forbidden().max_bytes(), None);
        assert_eq!(OutputVars::ignored().max_bytes(), None);
        let mut v = OutputVars::forbidden();
        assert_eq!(
            invalid(v.begin("openjd_output_var:", false, false)),
            "Invalid openjd_output_var directive: this Action can't report output values; only \
             the onRun action of a Step that declares outputVariables can, in \
             'openjd_output_var:'"
        );
        assert!(v.is_invalid());
        // Later lines are not processed.
        assert!(matches!(
            v.begin("openjd_output_var: N=1", false, false),
            Begin::NotDirective
        ));
        // Other lines are not directives.
        let mut v = OutputVars::forbidden();
        assert!(matches!(
            v.begin(" openjd_output_var: N=1", false, false),
            Begin::NotDirective
        ));
    }

    #[test]
    fn accepts_inline_values_without_a_space_after_the_colon() {
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        match v.begin("openjd_output_var:N=1", false, false) {
            Begin::Done(LineOutcome::Accepted { task, name, value }) => {
                assert_eq!((task, name.as_str(), value), (None, "N", ExprValue::Int(1)));
            }
            other => panic!("{other:?}"),
        }
        assert!(!v.is_invalid());
    }

    #[test]
    fn truncated_and_lossy_lines_are_invalid() {
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert_eq!(
            invalid(v.begin("openjd_output_var: N=1", true, false)),
            "Invalid openjd_output_var directive: the line is longer than the 65536 byte line \
             limit, so it was cut; write a large value to a file and report it with @<file>, in \
             'openjd_output_var: N=1'"
        );
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert_eq!(
            invalid(v.begin(r"openjd_output_var: N=1\xff", false, true)),
            r"Invalid openjd_output_var directive: the line is not valid UTF-8, in 'openjd_output_var: N=1\xff'"
        );
    }

    #[test]
    fn file_requests_and_their_failures() {
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, Some(vec![3]));
        let Begin::ReadFile(req) = v.begin("openjd_output_var: N[3]= @a b \t", false, false) else {
            panic!()
        };
        assert_eq!(
            (
                req.name.as_str(),
                req.task,
                req.path.as_str(),
                req.max_bytes
            ),
            ("N", Some(3), "a b", 100)
        );
        assert_eq!(
            match v.finish_file(&req, Err("does not exist".into())) {
                LineOutcome::Invalid(m) => m,
                o => panic!("{o:?}"),
            },
            "Output variable 'N[3]': the @ file 'a b' does not exist"
        );

        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        let Begin::ReadFile(req) = v.begin("openjd_output_var: N=@f", false, false) else {
            panic!()
        };
        assert_eq!(
            match v.finish_file(&req, Ok(vec![b'1'; 101])) {
                LineOutcome::Invalid(m) => m,
                o => panic!("{o:?}"),
            },
            "Output variable 'N': the @ file 'f' is larger than the 100 bytes the Task's \
             output values have left (the limit is 100 bytes per Task)"
        );

        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        let Begin::ReadFile(req) = v.begin("openjd_output_var: N=@f", false, false) else {
            panic!()
        };
        assert!(matches!(
            v.finish_file(&req, Ok(b"\xef\xbb\xbf 7\n".to_vec())),
            LineOutcome::Accepted {
                value: ExprValue::Int(7),
                ..
            }
        ));
    }

    #[test]
    fn undeclared_name_is_reported_before_the_file_is_read() {
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert_eq!(
            invalid(v.begin("openjd_output_var: M=@missing", false, false)),
            "Output variable 'M': the Step does not declare it (declared: 'N'), in \
             'openjd_output_var: M=@missing'"
        );
    }

    #[test]
    fn helper_files_are_reparsed() {
        let line = "openjd_output_var: N=@f";
        let parts = HelperFileParts {
            name: 19..20,
            task: None,
            path: 22..23,
        };
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert!(matches!(
            v.finish_helper_file(line, &parts, Ok(b"5".to_vec()), 1),
            Some(LineOutcome::Accepted {
                value: ExprValue::Int(5),
                ..
            })
        ));
        // The size the helper read decides, not the bytes it sent.
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert!(matches!(
            v.finish_helper_file(line, &parts, Ok(b"5\xff".to_vec()), 101),
            Some(LineOutcome::Invalid(m)) if m == "Output variable 'N': the @ file 'f' is \
                larger than the 100 bytes the Task's output values have left (the limit is \
                100 bytes per Task)"
        ));
        // After an invalid directive, a resolved @ line is ordinary output,
        // as on the direct path.
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert!(matches!(
            v.begin("openjd_output_var: N=x", false, false),
            Begin::Done(LineOutcome::Invalid(_))
        ));
        assert!(v
            .finish_helper_file(line, &parts, Ok(b"5".to_vec()), 1)
            .is_none());
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        let wrong = HelperFileParts {
            path: 21..23,
            ..parts.clone()
        };
        assert!(matches!(
            v.finish_helper_file(line, &wrong, Ok(b"5".to_vec()), 1),
            Some(LineOutcome::Invalid(m)) if m == "Invalid openjd_output_var directive: the cross-user \
                helper read the directive differently from the parser, in \
                'openjd_output_var: N=@f'"
        ));
        const DIFFERS: &str = "Invalid openjd_output_var directive: the cross-user helper read \
                               the directive differently from the parser, in \
                               'openjd_output_var: N=@f'";
        let differs = |parts: &HelperFileParts| {
            let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
            match v.finish_helper_file(line, parts, Ok(b"5".to_vec()), 1) {
                Some(LineOutcome::Invalid(m)) => m == DIFFERS,
                _ => false,
            }
        };
        assert!(differs(&HelperFileParts {
            name: 18..20,
            ..parts.clone()
        }));
        assert!(differs(&HelperFileParts {
            task: Some(0),
            ..parts.clone()
        }));
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert!(matches!(
            v.finish_helper_file("plain", &parts, Ok(b"5".to_vec()), 1),
            Some(LineOutcome::Invalid(m)) if m == "Invalid openjd_output_var directive: the \
                cross-user helper reported an @ file for a line that is not one, in 'plain'"
        ));
        // A read error from the helper is cut and escaped like quoted text.
        let mut v = allowed(r#"[{"name": "N", "type": "INT"}]"#, None);
        assert!(matches!(
            v.finish_helper_file(line, &parts, Err("can't be read:\nforged".into()), 0),
            Some(LineOutcome::Invalid(m)) if m == r"Output variable 'N': the @ file 'f' can't be read:\nforged"
        ));
    }

    /// The helper's `@` recognizer must accept exactly the lines the model's
    /// parser reads as `File`, with the same name, selector, and path (plan
    /// S3, "recognizer parity"). Checked over every combination of a set of
    /// fragments that covers each grammar element and its near misses.
    #[test]
    fn helper_recognizer_agrees_with_the_model_parser() {
        let prefixes = [
            "openjd_output_var:",
            "openjd_output_var :",
            "Openjd_output_var:",
        ];
        let lead = ["", " ", "\t", " \t ", "\u{0c}", "\u{a0}"];
        let names = ["N", "_n1", "1N", "", "N-x", "Né", "ab_C9"];
        let selectors = [
            "",
            "[3]",
            "[012]",
            "[-7]",
            "[]",
            "[ 1]",
            "[1 ]",
            "[+1]",
            "[1",
            "[a]",
            "[-]",
            "[9223372036854775807]",
            "[9223372036854775808]",
            "[-9223372036854775808]",
            "[1][2]",
        ];
        let before_eq = ["", " ", "\t"];
        let after_eq = ["", " ", "\t ", "\u{0c}"];
        let values = [
            "@f", "@", "@ ", "@a b", "@@", "@\u{0c}", "@é", "1", "\"@f\"", "@f=g", "",
        ];
        let trailing = ["", " ", "\t", " \t", "\u{0c}", "\r"];
        let mut checked = 0;
        for p in prefixes {
            for l in lead {
                for n in names {
                    for sel in selectors {
                        for b in before_eq {
                            for a in after_eq {
                                for v in values {
                                    for t in trailing {
                                        let line = format!("{p}{l}{n}{sel}{b}={a}{v}{t}");
                                        check_parity(&line);
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Lines without '=' at all.
        for line in [
            "openjd_output_var:",
            "openjd_output_var: N",
            "openjd_output_var: N@f",
        ] {
            check_parity(line);
        }
        assert!(checked > 100_000);
    }

    fn check_parity(line: &str) {
        let helper = crate::output_file::recognize_file_directive(line)
            .map(|d| (line[d.name].to_string(), d.task, line[d.path].to_string()));
        let model = line.strip_prefix(DIRECTIVE_PREFIX).and_then(|payload| {
            match parse_output_directive(payload) {
                Ok(OutputDirective::File { name, task, path }) => {
                    Some((name.to_string(), task, path.to_string()))
                }
                _ => None,
            }
        });
        assert_eq!(helper, model, "{line:?}");
    }

    #[test]
    fn quote_cuts_and_escapes() {
        assert_eq!(quote("a\tb"), r"'a\tb'");
        let long = "x".repeat(250);
        assert_eq!(quote(&long), format!("'{}...'", "x".repeat(200)));
        assert_eq!(label("N", Some(-2)), "N[-2]");
    }
}
