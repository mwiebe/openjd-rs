// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Shared utilities for CLI commands.

use std::fmt;
use std::io::Read;
use std::path::Path;

use openjd_model::template::parse::DocumentType;

/// Maximum size, in bytes, of a `file://`-referenced input read by the CLI
/// (job parameter files, task files, path-mapping rule files).
///
/// This bounds memory use when the CLI is invoked programmatically with
/// `file://` paths from less-trusted sources. The template itself is bounded
/// separately by the model crate's `CallerLimits::max_template_size`.
///
/// 10 MiB is large enough for any reasonable parameter / task / path-mapping
/// document and small enough to prevent runaway allocation.
pub const MAX_FILE_INPUT_SIZE: u64 = 10 * 1024 * 1024;

/// The CLI's default cap on the character length of a single resolved
/// process argument: 32K characters, on every platform.
///
/// This is an opinionated *reasonable default*, not a promise about any
/// particular operating system's limit. Template Schemas §5.1/§5.2 set
/// no maximum on `command`/`args` but note that "the specific operating
/// system that the command is run on will impose its own maximum
/// length"; those OS limits are measured in different units than this
/// cap (Linux `MAX_ARG_STRLEN` is 131072 **bytes**; the Windows command
/// line is 32767 **UTF-16 code units**), so no character count maps
/// exactly onto all of them. 32K characters is:
///
/// - far beyond any reasonable single argument, while catching
///   template-generated blowups at `check` instead of as an opaque
///   spawn failure on a worker;
/// - conservative for Linux/macOS byte limits (a `char` is at most 4
///   UTF-8 bytes, so 32K characters is at most 128 KiB — within
///   `MAX_ARG_STRLEN`);
/// - approximately the Windows command-line capacity (exceedable in
///   UTF-16 units only by astral-plane characters, which cost two
///   units each).
///
/// The worker's own run-time enforcement and the OS itself remain the
/// authoritative limits.
pub const DEFAULT_MAX_ARG_LEN: usize = 32 * 1024;

/// The CLI's opinionated caller limits: library defaults plus
/// `max_resolved_arg_len` set to [`DEFAULT_MAX_ARG_LEN`]. Library
/// callers of `openjd-model` default to no cap (spec-conforming
/// permissiveness); the CLI applies the default cap at every stage it
/// drives so a hopeless argument fails early with a field-path error.
pub fn caller_limits() -> openjd_model::CallerLimits {
    openjd_model::CallerLimits {
        max_resolved_arg_len: Some(DEFAULT_MAX_ARG_LEN),
        ..Default::default()
    }
}

/// The Steps whose `Steps.*` symbols something in `job` reads: a Step's
/// groups, script, or Step Environments, or a job output.
pub fn read_producers(job: &openjd_model::job::Job) -> std::collections::BTreeSet<String> {
    let mut producers: std::collections::BTreeSet<String> = job
        .steps
        .iter()
        .flat_map(|step| step.input_producers())
        .collect();
    for i in 0..job.outputs.as_ref().map_or(0, Vec::len) {
        for symbol in openjd_model::job_output_inputs(job, i) {
            if let Some(step) = step_of_symbol(&symbol) {
                producers.insert(step.to_string());
            }
        }
    }
    producers
}

/// `P` for a `Steps.P.<Member>.<Name>` symbol.
pub fn step_of_symbol(symbol: &str) -> Option<&str> {
    symbol.strip_prefix("Steps.")?.split('.').next()
}

/// Whether `Steps.*` symbol `symbol` (`Steps.<Step>.<Member>.<Name>`) is
/// concrete once `create_job` returns: every symbol of a skipped Step, and
/// the `TaskParam` and `TaskRawParam` symbols of a Step whose Tasks are
/// known. Decided from the Steps' bindings, without building any value.
pub fn concrete_at_creation(job: &openjd_model::job::Job, symbol: &str) -> bool {
    let mut parts = symbol.strip_prefix("Steps.").unwrap_or("").splitn(3, '.');
    let (Some(p), Some(member)) = (parts.next(), parts.next()) else {
        return false;
    };
    let Some(step) = job.steps.iter().find(|s| s.name == p) else {
        return false;
    };
    matches!(step.binding, openjd_model::job::StepBinding::Skipped)
        || (matches!(member, "TaskParam" | "TaskRawParam") && step.tasks_known().is_some())
}

/// What is known of a pending Step, one line per group, as RFC 0011
/// §Implementation strategy asks a tool to report it (plan C2):
///
/// * `Enabled: yes` or `Enabled: determined at run time from <symbols>`;
/// * `Tasks: <N> (known)`, with `; Step pending inputs for <symbols>` when
///   another group still waits, or `Tasks: determined at run time from
///   <symbols>` (`and enableIf`, or `(waiting for enableIf)` alone);
/// * `Host requirements: known`, or as for Tasks.
///
/// Empty for a Step that is not pending.
pub fn binding_lines(step: &openjd_model::job::Step) -> Vec<String> {
    use openjd_model::job::GroupReport;
    let Some(report) = step.binding_report() else {
        return Vec::new();
    };
    fn waiting(group: &GroupReport) -> String {
        match group {
            GroupReport::Known => String::new(),
            GroupReport::Waiting { on, for_enablement } => match (on.is_empty(), for_enablement) {
                (true, _) => "determined at run time (waiting for enableIf)".to_string(),
                (false, false) => format!("determined at run time from {}", on.join(", ")),
                (false, true) => {
                    format!("determined at run time from {} and enableIf", on.join(", "))
                }
            },
        }
    }
    let mut lines = Vec::new();
    lines.push(match &report.enablement {
        GroupReport::Known => "Enabled: yes".to_string(),
        other => format!("Enabled: {}", waiting(other)),
    });
    lines.push(match (&report.tasks, step.tasks_known()) {
        (GroupReport::Known, Some(n)) => {
            let mut others: Vec<&String> = Vec::new();
            for group in [&report.host_requirements, &report.remaining] {
                if let GroupReport::Waiting { on, .. } = group {
                    others.extend(on);
                }
            }
            others.sort();
            others.dedup();
            if others.is_empty() {
                format!("Tasks: {n} (known)")
            } else {
                let others: Vec<&str> = others.iter().map(|s| s.as_str()).collect();
                format!(
                    "Tasks: {n} (known; Step pending inputs for {})",
                    others.join(", ")
                )
            }
        }
        (group, _) => format!("Tasks: {}", waiting(group)),
    });
    lines.push(match &report.host_requirements {
        GroupReport::Known => "Host requirements: known".to_string(),
        other => format!("Host requirements: {}", waiting(other)),
    });
    lines
}

/// A value as JSON, for `--output json|yaml`: a `RangeExpr` as its string,
/// a path as its text.
pub fn expr_value_to_json(value: &openjd_expr::ExprValue) -> serde_json::Value {
    use openjd_expr::ExprValue;
    match value {
        ExprValue::Null => serde_json::Value::Null,
        ExprValue::Bool(b) => (*b).into(),
        ExprValue::Int(i) => (*i).into(),
        ExprValue::Float(f) => serde_json::Number::from_f64(f.value())
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| f.to_display_string().into()),
        v if v.is_list() => serde_json::Value::Array(
            v.list_elements()
                .unwrap_or_default()
                .iter()
                .map(expr_value_to_json)
                .collect(),
        ),
        other => other.to_display_string().into(),
    }
}

/// A value for a log line: JSON-style, with strings and paths quoted and a
/// float in its own spelling, so `"a=b"` and `1.50` read as reported.
pub fn display_value(value: &openjd_expr::ExprValue) -> String {
    use openjd_expr::ExprValue;
    match value {
        ExprValue::Null => "null".to_string(),
        ExprValue::String(s) | ExprValue::Path { value: s, .. } => {
            serde_json::Value::String(s.clone()).to_string()
        }
        ExprValue::RangeExpr(r) => serde_json::Value::String(r.to_string()).to_string(),
        v if v.is_list() => format!(
            "[{}]",
            v.list_elements()
                .unwrap_or_default()
                .iter()
                .map(display_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_display_string(),
    }
}

/// `value` followed by ` <unit>` when there is one.
pub fn with_unit(value: &openjd_expr::ExprValue, unit: Option<&str>) -> String {
    match unit {
        Some(unit) if !matches!(value, openjd_expr::ExprValue::Null) => {
            format!("{} {unit}", display_value(value))
        }
        _ => display_value(value),
    }
}

/// Infer the template document format from its filename extension.
pub fn document_type(path: &Path) -> DocumentType {
    if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
        DocumentType::Json
    } else {
        DocumentType::Yaml
    }
}

/// Read a file's contents as a UTF-8 string, mapping I/O errors to descriptive
/// CLI error messages and optionally enforcing a maximum file size.
///
/// When `max_size` is `Some(cap)`, reads at most `cap + 1` bytes via
/// [`Read::take`]; if the resulting buffer exceeds `cap` the file is
/// rejected with an "exceeds maximum size" error. Allocation is bounded
/// to `cap + 1` bytes regardless of how large the underlying file is.
///
/// No separate existence / type check precedes the read — the open itself
/// is the only filesystem access on the happy path, eliminating TOCTOU
/// between a check and the read. A best-effort `metadata()` call on the
/// failed-open path is used *only* to refine the error message (e.g. to
/// distinguish a directory from a generic I/O error); it is not a security
/// control.
///
/// Error messages:
///
/// * NotFound → `not_found_msg()`
/// * Directory → `"'<path>' is not a file."`
/// * Exceeds `max_size` → `"File '<path>' exceeds maximum size of <N> bytes."`
/// * Any other open / read error → `read_err_prefix()` followed by `": <source>"`
pub fn read_input_file_with<FN, FE>(
    path: &Path,
    max_size: Option<u64>,
    not_found_msg: FN,
    read_err_prefix: FE,
) -> Result<String, String>
where
    FN: FnOnce() -> String,
    FE: FnOnce() -> String,
{
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found_msg()),
        Err(e) => {
            // Best-effort: a failed open on a directory typically reports
            // IsADirectory on Linux but Other on some platforms, so use
            // metadata() to produce a stable diagnostic.
            if std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false) {
                return Err(format!("'{}' is not a file.", path.display()));
            }
            return Err(format!("{}: {e}", read_err_prefix()));
        }
    };

    // On Linux, File::open succeeds on a directory and the read fails with
    // IsADirectory. Detect that up-front via the open file's metadata so we
    // produce the same "is not a file" message on every platform.
    if file.metadata().map(|m| m.is_dir()).unwrap_or(false) {
        return Err(format!("'{}' is not a file.", path.display()));
    }

    let mut buf = String::new();
    match max_size {
        Some(cap) => {
            // Read cap + 1 bytes. If the file is larger than the cap,
            // we'll have read cap + 1 bytes; if it's cap or fewer, we'll
            // have read the whole file. Bounded allocation either way.
            let limit = cap.saturating_add(1);
            file.take(limit)
                .read_to_string(&mut buf)
                .map_err(|e| format!("{}: {e}", read_err_prefix()))?;
            if buf.len() as u64 > cap {
                return Err(format!(
                    "File '{}' exceeds maximum size of {} bytes.",
                    path.display(),
                    cap
                ));
            }
        }
        None => {
            let mut file = file;
            file.read_to_string(&mut buf)
                .map_err(|e| format!("{}: {e}", read_err_prefix()))?;
        }
    }
    Ok(buf)
}

/// Convenience wrapper for template-style reads: no size cap and the standard
/// `'<path>' does not exist.` / `Cannot read '<path>'` messages used by the
/// `check`, `summary`, `run`, and `help` subcommands.
pub fn read_input_file(path: &Path) -> Result<String, String> {
    read_input_file_with(
        path,
        None,
        || format!("'{}' does not exist.", path.display()),
        || format!("Cannot read '{}'", path.display()),
    )
}

/// The full default list of supported OpenJD extension names,
/// sourced from the authoritative [`openjd_model::ModelExtension::ALL`].
/// Using the enum rather than a CLI-local string array keeps the
/// CLI, JS bindings, and Python bindings in sync whenever a new
/// extension is added upstream — one place to update.
fn supported_extensions() -> Vec<&'static str> {
    openjd_model::ModelExtension::ALL
        .iter()
        .map(|e| e.as_str())
        .collect()
}

/// A CLI command result that can be rendered as JSON, YAML, or human-readable text.
///
/// This mirrors Python's `OpenJDCliResult` / `@print_cli_result` pattern:
/// each command returns a result struct, and output formatting is handled
/// uniformly by [`print_cli_result`].
pub trait CliResult: fmt::Display {
    /// Serialize to a JSON value for structured output.
    ///
    /// Fields with `None` / empty values should be omitted to match
    /// Python's `_asdict_omit_null` behavior.
    fn to_json_value(&self) -> serde_json::Value;
}

/// Format and print a [`CliResult`] according to the user's `--output` choice.
///
/// Equivalent to Python's `@print_cli_result` decorator.
pub fn print_cli_result(result: &dyn CliResult, format: &str) {
    match format {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&result.to_json_value()).unwrap()
        ),
        "yaml" => print!(
            "{}",
            serde_saphyr::to_string(&result.to_json_value()).unwrap()
        ),
        _ => println!("{result}"),
    }
}

/// Parse and validate the `--extensions` argument.
/// Returns an error if any extension name is not recognized.
pub fn parse_extensions(arg: &Option<String>) -> Result<Vec<String>, String> {
    let supported = supported_extensions();
    match arg {
        Some(ext_str) if ext_str.is_empty() => Ok(vec![]),
        Some(ext_str) => {
            let exts: Vec<String> = ext_str
                .split(',')
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty())
                .collect();
            let unsupported: Vec<&str> = exts
                .iter()
                .filter(|e| !supported.contains(&e.as_str()))
                .map(|e| e.as_str())
                .collect();
            if !unsupported.is_empty() {
                return Err(format!(
                    "Unsupported Open Job Description extension(s): {}",
                    unsupported.join(", ")
                ));
            }
            Ok(exts)
        }
        None => Ok(supported.iter().map(|s| s.to_string()).collect()),
    }
}

#[cfg(test)]
mod limits_tests {
    use super::*;

    #[test]
    fn default_max_arg_len_is_32k_on_every_platform() {
        // A uniform, opinionated default — not an exact encoding of any
        // OS's limit (those are measured in bytes / UTF-16 units).
        assert_eq!(DEFAULT_MAX_ARG_LEN, 32768);
    }

    #[test]
    fn cli_caller_limits_cap_resolved_args_only() {
        // CLI policy: an opinionated resolved-argument cap; everything
        // else stays at the library default (None).
        let limits = caller_limits();
        assert_eq!(limits.max_resolved_arg_len, Some(DEFAULT_MAX_ARG_LEN));
        assert_eq!(limits.max_resolved_data_len, None);
        assert_eq!(limits.max_eval_memory_bytes, None);
        assert_eq!(limits.max_eval_operations, None);
    }
}
