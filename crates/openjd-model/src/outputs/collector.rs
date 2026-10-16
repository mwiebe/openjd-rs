// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The output collector (RFC 0010 §Processing a directive, §Collect modes,
//! §Chunked Steps, §Limits).

use indexmap::IndexMap;
use openjd_expr::path_mapping::{unapply_rules_with_source_format, PathFormat, PathMappingRule};
use openjd_expr::types::TypeCode;
use openjd_expr::{ExprType, ExprValue};

use super::convert::{check_no_null_or_object, number_spellings, ConvertError, Converter};
use super::{quote, OutputError, OutputErrorKind};
use crate::job::OutputVariable;
use crate::types::OutputCollectMode;

/// The output values of one Task: each declared output's name and value, in
/// declaration order.
pub type TaskOutputs = IndexMap<String, ExprValue>;

/// What one Task holds so far for one output.
#[derive(Debug, Clone)]
enum Held {
    /// The `ONCE` or current `LAST` value, and its counted size.
    Value(ExprValue, usize),
    /// The `APPEND` elements so far, each with its counted size.
    Elements(Vec<(ExprValue, usize)>),
}

impl Held {
    fn size(&self) -> usize {
        match self {
            Self::Value(_, size) => *size,
            Self::Elements(elements) => elements.iter().map(|(_, size)| size).sum(),
        }
    }
}

/// One Task's state: what it holds for each output, and the total size.
#[derive(Debug, Clone, Default)]
struct TaskState {
    held: IndexMap<String, Held>,
    size: usize,
}

/// Collects the `openjd_output_var` directives of one Action: the `onRun` of
/// one Task, or of every Task of a chunk.
///
/// Call [`check_name`](Self::check_name) for each directive before reading an
/// `@` file, [`remaining_bytes_for`](Self::remaining_bytes_for) to bound the
/// read, then [`accept`](Self::accept) with the value's text; and
/// [`finish`](Self::finish) when the Action exits successfully. An `Err` from
/// any of them means the directive is invalid, or the outputs incomplete, and
/// the Action fails; a failed call leaves the collector's state unchanged.
///
/// In a chunked Step every Task of the chunk has its own values, collect-mode
/// state, and size limit.
#[derive(Debug, Clone)]
pub struct OutputCollector {
    declared: IndexMap<String, OutputVariable>,
    max_bytes: usize,
    path_rules: Vec<PathMappingRule>,
    worker_format: PathFormat,
    chunked: bool,
    tasks: IndexMap<Option<i64>, TaskState>,
}

impl OutputCollector {
    /// A collector for one Action.
    ///
    /// * `declared` — the Step's `job::Step::output_variables`. Names are
    ///   unique there; a repeated name keeps its first declaration.
    /// * `max_bytes` — the per-Task size limit of RFC 0010 §Limits item 2. A
    ///   runtime passes at least [`MIN_OUTPUT_BYTES_LIMIT`](super::MIN_OUTPUT_BYTES_LIMIT).
    /// * `path_rules` — the Session's full path mapping rules in the order it
    ///   applies them, for reverse-mapping `PATH` values
    ///   (see [`openjd_expr::path_mapping::unapply_rules`]).
    /// * `worker_format` — the worker host's path format.
    /// * `chunk` — the values of the Step's `CHUNK[INT]` parameter in the
    ///   chunk being run, or `None` for a Step without one.
    ///
    /// A chunk that repeats a value is an error
    /// ([`OutputErrorKind::Selector`]): a value names exactly one Task.
    pub fn new(
        declared: &[OutputVariable],
        max_bytes: usize,
        path_rules: &[PathMappingRule],
        worker_format: PathFormat,
        chunk: Option<Vec<i64>>,
    ) -> Result<Self, OutputError> {
        let mut by_name = IndexMap::new();
        for var in declared {
            by_name
                .entry(var.name.clone())
                .or_insert_with(|| var.clone());
        }
        // `accept` and `finish` take an APPEND output's element type.
        if let Some(var) = by_name.values().find(|v| {
            v.collect == OutputCollectMode::Append
                && v.param_type.expr_type().code() != TypeCode::List
        }) {
            return Err(OutputError::new(
                OutputErrorKind::Declaration,
                format!(
                    "Output variable '{}': collect mode APPEND requires a LIST type, but it is \
                     declared {}",
                    var.name, var.param_type
                ),
            ));
        }
        let chunked = chunk.is_some();
        let mut tasks = IndexMap::new();
        match chunk {
            None => {
                tasks.insert(None, TaskState::default());
            }
            Some(values) => {
                for value in values {
                    if tasks.insert(Some(value), TaskState::default()).is_some() {
                        return Err(OutputError::new(
                            OutputErrorKind::Selector,
                            format!(
                                "The chunk repeats the CHUNK[INT] value {value}; in a Step with \
                                 output variables each value must name exactly one Task"
                            ),
                        ));
                    }
                }
            }
        }
        Ok(Self {
            declared: by_name,
            max_bytes,
            path_rules: path_rules.to_vec(),
            worker_format,
            chunked,
            tasks,
        })
    }

    /// Steps 1 and 2 of RFC 0010 §Processing a directive: `name` is a
    /// declared output; `task` is present exactly when the Step is chunked and
    /// names a value of the chunk; and a `collect: ONCE` output has no value
    /// yet (for that Task). Call it before reading an `@` file.
    pub fn check_name(&self, name: &str, task: Option<i64>) -> Result<(), OutputError> {
        let var = self.declared.get(name).ok_or_else(|| {
            let declared: Vec<String> = self.declared.keys().map(|n| format!("'{n}'")).collect();
            let declared = if declared.is_empty() {
                "the Step declares no output variables".to_string()
            } else {
                format!("declared: {}", declared.join(", "))
            };
            OutputError::new(
                OutputErrorKind::Undeclared,
                format!(
                    "Output variable {}: the Step does not declare it ({declared})",
                    quote(&label(name, task)),
                ),
            )
        })?;
        let state = self.task_state(name, task)?;
        if var.collect == OutputCollectMode::Once && state.held.contains_key(name) {
            return Err(OutputError::new(
                OutputErrorKind::Repeated,
                format!(
                    "Output variable '{}': a second value was reported, but its collect mode is \
                     ONCE",
                    label(name, task)
                ),
            ));
        }
        Ok(())
    }

    /// How many bytes an `@` file for `name` may have: the Task's limit, less
    /// what the Task holds, not counting the current value of a `LAST` output
    /// that the file's value would replace. A runtime reads at most this many
    /// bytes, plus one to detect a file that exceeds it. Meaningful after
    /// [`check_name`](Self::check_name) succeeds; for a name or task it
    /// rejects, the result counts nothing as replaced.
    pub fn remaining_bytes_for(&self, name: &str, task: Option<i64>) -> usize {
        let Some(state) = self.tasks.get(&task) else {
            return self.max_bytes;
        };
        let replaced = match (self.declared.get(name), state.held.get(name)) {
            (Some(var), Some(held)) if var.collect == OutputCollectMode::Last => held.size(),
            _ => 0,
        };
        self.max_bytes
            .saturating_sub(state.size.saturating_sub(replaced))
    }

    /// Steps 4 to 6 of RFC 0010 §Processing a directive: convert `json` (an
    /// inline value's text, or an `@` file's whole content when `from_file`
    /// is `true`) to the declared type, or under `collect: APPEND` to its
    /// element type; map a `PATH` value back to the submission path context;
    /// check the constraint properties; apply the collect mode; and update
    /// the Task's size. It repeats the checks of
    /// [`check_name`](Self::check_name) first.
    ///
    /// File content may start with a UTF-8 byte order mark and span lines,
    /// and must be valid UTF-8 without a NUL character.
    ///
    /// Returns the accepted value, for the caller to report: the new value
    /// under `ONCE` and `LAST`, the new element under `APPEND`.
    pub fn accept(
        &mut self,
        name: &str,
        task: Option<i64>,
        json: &[u8],
        from_file: bool,
    ) -> Result<ExprValue, OutputError> {
        self.check_name(name, task)?;
        let var = &self.declared[name];
        let label = label(name, task);
        let err = |kind, detail: String| {
            OutputError::new(kind, format!("Output variable '{label}': {detail}"))
        };

        // The text, and the size it counts for.
        let (text, reported) = if from_file {
            let content = json.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(json);
            let text = std::str::from_utf8(content).map_err(|e| {
                err(
                    OutputErrorKind::FileContent,
                    format!(
                        "the @ file's content is not valid UTF-8 (at byte {})",
                        e.valid_up_to() + (json.len() - content.len())
                    ),
                )
            })?;
            if text.contains('\0') {
                return Err(err(
                    OutputErrorKind::FileContent,
                    "the @ file's content contains a NUL character".to_string(),
                ));
            }
            (text, json.len())
        } else {
            let text = std::str::from_utf8(json).map_err(|_| {
                OutputError::new(
                    OutputErrorKind::Grammar,
                    format!(
                        "Invalid openjd_output_var directive: the value of '{label}' is not \
                         valid UTF-8"
                    ),
                )
            })?;
            let text = text.trim_matches([' ', '\t']);
            (text, text.len())
        };

        let parsed: serde_json::Value = serde_json::from_str(text).map_err(|e| {
            let shown = if from_file {
                "the @ file's content".to_string()
            } else {
                quote(text)
            };
            // A number too large for a float is valid JSON (RFC 8259) but
            // does not convert (RFC 0010 §Processing a directive, "Type
            // conversion"); serde_json reports it as a parse error.
            if e.to_string().starts_with("number out of range") {
                err(
                    OutputErrorKind::Conversion,
                    format!(
                        "{shown} has a number too large to represent as a float (at line {} \
                         column {})",
                        e.line(),
                        e.column()
                    ),
                )
            } else {
                err(
                    OutputErrorKind::Json,
                    format!("{shown} is not one valid JSON value: {e}"),
                )
            }
        })?;

        let append = var.collect == OutputCollectMode::Append;
        let declared_ty = var.param_type.expr_type();
        let target = if append {
            declared_ty.params()[0].clone()
        } else {
            declared_ty
        };
        let subject = if append { "element" } else { "value" };
        let conversion = |e: ConvertError| {
            err(
                OutputErrorKind::Conversion,
                format!("{} {}", e.subject, e.detail),
            )
        };
        check_no_null_or_object(&parsed, subject).map_err(conversion)?;
        let mut converter = Converter {
            spellings: number_spellings(text).into_iter(),
            path_format: self.worker_format,
            show_values: !from_file,
        };
        let mut value = converter
            .convert(&parsed, &target, subject)
            .map_err(conversion)?;

        // PATH values are stored in the submission path context.
        let mut size = reported;
        if is_path_type(&target) {
            value = self.unmap(value, subject).map_err(|(at, path, e)| {
                err(
                    OutputErrorKind::Path,
                    format!("{at} {} is not a valid PATH output: {e}", quote(&path)),
                )
            })?;
            size = size.max(stored_json_len(&value));
        }

        // Constraints. A RANGE_EXPR value is checked in its written spelling,
        // as a RANGE_EXPR job parameter value is.
        let checked = match (&parsed, target.code()) {
            (serde_json::Value::String(s), TypeCode::RangeExpr) => ExprValue::String(s.clone()),
            _ => value.clone(),
        };
        let state = &self.tasks[&task];
        let held = state.held.get(name);
        let constraint = |msg: String| {
            // The constraint messages name the output without the selector.
            let msg = match (
                task,
                msg.strip_prefix(&format!("Output variable '{name}': ")),
            ) {
                (Some(_), Some(rest)) => format!("Output variable '{label}': {rest}"),
                _ => msg,
            };
            OutputError::new(OutputErrorKind::Constraint, msg)
        };
        if append {
            let count = match held {
                Some(Held::Elements(elements)) => elements.len(),
                _ => 0,
            };
            var.check_element(&checked).map_err(constraint)?;
            var.check_length(count + 1, false).map_err(constraint)?;
        } else {
            var.check_constraints(&checked).map_err(constraint)?;
        }

        // Size.
        let replaced = match held {
            Some(Held::Value(_, s)) => *s,
            _ => 0,
        };
        let total = state.size - replaced + size;
        if total > self.max_bytes {
            return Err(err(
                OutputErrorKind::Size,
                format!(
                    "the value's {size} bytes take the Task's output values to {total} bytes, \
                     over the limit of {} bytes",
                    self.max_bytes
                ),
            ));
        }

        // Apply the collect mode.
        let state = self.tasks.get_mut(&task).expect("checked by check_name");
        state.size = total;
        if append {
            // An APPEND output only ever holds elements.
            if let Held::Elements(elements) = state
                .held
                .entry(name.to_string())
                .or_insert_with(|| Held::Elements(Vec::new()))
            {
                elements.push((value.clone(), size));
            }
        } else {
            state
                .held
                .insert(name.to_string(), Held::Value(value.clone(), size));
        }
        Ok(value)
    }

    /// At exit (the Action succeeded): every `ONCE` and `LAST` output has a
    /// value and every `APPEND` output's list meets its `minLength`, for every
    /// Task. Returns each Task's values, keyed by its `CHUNK[INT]` value in
    /// the chunk's order (`None` for an unchunked Step), each in declaration
    /// order. An `APPEND` output with no directive is the empty list.
    pub fn finish(self) -> Result<IndexMap<Option<i64>, TaskOutputs>, OutputError> {
        let mut result = IndexMap::with_capacity(self.tasks.len());
        for (task, mut state) in self.tasks {
            let mut outputs = TaskOutputs::with_capacity(self.declared.len());
            for (name, var) in &self.declared {
                let label = label(name, task);
                let value = match (var.collect, state.held.swap_remove(name)) {
                    (OutputCollectMode::Append, held) => {
                        let elements: Vec<ExprValue> = match held {
                            Some(Held::Elements(elements)) => {
                                elements.into_iter().map(|(v, _)| v).collect()
                            }
                            _ => Vec::new(),
                        };
                        let elem_ty = var.param_type.expr_type().params()[0].clone();
                        let len = elements.len();
                        var.check_length(len, true).map_err(|msg| {
                            OutputError::new(
                                OutputErrorKind::Missing,
                                match msg.strip_prefix(&format!("Output variable '{name}': ")) {
                                    Some(rest) => format!("Output variable '{label}': {rest}"),
                                    None => msg,
                                },
                            )
                        })?;
                        build_list(elements, elem_ty, self.worker_format)
                    }
                    (_, Some(Held::Value(value, _))) => value,
                    (mode, _) => {
                        return Err(OutputError::new(
                            OutputErrorKind::Missing,
                            format!(
                                "Output variable '{label}': no value was reported (collect mode \
                                 {mode})"
                            ),
                        ))
                    }
                };
                outputs.insert(name.clone(), value);
            }
            result.insert(task, outputs);
        }
        Ok(result)
    }

    fn task_state(&self, name: &str, task: Option<i64>) -> Result<&TaskState, OutputError> {
        let selector = |detail: String| {
            OutputError::new(
                OutputErrorKind::Selector,
                format!("Output variable '{}': {detail}", label(name, task)),
            )
        };
        match (self.chunked, task) {
            (true, None) => Err(selector(format!(
                "a task selector, as in '{name}[<value>]', is required in a Step with a \
                 CHUNK[INT] parameter"
            ))),
            (false, Some(_)) => Err(selector(
                "a task selector is only allowed in a Step with a CHUNK[INT] parameter".to_string(),
            )),
            _ => self.tasks.get(&task).ok_or_else(|| {
                selector(format!(
                    "{} is not a value of the chunk being run ({})",
                    task.unwrap_or_default(),
                    chunk_summary(self.tasks.keys())
                ))
            }),
        }
    }

    /// Map a `path` value, or each element of a `list[path]`, back to the
    /// submission path context. On failure: where (`value`, `element`,
    /// `item[1]`), the path, and the reason.
    fn unmap(
        &self,
        value: ExprValue,
        subject: &str,
    ) -> Result<ExprValue, (String, String, String)> {
        let unmap_one = |p: &str, at: String| {
            unapply_rules_with_source_format(&self.path_rules, p, self.worker_format)
                .map(|(stored, format)| ExprValue::new_path(stored, format))
                .map_err(|e| (at, p.to_string(), e.to_string()))
        };
        match value {
            ExprValue::Path { value, .. } => unmap_one(&value, subject.to_string()),
            ExprValue::ListPath(values, ..) => {
                let elements = values
                    .iter()
                    .enumerate()
                    .map(|(i, p)| unmap_one(p, format!("item[{i}]")))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(build_list(elements, ExprType::PATH, self.worker_format))
            }
            other => Ok(other),
        }
    }
}

/// `N` or `N[3]`.
fn label(name: &str, task: Option<i64>) -> String {
    match task {
        Some(t) => format!("{name}[{t}]"),
        None => name.to_string(),
    }
}

/// The chunk's values for a message, at most 10 of them.
fn chunk_summary<'a>(values: impl Iterator<Item = &'a Option<i64>>) -> String {
    let values: Vec<i64> = values.flatten().copied().collect();
    let shown: Vec<String> = values.iter().take(10).map(i64::to_string).collect();
    match values.len() {
        0 => "the chunk is empty".to_string(),
        n if n > 10 => format!("chunk values {}, ... ({n} values)", shown.join(", ")),
        _ => format!("chunk values {}", shown.join(", ")),
    }
}

/// `path`, or `list[path]`.
fn is_path_type(ty: &ExprType) -> bool {
    ty.code() == TypeCode::Path
        || (ty.code() == TypeCode::List && ty.params()[0].code() == TypeCode::Path)
}

/// The bytes of the JSON serialization of a stored `path` or `list[path]`
/// value.
fn stored_json_len(value: &ExprValue) -> usize {
    let json = match value {
        ExprValue::Path { value, .. } => serde_json::Value::String(value.clone()),
        ExprValue::ListPath(values, ..) => serde_json::Value::Array(
            values
                .iter()
                .map(|v| serde_json::Value::String(v.clone()))
                .collect(),
        ),
        _ => return 0,
    };
    json.to_string().len()
}

/// A list of `elements` of type `elem_ty`. A `list[path]` takes the format of
/// its first element (`worker_format` when empty); every element's text is
/// kept as stored.
fn build_list(elements: Vec<ExprValue>, elem_ty: ExprType, worker_format: PathFormat) -> ExprValue {
    if elem_ty.code() == TypeCode::Path && elements.is_empty() {
        return ExprValue::ListPath(Vec::new(), worker_format, 0);
    }
    // Every element was converted to `elem_ty`, so the list builds.
    ExprValue::make_list(elements, elem_ty.clone())
        .unwrap_or_else(|_| ExprValue::ListList(Vec::new(), elem_ty, 0))
}
