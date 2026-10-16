// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The `CONTROL_FLOW` output collector (RFC 0010 §Processing a directive,
//! §Collect modes, §Chunked Steps, §Path values, §Limits):
//! `outputs::OutputCollector`. See `specs/model/outputs.md`.

use indexmap::IndexMap;
use openjd_expr::path_mapping::{PathFormat, PathMappingRule};
use openjd_expr::value::Float64;
use openjd_expr::{ExprType, ExprValue, RangeExpr};
use openjd_model::job::OutputVariable;
use openjd_model::outputs::{
    parse_output_directive, OutputCollector, OutputDirective, OutputError, OutputErrorKind,
    TaskOutputs, MIN_OUTPUT_BYTES_LIMIT,
};
use openjd_model::template::OutputVariableDefinition;

// ══════════════════════════════════════════════════════════════
// Helpers
// ══════════════════════════════════════════════════════════════

/// One declaration from flow YAML, as `create_job` stores it.
fn var(s: &str) -> OutputVariable {
    let v: serde_json::Value = serde_saphyr::from_str(s).unwrap();
    serde_json::from_value::<OutputVariableDefinition>(v)
        .expect("valid output variable definition")
        .to_job_output_variable()
}

fn collector(decls: &[&str]) -> OutputCollector {
    let declared: Vec<OutputVariable> = decls.iter().map(|d| var(d)).collect();
    OutputCollector::new(
        &declared,
        MIN_OUTPUT_BYTES_LIMIT,
        &[],
        PathFormat::Posix,
        None,
    )
    .unwrap()
}

fn chunked(decls: &[&str], chunk: &[i64]) -> OutputCollector {
    let declared: Vec<OutputVariable> = decls.iter().map(|d| var(d)).collect();
    OutputCollector::new(
        &declared,
        MIN_OUTPUT_BYTES_LIMIT,
        &[],
        PathFormat::Posix,
        Some(chunk.to_vec()),
    )
    .unwrap()
}

fn with_limit(decls: &[&str], max_bytes: usize, chunk: Option<Vec<i64>>) -> OutputCollector {
    let declared: Vec<OutputVariable> = decls.iter().map(|d| var(d)).collect();
    OutputCollector::new(&declared, max_bytes, &[], PathFormat::Posix, chunk).unwrap()
}

fn with_rules(decls: &[&str], rules: &[PathMappingRule], worker: PathFormat) -> OutputCollector {
    let declared: Vec<OutputVariable> = decls.iter().map(|d| var(d)).collect();
    OutputCollector::new(&declared, MIN_OUTPUT_BYTES_LIMIT, rules, worker, None).unwrap()
}

fn rule(format: PathFormat, src: &str, dst: &str) -> PathMappingRule {
    PathMappingRule {
        source_path_format: format,
        source_path: src.to_string(),
        destination_path: dst.to_string(),
    }
}

/// Process one directive (the text after `openjd_output_var: `) the way a
/// runtime does for an inline value. `@` directives are processed with
/// [`file`].
fn directive(c: &mut OutputCollector, payload: &str) -> Result<ExprValue, OutputError> {
    match parse_output_directive(payload)? {
        OutputDirective::Inline { name, task, json } => {
            c.check_name(name, task)?;
            c.accept(name, task, json.as_bytes(), false)
        }
        OutputDirective::File { .. } => panic!("use file() for @ directives"),
    }
}

/// Process an `@` value whose file holds `content`.
fn file(
    c: &mut OutputCollector,
    name: &str,
    task: Option<i64>,
    content: &[u8],
) -> Result<ExprValue, OutputError> {
    c.check_name(name, task)?;
    c.accept(name, task, content, true)
}

fn ok(c: &mut OutputCollector, payload: &str) -> ExprValue {
    directive(c, payload).unwrap_or_else(|e| panic!("{payload}: {e}"))
}

fn err(c: &mut OutputCollector, payload: &str, kind: OutputErrorKind) -> String {
    let e = directive(c, payload).expect_err(payload);
    assert_eq!(e.kind(), kind, "{payload}: {e}");
    e.to_string()
}

/// The value of one declared output of an unchunked Step, from one directive.
fn convert(decl: &str, payload: &str) -> Result<ExprValue, OutputError> {
    let mut c = collector(&[decl]);
    directive(&mut c, payload)
}

fn finished(c: OutputCollector) -> IndexMap<Option<i64>, TaskOutputs> {
    c.finish().unwrap_or_else(|e| panic!("finish: {e}"))
}

fn only_task(c: OutputCollector) -> TaskOutputs {
    let mut all = finished(c);
    assert_eq!(all.len(), 1);
    all.swap_remove(&None).expect("unchunked")
}

fn float(v: f64) -> ExprValue {
    ExprValue::Float(Float64::new(v).unwrap())
}

fn strings(items: &[&str]) -> ExprValue {
    ExprValue::make_list(
        items
            .iter()
            .map(|s| ExprValue::String(s.to_string()))
            .collect(),
        ExprType::STRING,
    )
    .unwrap()
}

fn int_lists(items: &[&[i64]]) -> ExprValue {
    ExprValue::make_list(
        items
            .iter()
            .map(|v| ExprValue::ListInt(v.to_vec()))
            .collect(),
        ExprType::list(ExprType::INT),
    )
    .unwrap()
}

/// The text and format of a stored `path` value.
fn path_parts(v: &ExprValue) -> (String, PathFormat) {
    match v {
        ExprValue::Path { value, format, .. } => (value.clone(), *format),
        other => panic!("not a path: {other:?}"),
    }
}

// ══════════════════════════════════════════════════════════════
// The RFC's conversion table, row by row
// (§Processing a directive, "Type conversion")
// ══════════════════════════════════════════════════════════════

#[test]
fn table_int_from_int() {
    assert_eq!(
        convert("{name: 'N', type: INT}", "N=12"),
        Ok(ExprValue::Int(12))
    );
}

#[test]
fn table_int_from_whole_float() {
    assert_eq!(
        convert("{name: 'N', type: INT}", "N=12.0"),
        Ok(ExprValue::Int(12))
    );
}

#[test]
fn table_int_from_fraction_is_invalid() {
    let mut c = collector(&["{name: 'N', type: INT}"]);
    assert_eq!(
        err(&mut c, "N=12.5", OutputErrorKind::Conversion),
        "Output variable 'N': value (float) '12.5' does not convert to int: Cannot coerce \
         float to int: 12.5 is not a whole number"
    );
}

#[test]
fn table_float_from_int() {
    let v = convert("{name: X, type: FLOAT}", "X=3").unwrap();
    assert_eq!(v, float(3.0));
    assert_eq!(v.to_display_string(), "3.0");
}

#[test]
fn table_string_with_equals_sign() {
    assert_eq!(
        convert("{name: S, type: STRING}", "S=\"a=b\""),
        Ok(ExprValue::String("a=b".into()))
    );
}

#[test]
fn table_string_from_int() {
    assert_eq!(
        convert("{name: S, type: STRING}", "S=123"),
        Ok(ExprValue::String("123".into()))
    );
}

#[test]
fn table_bool() {
    assert_eq!(
        convert("{name: B, type: BOOL}", "B=true"),
        Ok(ExprValue::Bool(true))
    );
}

#[test]
fn table_range_expr_from_string() {
    let v = convert("{name: R, type: RANGE_EXPR}", "R=\"1-10:2\"").unwrap();
    assert_eq!(
        v,
        ExprValue::RangeExpr("1-10:2".parse::<RangeExpr>().unwrap())
    );
}

#[test]
fn table_range_expr_from_list_is_invalid() {
    let mut c = collector(&["{name: R, type: RANGE_EXPR}"]);
    assert_eq!(
        err(&mut c, "R=[1, 3, 5]", OutputErrorKind::Conversion),
        "Output variable 'R': value (list[int]) '[1, 3, 5]' does not convert to range_expr: \
         Cannot coerce list[int] to range_expr"
    );
}

#[test]
fn table_empty_list_int() {
    assert_eq!(
        convert("{name: L, type: 'LIST[INT]'}", "L=[]"),
        Ok(ExprValue::ListInt(vec![]))
    );
}

#[test]
fn table_list_float_from_mixed() {
    assert_eq!(
        convert("{name: L, type: 'LIST[FLOAT]'}", "L=[1, 2.5]"),
        Ok(ExprValue::ListFloat(vec![
            Float64::new(1.0).unwrap(),
            Float64::new(2.5).unwrap()
        ]))
    );
}

#[test]
fn table_list_string_from_mixed() {
    assert_eq!(
        convert("{name: L, type: 'LIST[STRING]'}", "L=[1, \"a\"]"),
        Ok(strings(&["1", "a"]))
    );
}

#[test]
fn table_list_with_null_is_invalid() {
    let mut c = collector(&["{name: L, type: 'LIST[INT]'}"]);
    assert_eq!(
        err(&mut c, "L=[1, null]", OutputErrorKind::Conversion),
        "Output variable 'L': item[1] is null; output values cannot be or contain null"
    );
}

#[test]
fn table_list_list_int() {
    let v = convert("{name: G, type: 'LIST[LIST[INT]]'}", "G=[[1], []]").unwrap();
    assert_eq!(v, int_lists(&[&[1], &[]]));
    assert_eq!(v.expr_type(), ExprType::list(ExprType::list(ExprType::INT)));
}

#[test]
fn table_append_floats() {
    let mut c = collector(&["{name: E, type: 'LIST[FLOAT]', collect: APPEND}"]);
    assert_eq!(ok(&mut c, "E=1"), float(1.0));
    assert_eq!(ok(&mut c, "E=2.5"), float(2.5));
    assert_eq!(
        only_task(c)["E"],
        ExprValue::ListFloat(vec![Float64::new(1.0).unwrap(), Float64::new(2.5).unwrap()])
    );
}

#[test]
fn table_append_list_element_is_invalid() {
    let mut c = collector(&["{name: E, type: 'LIST[FLOAT]', collect: APPEND}"]);
    assert_eq!(
        err(&mut c, "E=[1]", OutputErrorKind::Conversion),
        "Output variable 'E': element (list[int]) '[1]' does not convert to float: Cannot \
         coerce list[int] to float"
    );
}

#[test]
fn table_append_list_list_int() {
    let mut c = collector(&["{name: G, type: 'LIST[LIST[INT]]', collect: APPEND}"]);
    assert_eq!(ok(&mut c, "G=[1]"), ExprValue::ListInt(vec![1]));
    assert_eq!(ok(&mut c, "G=[]"), ExprValue::ListInt(vec![]));
    let v = only_task(c).swap_remove("G").unwrap();
    assert_eq!(v, int_lists(&[&[1], &[]]));
    assert_eq!(v.expr_type(), ExprType::list(ExprType::list(ExprType::INT)));
}

#[test]
fn table_path_is_mapped_back() {
    let rules = [rule(PathFormat::Windows, "P:\\", "/mnt/proj")];
    let mut c = with_rules(&["{name: P, type: PATH}"], &rules, PathFormat::Posix);
    let v = ok(&mut c, "P=\"/mnt/proj/shot01/out\"");
    assert_eq!(
        path_parts(&v),
        ("P:\\shot01\\out".to_string(), PathFormat::Windows)
    );
}

#[test]
fn table_path_from_int_is_invalid() {
    let mut c = collector(&["{name: P, type: PATH}"]);
    assert_eq!(
        err(&mut c, "P=7", OutputErrorKind::Conversion),
        "Output variable 'P': value (int) '7' does not convert to path: Cannot coerce int to \
         path"
    );
}

#[test]
fn table_null_is_invalid_for_every_type() {
    for ty in [
        "INT",
        "FLOAT",
        "STRING",
        "PATH",
        "BOOL",
        "RANGE_EXPR",
        "'LIST[INT]'",
        "'LIST[FLOAT]'",
        "'LIST[STRING]'",
        "'LIST[PATH]'",
        "'LIST[BOOL]'",
        "'LIST[LIST[INT]]'",
    ] {
        let mut c = collector(&[&format!("{{name: X, type: {ty}}}")]);
        assert_eq!(
            err(&mut c, "X=null", OutputErrorKind::Conversion),
            "Output variable 'X': value is null; output values cannot be or contain null",
            "{ty}"
        );
    }
}

// ══════════════════════════════════════════════════════════════
// Typing and conversion beyond the table
// ══════════════════════════════════════════════════════════════

#[test]
fn object_anywhere_is_invalid() {
    let mut c = collector(&["{name: G, type: 'LIST[LIST[INT]]'}"]);
    assert_eq!(
        err(&mut c, "G={}", OutputErrorKind::Conversion),
        "Output variable 'G': value is a JSON object; output values cannot be or contain \
         objects"
    );
    assert_eq!(
        err(
            &mut c,
            "G=[[1], [2, {\"a\": 1}]]",
            OutputErrorKind::Conversion
        ),
        "Output variable 'G': item[1][1] is a JSON object; output values cannot be or contain \
         objects"
    );
    assert_eq!(
        err(&mut c, "G=[[null]]", OutputErrorKind::Conversion),
        "Output variable 'G': item[0][0] is null; output values cannot be or contain null"
    );
}

#[test]
fn nesting_deeper_than_the_target_is_invalid() {
    let mut c = collector(&["{name: L, type: 'LIST[INT]'}"]);
    assert_eq!(
        err(&mut c, "L=[1, [2]]", OutputErrorKind::Conversion),
        "Output variable 'L': item[1] (list[int]) '[2]' does not convert to int: Cannot \
         coerce list[int] to int"
    );
    let mut c = collector(&["{name: G, type: 'LIST[LIST[INT]]'}"]);
    assert_eq!(
        err(&mut c, "G=[[[1]]]", OutputErrorKind::Conversion),
        "Output variable 'G': item[0][0] (list[int]) '[1]' does not convert to int: Cannot \
         coerce list[int] to int"
    );
    let mut c = collector(&["{name: 'N', type: INT}"]);
    assert_eq!(
        err(&mut c, "N=[1]", OutputErrorKind::Conversion),
        "Output variable 'N': value (list[int]) '[1]' does not convert to int: Cannot coerce \
         list[int] to int"
    );
}

#[test]
fn a_heterogeneous_list_against_a_scalar_is_described_as_a_list() {
    let mut c = collector(&["{name: 'N', type: INT}"]);
    assert_eq!(
        err(&mut c, "N=[1, \"a\"]", OutputErrorKind::Conversion),
        "Output variable 'N': value (list) does not convert to int"
    );
}

#[test]
fn scalar_against_a_list_is_invalid() {
    let mut c = collector(&["{name: L, type: 'LIST[STRING]'}"]);
    assert_eq!(
        err(&mut c, "L=\"a\"", OutputErrorKind::Conversion),
        "Output variable 'L': value (string) 'a' does not convert to list[string]: Cannot \
         coerce string to list[string]"
    );
}

#[test]
fn list_element_conversion_failure_names_the_item() {
    let mut c = collector(&["{name: L, type: 'LIST[INT]'}"]);
    assert_eq!(
        err(&mut c, "L=[1, \"x\"]", OutputErrorKind::Conversion),
        "Output variable 'L': item[1] (string) 'x' does not convert to int: Cannot convert \
         'x' to int: invalid digit found in string"
    );
}

#[test]
fn numbers_keep_their_written_digits() {
    // Plan ambiguity A6: the prototype keeps the spelling, as a FLOAT job
    // parameter value does.
    assert_eq!(
        convert("{name: S, type: STRING}", "S=1.50"),
        Ok(ExprValue::String("1.50".into()))
    );
    assert_eq!(
        convert("{name: S, type: STRING}", "S=12345678901234567890"),
        Ok(ExprValue::String("12345678901234567890".into()))
    );
    assert_eq!(
        convert("{name: S, type: STRING}", "S=1E3"),
        Ok(ExprValue::String("1E3".into()))
    );
    assert_eq!(
        convert("{name: S, type: STRING}", "S=1e+5"),
        Ok(ExprValue::String("1e+5".into()))
    );
    assert_eq!(
        convert("{name: S, type: STRING}", "S=2E-3"),
        Ok(ExprValue::String("2E-3".into()))
    );
    let f = convert("{name: X, type: FLOAT}", "X=1e+5").unwrap();
    assert_eq!(f, float(100000.0));
    assert_eq!(f.to_display_string(), "1e+5");
    let f = convert("{name: X, type: FLOAT}", "X=1.50").unwrap();
    assert_eq!(f, float(1.5));
    assert_eq!(f.to_display_string(), "1.50");
    let l = convert("{name: L, type: 'LIST[STRING]'}", "L=[\"1.5\", 2.50, -0.0]").unwrap();
    assert_eq!(l, strings(&["1.5", "2.50", "0.0"]));
}

#[test]
fn number_spellings_skip_strings() {
    // Digits and '-' inside strings, including after escapes, are not numbers.
    let l = convert(
        "{name: L, type: 'LIST[STRING]'}",
        r#"L=["-1", "a\"2", "\\", 3.0, "\u0034", 5e0]"#,
    )
    .unwrap();
    assert_eq!(l, strings(&["-1", "a\"2", "\\", "3.0", "4", "5e0"]));
}

#[test]
fn strings_keep_their_exact_text() {
    assert_eq!(
        convert("{name: S, type: STRING}", "S=\"  a \\t \""),
        Ok(ExprValue::String("  a \t ".into()))
    );
}

#[test]
fn inline_values_have_no_bom_and_nul_is_a_json_error() {
    // A BOM is allowed only at the start of an @ file (RFC 0010 §File
    // values item 4), and a NUL is a file-content problem only for files.
    let mut c = collector(&["{name: 'N', type: INT}"]);
    let e = c.accept("N", None, b"\xEF\xBB\xBF1", false).unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Json);
    assert_eq!(
        e.to_string(),
        "Output variable 'N': '\u{feff}1' is not one valid JSON value: expected value at line 1 \
         column 1"
    );
    let e = c.accept("N", None, b"1\0", false).unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Json);
    assert_eq!(
        e.to_string(),
        "Output variable 'N': '1\\0' is not one valid JSON value: trailing characters at line 1 \
         column 2"
    );
}

#[test]
fn min_output_bytes_limit_is_one_mebibyte() {
    assert_eq!(MIN_OUTPUT_BYTES_LIMIT, 1 << 20);
}

#[test]
fn integer_spellings() {
    // `-0` has neither fraction nor exponent, so it is the int 0.
    assert_eq!(
        convert("{name: S, type: STRING}", "S=-0"),
        Ok(ExprValue::String("0".into()))
    );
    assert_eq!(
        convert("{name: 'N', type: INT}", "N=-9223372036854775808"),
        Ok(ExprValue::Int(i64::MIN))
    );
    // Outside i64, a number is a float, which overflows on conversion to int.
    let mut c = collector(&["{name: 'N', type: INT}"]);
    assert_eq!(
        err(&mut c, "N=9223372036854775808", OutputErrorKind::Conversion),
        "Output variable 'N': value (float) '9223372036854775808' does not convert to int: \
         Integer overflow: result is outside the 64-bit signed range"
    );
    assert_eq!(
        convert("{name: 'N', type: INT}", "N=1e2"),
        Ok(ExprValue::Int(100))
    );
}

#[test]
fn non_finite_numbers_are_invalid() {
    for (text, detail) in [
        ("NaN", "expected value at line 1 column 1"),
        ("Infinity", "expected value at line 1 column 1"),
        ("-Infinity", "invalid number at line 1 column 2"),
    ] {
        let mut c = collector(&["{name: X, type: FLOAT}"]);
        assert_eq!(
            err(&mut c, &format!("X={text}"), OutputErrorKind::Json),
            format!("Output variable 'X': '{text}' is not one valid JSON value: {detail}")
        );
    }
}

#[test]
fn numbers_too_large_for_a_float_do_not_convert() {
    // Valid JSON (RFC 8259), but RFC 0010 makes it a conversion error.
    let mut c = collector(&["{name: X, type: FLOAT}"]);
    assert_eq!(
        err(&mut c, "X=1e400", OutputErrorKind::Conversion),
        "Output variable 'X': '1e400' has a number too large to represent as a float (at line \
         1 column 5)"
    );
    let mut c = collector(&["{name: S, type: 'LIST[STRING]'}"]);
    assert_eq!(
        err(&mut c, "S=[1, -1e400]", OutputErrorKind::Conversion),
        "Output variable 'S': '[1, -1e400]' has a number too large to represent as a float \
         (at line 1 column 10)"
    );
    let e = file(&mut c, "S", None, b"[\n1e999]").unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Conversion);
    assert_eq!(
        e.to_string(),
        "Output variable 'S': the @ file's content has a number too large to represent as a \
         float (at line 2 column 5)"
    );
}

#[test]
fn nesting_past_the_json_recursion_limit_does_not_convert() {
    let deep = format!("{}1{}", "[".repeat(130), "]".repeat(130));
    let mut c = collector(&["{name: G, type: 'LIST[LIST[INT]]'}"]);
    assert_eq!(
        err(&mut c, &format!("G={deep}"), OutputErrorKind::Conversion),
        format!(
            "Output variable 'G': '{}1{}...' nests deeper than any output type (at line 1 \
             column 128)",
            "[".repeat(130),
            "]".repeat(69)
        )
    );
    let e = file(&mut c, "G", None, deep.as_bytes()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "Output variable 'G': the @ file's content nests deeper than any output type (at line \
         1 column 128)"
    );
    // Below the limit, the depth is a conversion against the target.
    let deep = format!("{}1{}", "[".repeat(127), "]".repeat(127));
    assert_eq!(
        err(&mut c, &format!("G={deep}"), OutputErrorKind::Conversion),
        "Output variable 'G': item[0][0] (list) does not convert to int"
    );
}

#[test]
fn lone_surrogate_escape_is_invalid_json() {
    let mut c = collector(&["{name: S, type: STRING}"]);
    assert_eq!(
        err(&mut c, "S=\"\\udc00\"", OutputErrorKind::Json),
        "Output variable 'S': '\"\\udc00\"' is not one valid JSON value: lone leading \
         surrogate in hex escape at line 1 column 7"
    );
    let e = file(&mut c, "S", None, b"\"\\ud800\\u0041 x\"").unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Json);
    assert_eq!(
        e.to_string(),
        "Output variable 'S': the @ file's content is not one valid JSON value: lone leading \
         surrogate in hex escape at line 1 column 13"
    );
}

#[test]
fn an_empty_list_is_described_as_such() {
    let mut c = collector(&["{name: L, type: 'LIST[INT]'}"]);
    assert_eq!(
        err(&mut c, "L=[[]]", OutputErrorKind::Conversion),
        "Output variable 'L': item[0] (empty list) '[]' does not convert to int: Cannot coerce \
         list[nulltype] to int"
    );
    let e = file(&mut c, "L", None, b"[[]]").unwrap_err();
    assert_eq!(
        e.to_string(),
        "Output variable 'L': item[0] (empty list) does not convert to int"
    );
}

#[test]
fn invalid_json_after_a_huge_number_is_invalid_json() {
    // Validity is decided before conversion, so the text's real problem is
    // reported.
    let mut c = collector(&["{name: X, type: FLOAT}"]);
    assert_eq!(
        err(&mut c, "X=1e400 garbage", OutputErrorKind::Json),
        "Output variable 'X': '1e400 garbage' is not one valid JSON value: trailing characters \
         at line 1 column 7"
    );
    assert_eq!(
        err(&mut c, "X=[1e400", OutputErrorKind::Json),
        "Output variable 'X': '[1e400' is not one valid JSON value: EOF while parsing a list at \
         line 1 column 6"
    );
}

#[test]
fn invalid_json() {
    let mut c = collector(&["{name: S, type: STRING}"]);
    assert_eq!(
        err(&mut c, "S=abc", OutputErrorKind::Json),
        "Output variable 'S': 'abc' is not one valid JSON value: expected value at line 1 \
         column 1"
    );
    assert_eq!(
        err(&mut c, "S=1 2", OutputErrorKind::Json),
        "Output variable 'S': '1 2' is not one valid JSON value: trailing characters at line \
         1 column 3"
    );
    assert_eq!(
        err(&mut c, "S=\"a", OutputErrorKind::Json),
        "Output variable 'S': '\"a' is not one valid JSON value: EOF while parsing a string \
         at line 1 column 2"
    );
}

#[test]
fn long_inline_text_is_shortened_in_the_message() {
    let mut c = collector(&["{name: S, type: STRING}"]);
    let text = format!("\"{}", "a".repeat(500));
    let msg = err(&mut c, &format!("S={text}"), OutputErrorKind::Json);
    let quoted: String = text.chars().take(200).collect();
    assert_eq!(
        msg,
        format!(
            "Output variable 'S': '{quoted}...' is not one valid JSON value: EOF while \
             parsing a string at line 1 column 501"
        )
    );
}

#[test]
fn expr_coercion_not_job_parameter_coercion() {
    // Plan ambiguity A7: EXPR's implicit coercion, so `1` is not a bool but
    // the strings EXPR accepts for bool and int are.
    let mut c = collector(&["{name: B, type: BOOL}"]);
    assert_eq!(
        err(&mut c, "B=1", OutputErrorKind::Conversion),
        "Output variable 'B': value (int) '1' does not convert to bool: Cannot coerce int to \
         bool"
    );
    assert_eq!(
        convert("{name: B, type: BOOL}", "B=\"yes\""),
        Ok(ExprValue::Bool(true))
    );
    assert_eq!(
        convert("{name: 'N', type: INT}", "N=\"12\""),
        Ok(ExprValue::Int(12))
    );
    assert_eq!(
        convert("{name: S, type: STRING}", "S=true"),
        Ok(ExprValue::String("true".into()))
    );
}

#[test]
fn every_list_type() {
    assert_eq!(
        convert("{name: L, type: 'LIST[BOOL]'}", "L=[true, false]"),
        Ok(ExprValue::ListBool(vec![true, false]))
    );
    assert_eq!(
        convert("{name: L, type: 'LIST[INT]'}", "L=[1, 2.0]"),
        Ok(ExprValue::ListInt(vec![1, 2]))
    );
    let l = convert("{name: L, type: 'LIST[PATH]'}", "L=[\"/a\", \"/b\"]").unwrap();
    assert_eq!(l.expr_type(), ExprType::list(ExprType::PATH));
    assert_eq!(l.to_display_string(), "[\"/a\", \"/b\"]");
    assert_eq!(
        convert("{name: L, type: 'LIST[LIST[INT]]'}", "L=[]")
            .unwrap()
            .expr_type(),
        ExprType::list(ExprType::list(ExprType::INT))
    );
}

#[test]
fn empty_lists_have_the_declared_type() {
    for (ty, expected) in [
        ("'LIST[INT]'", ExprType::list(ExprType::INT)),
        ("'LIST[FLOAT]'", ExprType::list(ExprType::FLOAT)),
        ("'LIST[STRING]'", ExprType::list(ExprType::STRING)),
        ("'LIST[PATH]'", ExprType::list(ExprType::PATH)),
        ("'LIST[BOOL]'", ExprType::list(ExprType::BOOL)),
    ] {
        let v = convert(&format!("{{name: L, type: {ty}}}"), "L=[]").unwrap();
        assert_eq!(v.expr_type(), expected, "{ty}");
    }
}

// ══════════════════════════════════════════════════════════════
// Names and collect modes
// ══════════════════════════════════════════════════════════════

#[test]
fn undeclared_name() {
    let mut c = collector(&["{name: A, type: INT}", "{name: B, type: INT}"]);
    assert_eq!(
        err(&mut c, "C=1", OutputErrorKind::Undeclared),
        "Output variable 'C': the Step does not declare it (declared: 'A', 'B')"
    );
    // Names are matched case-sensitively.
    assert_eq!(
        err(&mut c, "a=1", OutputErrorKind::Undeclared),
        "Output variable 'a': the Step does not declare it (declared: 'A', 'B')"
    );
}

#[test]
fn accept_repeats_the_name_checks() {
    let mut c = chunked(&["{name: 'N', type: INT}"], &[1]);
    let e = c.accept("M", Some(1), b"1", false).unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Undeclared);
    assert_eq!(
        e.to_string(),
        "Output variable 'M[1]': the Step does not declare it (declared: 'N')"
    );
    let e = c.accept("N", None, b"1", false).unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Selector);
    assert_eq!(
        e.to_string(),
        "Output variable 'N': a task selector, as in 'N[<value>]', is required in a Step with \
         a CHUNK[INT] parameter"
    );
    c.accept("N", Some(1), b"1", false).unwrap();
    let e = c.accept("N", Some(1), b"2", false).unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Repeated);
    assert_eq!(
        e.to_string(),
        "Output variable 'N[1]': a second value was reported, but its collect mode is ONCE"
    );
}

#[test]
fn accept_trims_spaces_and_tabs_around_an_inline_value() {
    // Called directly, without parse_output_directive's trimming; the
    // trimmed text is what counts toward the size.
    let mut c = with_limit(&["{name: S, type: STRING}"], 5, None);
    assert_eq!(
        c.accept("S", None, b" \t\"abc\"\t ", false),
        Ok(ExprValue::String("abc".into()))
    );
    assert_eq!(c.remaining_bytes_for("S", None), 0);
}

#[test]
fn inline_value_not_utf8() {
    let mut c = chunked(&["{name: S, type: STRING}"], &[2]);
    let e = c.accept("S", Some(2), b"\"a\xFF\"", false).unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Grammar);
    assert_eq!(
        e.to_string(),
        "Invalid openjd_output_var directive: the value of 'S[2]' is not valid UTF-8"
    );
}

#[test]
fn a_repeated_declaration_keeps_the_first() {
    let mut c = collector(&["{name: 'N', type: INT}", "{name: 'N', type: STRING}"]);
    assert_eq!(c.accept("N", None, b"\"7\"", false), Ok(ExprValue::Int(7)));
    assert_eq!(only_task(c).len(), 1);
}

#[test]
fn append_on_a_scalar_type_is_refused() {
    // Validation rejects this; a hand-built declaration reaches `new`.
    let mut v = var("{name: E, type: 'LIST[INT]', collect: APPEND}");
    v.param_type = openjd_model::JobParameterType::Int;
    let e = OutputCollector::new(&[v], MIN_OUTPUT_BYTES_LIMIT, &[], PathFormat::Posix, None)
        .unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Declaration);
    assert_eq!(
        e.to_string(),
        "Output variable 'E': collect mode APPEND requires a LIST type, but it is declared INT"
    );
}

#[test]
fn once_repeated_is_invalid() {
    let mut c = collector(&["{name: 'N', type: INT}"]);
    ok(&mut c, "N=1");
    let e = c.check_name("N", None).unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Repeated);
    assert_eq!(
        e.to_string(),
        "Output variable 'N': a second value was reported, but its collect mode is ONCE"
    );
    assert_eq!(only_task(c)["N"], ExprValue::Int(1));
}

#[test]
fn once_missing_fails_finish() {
    let mut c = collector(&["{name: A, type: INT}", "{name: B, type: INT}"]);
    ok(&mut c, "A=1");
    let e = c.finish().unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Missing);
    assert_eq!(
        e.to_string(),
        "Output variable 'B': no value was reported (collect mode ONCE)"
    );
}

#[test]
fn last_missing_fails_finish() {
    let c = collector(&["{name: F, type: INT, collect: LAST}"]);
    assert_eq!(
        c.finish().unwrap_err().to_string(),
        "Output variable 'F': no value was reported (collect mode LAST)"
    );
}

#[test]
fn last_replaces_the_value() {
    let mut c = collector(&["{name: F, type: INT, collect: LAST}"]);
    ok(&mut c, "F=1");
    ok(&mut c, "F=2");
    ok(&mut c, "F=3");
    assert_eq!(only_task(c)["F"], ExprValue::Int(3));
}

#[test]
fn last_checks_every_value_not_only_the_last() {
    let mut c = collector(&["{name: F, type: INT, collect: LAST, maxValue: 10}"]);
    ok(&mut c, "F=1");
    assert_eq!(
        err(&mut c, "F=11", OutputErrorKind::Constraint),
        "Output variable 'F': value 11 exceeds maximum 10"
    );
    // The failed directive left the earlier value in place.
    assert_eq!(only_task(c)["F"], ExprValue::Int(1));
}

#[test]
fn append_with_no_directive_is_empty() {
    let c = collector(&["{name: E, type: 'LIST[STRING]', collect: APPEND}"]);
    let v = only_task(c).swap_remove("E").unwrap();
    assert_eq!(v, strings(&[]));
    assert_eq!(v.expr_type(), ExprType::list(ExprType::STRING));
}

#[test]
fn append_several() {
    let mut c = collector(&["{name: E, type: 'LIST[STRING]', collect: APPEND}"]);
    assert_eq!(ok(&mut c, "E=\"a\""), ExprValue::String("a".into()));
    ok(&mut c, "E=2");
    ok(&mut c, "E=\"c\"");
    assert_eq!(only_task(c)["E"], strings(&["a", "2", "c"]));
}

#[test]
fn append_checks_item_per_element() {
    let mut c = collector(&["{name: E, type: 'LIST[INT]', collect: APPEND, item: {maxValue: 5}}"]);
    ok(&mut c, "E=5");
    assert_eq!(
        err(&mut c, "E=6", OutputErrorKind::Constraint),
        "Output variable 'E': element 6 exceeds maximum 5"
    );
    assert_eq!(only_task(c)["E"], ExprValue::ListInt(vec![5]));
}

#[test]
fn append_checks_max_length_as_the_list_grows() {
    let mut c = collector(&["{name: E, type: 'LIST[INT]', collect: APPEND, maxLength: 2}"]);
    ok(&mut c, "E=1");
    ok(&mut c, "E=2");
    assert_eq!(
        err(&mut c, "E=3", OutputErrorKind::Constraint),
        "Output variable 'E': list length 3 exceeds maximum 2"
    );
}

#[test]
fn append_checks_min_length_only_at_exit() {
    let mut c = collector(&["{name: E, type: 'LIST[INT]', collect: APPEND, minLength: 2}"]);
    ok(&mut c, "E=1");
    let e = c.clone().finish().unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Missing);
    assert_eq!(
        e.to_string(),
        "Output variable 'E': list length 1 is less than minimum 2"
    );
    ok(&mut c, "E=2");
    assert_eq!(only_task(c)["E"], ExprValue::ListInt(vec![1, 2]));
}

#[test]
fn append_list_list_checks_element_lengths() {
    let mut c = collector(&[
        "{name: G, type: 'LIST[LIST[INT]]', collect: APPEND, item: {minLength: 1, item: {maxValue: 3}}}",
    ]);
    ok(&mut c, "G=[1, 3]");
    assert_eq!(
        err(&mut c, "G=[]", OutputErrorKind::Constraint),
        "Output variable 'G': element length 0 is less than minimum 1"
    );
    assert_eq!(
        err(&mut c, "G=[4]", OutputErrorKind::Constraint),
        "Output variable 'G': element item[0] 4 exceeds maximum 3"
    );
}

#[test]
fn once_list_checks_min_length_when_reported() {
    let mut c = collector(&["{name: L, type: 'LIST[INT]', minLength: 1, item: {minValue: 0}}"]);
    assert_eq!(
        err(&mut c, "L=[]", OutputErrorKind::Constraint),
        "Output variable 'L': list length 0 is less than minimum 1"
    );
    assert_eq!(
        err(&mut c, "L=[1, -1]", OutputErrorKind::Constraint),
        "Output variable 'L': item[1] -1 is less than minimum 0"
    );
}

#[test]
fn finish_keeps_declaration_order() {
    let mut c = collector(&[
        "{name: Z, type: INT}",
        "{name: A, type: 'LIST[INT]', collect: APPEND}",
        "{name: M, type: STRING, collect: LAST}",
    ]);
    ok(&mut c, "M=\"m\"");
    ok(&mut c, "Z=1");
    let names: Vec<String> = only_task(c).keys().cloned().collect();
    assert_eq!(names, ["Z", "A", "M"]);
}

// ══════════════════════════════════════════════════════════════
// Constraints
// ══════════════════════════════════════════════════════════════

#[test]
fn string_allowed_values() {
    let mut c = collector(&["{name: S, type: STRING, allowedValues: [a, b]}"]);
    assert_eq!(
        err(&mut c, "S=\"c\"", OutputErrorKind::Constraint),
        "Output variable 'S': value 'c' is not in allowed values"
    );
    ok(&mut c, "S=\"b\"");
}

#[test]
fn float_bounds() {
    let mut c = collector(&["{name: X, type: FLOAT, minValue: 0.5}"]);
    assert_eq!(
        err(&mut c, "X=0", OutputErrorKind::Constraint),
        "Output variable 'X': value 0.0 is less than minimum 0.5"
    );
}

#[test]
fn range_expr_is_measured_in_its_written_spelling() {
    // `1-10:2` is 6 characters; its canonical form `1-9:2` is 5.
    let mut c = collector(&["{name: R, type: RANGE_EXPR, maxLength: 5}"]);
    assert_eq!(
        err(&mut c, "R=\"1-10:2\"", OutputErrorKind::Constraint),
        "Output variable 'R': value length 6 exceeds maximum 5"
    );
    ok(&mut c, "R=\"1-9:2\"");
}

#[test]
fn range_expr_longer_than_1024_characters_without_max_length() {
    let spelled: Vec<String> = (1..=400).map(|i| (i * 2).to_string()).collect();
    let text = spelled.join(",");
    assert!(text.len() > 1024);
    let v = convert("{name: R, type: RANGE_EXPR}", &format!("R=\"{text}\"")).unwrap();
    assert_eq!(v, ExprValue::RangeExpr(text.parse::<RangeExpr>().unwrap()));
}

#[test]
fn invalid_range_expr_string() {
    let mut c = collector(&["{name: R, type: RANGE_EXPR}"]);
    assert_eq!(
        err(&mut c, "R=\"\"", OutputErrorKind::Conversion),
        "Output variable 'R': value (string) '' does not convert to range_expr: Cannot \
         convert '' to range_expr: Empty expression"
    );
}

// ══════════════════════════════════════════════════════════════
// @ file values
// ══════════════════════════════════════════════════════════════

#[test]
fn file_with_bom_and_line_breaks() {
    let mut c = collector(&["{name: K, type: 'LIST[STRING]'}"]);
    let v = file(
        &mut c,
        "K",
        None,
        b"\xEF\xBB\xBF\r\n[\n  \"a\",\n  \"b\"\n]\n",
    )
    .unwrap();
    assert_eq!(v, strings(&["a", "b"]));
}

#[test]
fn file_not_utf8() {
    let mut c = collector(&["{name: S, type: STRING}"]);
    let e = file(&mut c, "S", None, b"\xEF\xBB\xBF\"a\xFFb\"").unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::FileContent);
    assert_eq!(
        e.to_string(),
        "Output variable 'S': the @ file's content is not valid UTF-8 (at byte 5)"
    );
}

#[test]
fn file_with_nul() {
    let mut c = collector(&["{name: S, type: STRING}"]);
    let e = file(&mut c, "S", None, b"\"a\0b\"").unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::FileContent);
    assert_eq!(
        e.to_string(),
        "Output variable 'S': the @ file's content contains a NUL character"
    );
}

#[test]
fn file_not_one_json_value_does_not_quote_the_content() {
    let mut c = collector(&["{name: S, type: STRING}"]);
    for (content, detail) in [
        (
            &b"\"secret\" 2"[..],
            "trailing characters at line 1 column 10",
        ),
        (b"", "EOF while parsing a value at line 1 column 0"),
        (b"  \n ", "EOF while parsing a value at line 2 column 1"),
    ] {
        let e = file(&mut c, "S", None, content).unwrap_err();
        assert_eq!(e.kind(), OutputErrorKind::Json);
        assert_eq!(
            e.to_string(),
            format!(
                "Output variable 'S': the @ file's content is not one valid JSON value: {detail}"
            )
        );
    }
}

#[test]
fn file_conversion_failure_does_not_quote_the_value() {
    let mut c = collector(&["{name: L, type: 'LIST[INT]'}"]);
    let e = file(&mut c, "L", None, b"[1, \"secret\"]").unwrap_err();
    assert_eq!(
        e.to_string(),
        "Output variable 'L': item[1] (string) does not convert to int"
    );
}

#[test]
fn file_value_path_and_constraint_errors_name_the_converted_value() {
    // The file's text is never quoted, but the invalid value after conversion
    // is, as for an inline value (the RFC asks for the offending text).
    let rules = [rule(PathFormat::Posix, "/proj", "/mnt/proj")];
    let mut c = with_rules(
        &["{name: P, type: 'LIST[PATH]'}"],
        &rules,
        PathFormat::Posix,
    );
    let e = file(&mut c, "P", None, b"[\"/mnt/proj/a\",\n \"/tmp/b\"]").unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Path);
    assert_eq!(
        e.to_string(),
        "Output variable 'P': item[1] '/tmp/b' is not a valid PATH output: no path mapping rule \
         maps the path back to the submission path context, so it names a location only this \
         worker can see"
    );
    let mut c = collector(&["{name: S, type: STRING, allowedValues: [a]}"]);
    let e = file(&mut c, "S", None, b"\"b\"").unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Constraint);
    assert_eq!(
        e.to_string(),
        "Output variable 'S': value 'b' is not in allowed values"
    );
}

#[test]
fn coercion_detail_is_cut_and_escaped() {
    let long = format!("a\\n{}", "y".repeat(300));
    let mut c = collector(&["{name: I, type: INT}"]);
    let msg = err(
        &mut c,
        &format!("I=\"{long}\""),
        OutputErrorKind::Conversion,
    );
    assert!(!msg.contains('\n'), "{msg}");
    assert_eq!(
        msg,
        format!(
            "Output variable 'I': value (string) 'a\\n{y198}...' does not convert to int: \
             Cannot convert 'a\\n{y182}...",
            y198 = "y".repeat(198),
            y182 = "y".repeat(182)
        )
    );
}

#[test]
fn coercion_detail_is_cut() {
    let long = "y".repeat(300);
    let mut c = collector(&["{name: I, type: INT}"]);
    assert_eq!(
        err(
            &mut c,
            &format!("I=\"{long}\""),
            OutputErrorKind::Conversion
        ),
        format!(
            "Output variable 'I': value (string) '{y200}...' does not convert to int: Cannot \
             convert '{y184}...",
            y200 = "y".repeat(200),
            y184 = "y".repeat(184)
        )
    );
}

#[test]
fn hand_built_names_are_escaped_in_the_selector_hint_and_cut() {
    let mut v = var("{name: X, type: INT}");
    v.name = "a\u{1b}b".into();
    let c = OutputCollector::new(
        std::slice::from_ref(&v),
        MIN_OUTPUT_BYTES_LIMIT,
        &[],
        PathFormat::Posix,
        Some(vec![1]),
    )
    .unwrap();
    assert_eq!(
        c.check_name("a\u{1b}b", None).unwrap_err().to_string(),
        "Output variable 'a\\u{1b}b': a task selector, as in 'a\\u{1b}b[<value>]', is \
         required in a Step with a CHUNK[INT] parameter"
    );
    v.name = "N".repeat(300);
    let c = OutputCollector::new(&[v], 10, &[], PathFormat::Posix, None).unwrap();
    assert_eq!(
        c.finish().unwrap_err().to_string(),
        format!(
            "Output variable '{}...': no value was reported (collect mode ONCE)",
            "N".repeat(200)
        )
    );
}

#[test]
fn hand_built_names_with_control_characters_are_escaped() {
    let mut v = var("{name: X, type: INT, maxValue: 1}");
    v.name = "a\u{1b}b".into();
    let mut c = OutputCollector::new(
        std::slice::from_ref(&v),
        MIN_OUTPUT_BYTES_LIMIT,
        &[],
        PathFormat::Posix,
        None,
    )
    .unwrap();
    assert_eq!(
        c.check_name("z", None).unwrap_err().to_string(),
        "Output variable 'z': the Step does not declare it (declared: 'a\\u{1b}b')"
    );
    assert_eq!(
        c.accept("a\u{1b}b", None, b"2", false)
            .unwrap_err()
            .to_string(),
        "Output variable 'a\\u{1b}b': value 2 exceeds maximum 1"
    );
    let mut e = var("{name: E, type: 'LIST[INT]', collect: APPEND, minLength: 1}");
    e.name = "e\n".into();
    assert_eq!(
        OutputCollector::new(&[e.clone()], 10, &[], PathFormat::Posix, None)
            .unwrap()
            .finish()
            .unwrap_err()
            .to_string(),
        "Output variable 'e\\n': list length 0 is less than minimum 1"
    );
    e.param_type = openjd_model::JobParameterType::Int;
    assert_eq!(
        OutputCollector::new(&[e], 10, &[], PathFormat::Posix, None)
            .unwrap_err()
            .to_string(),
        "Output variable 'e\\n': collect mode APPEND requires a LIST type, but it is declared \
         INT"
    );
}

#[test]
fn quoted_values_are_cut_and_escaped() {
    let mut c = collector(&["{name: S, type: STRING, allowedValues: [a]}"]);
    let long = "x".repeat(300);
    let e = file(&mut c, "S", None, format!("\"{long}\"").as_bytes()).unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "Output variable 'S': value '{}...' is not in allowed values",
            "x".repeat(200)
        )
    );
    assert_eq!(
        err(&mut c, "S=\"a\\nb\\u001b[0m\"", OutputErrorKind::Constraint),
        "Output variable 'S': value 'a\\nb\\u{1b}[0m' is not in allowed values"
    );
    let mut c = collector(&["{name: 'N', type: INT}"]);
    assert_eq!(
        err(&mut c, "N=\"a\\tb\"", OutputErrorKind::Conversion),
        "Output variable 'N': value (string) 'a\\tb' does not convert to int: Cannot convert \
         'a\\tb' to int: invalid digit found in string"
    );
}

#[test]
fn file_under_append_supplies_one_element() {
    let mut c = collector(&["{name: E, type: 'LIST[LIST[INT]]', collect: APPEND}"]);
    file(&mut c, "E", None, b"[1, 2]").unwrap();
    ok(&mut c, "E=[3]");
    assert_eq!(only_task(c)["E"], int_lists(&[&[1, 2], &[3]]));
}

// ══════════════════════════════════════════════════════════════
// Size limit (§Limits item 2)
// ══════════════════════════════════════════════════════════════

#[test]
fn inline_size_is_the_text_without_surrounding_whitespace() {
    // `"abc"` is 5 bytes; the spaces around it don't count.
    let mut c = with_limit(&["{name: S, type: STRING}"], 5, None);
    ok(&mut c, "S= \t\"abc\" \t");
    let mut c = with_limit(&["{name: S, type: STRING}"], 4, None);
    assert_eq!(
        err(&mut c, "S=\"abc\"", OutputErrorKind::Size),
        "Output variable 'S': the value's 5 bytes take the Task's output values to 5 bytes, \
         over the limit of 4 bytes"
    );
}

#[test]
fn size_is_the_total_of_held_values() {
    let mut c = with_limit(
        &[
            "{name: A, type: INT}",
            "{name: E, type: 'LIST[INT]', collect: APPEND}",
        ],
        6,
        None,
    );
    ok(&mut c, "A=100"); // 3
    ok(&mut c, "E=10"); // 2
    ok(&mut c, "E=1"); // 1
    assert_eq!(
        err(&mut c, "E=2", OutputErrorKind::Size),
        "Output variable 'E': the value's 1 bytes take the Task's output values to 7 bytes, \
         over the limit of 6 bytes"
    );
}

#[test]
fn file_size_is_the_whole_content() {
    // The content, BOM and whitespace included, counts: 3 + 1 + 3 + 1 + 1 = 9.
    let mut c = with_limit(&["{name: S, type: STRING}"], 8, None);
    let e = file(&mut c, "S", None, b"\xEF\xBB\xBF \"a\" \n").unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Size);
    assert_eq!(
        e.to_string(),
        "Output variable 'S': the value's 9 bytes take the Task's output values to 9 bytes, \
         over the limit of 8 bytes"
    );
    let mut c = with_limit(&["{name: S, type: STRING}"], 9, None);
    file(&mut c, "S", None, b"\xEF\xBB\xBF \"a\" \n").unwrap();
}

#[test]
fn a_failed_size_check_leaves_the_size_unchanged() {
    let mut c = with_limit(&["{name: E, type: 'LIST[INT]', collect: APPEND}"], 3, None);
    ok(&mut c, "E=1");
    err(&mut c, "E=1234", OutputErrorKind::Size);
    assert_eq!(c.remaining_bytes_for("E", None), 2);
    ok(&mut c, "E=23");
    assert_eq!(only_task(c)["E"], ExprValue::ListInt(vec![1, 23]));
}

#[test]
fn last_replaced_value_stops_counting() {
    let mut c = with_limit(
        &[
            "{name: F, type: STRING, collect: LAST}",
            "{name: A, type: INT}",
        ],
        10,
        None,
    );
    ok(&mut c, "A=1"); // 1
    ok(&mut c, "F=\"aaaaaaa\""); // 9; total 10
    assert_eq!(c.remaining_bytes_for("F", None), 9);
    assert_eq!(c.remaining_bytes_for("A", None), 0);
    ok(&mut c, "F=\"bbbbbbb\""); // replaces 9 with 9
    ok(&mut c, "F=\"c\""); // 3; total 4
    assert_eq!(c.remaining_bytes_for("F", None), 9);
    assert_eq!(c.remaining_bytes_for("A", None), 6);
}

#[test]
fn remaining_bytes_for_append_counts_every_element() {
    let mut c = with_limit(&["{name: E, type: 'LIST[INT]', collect: APPEND}"], 10, None);
    assert_eq!(c.remaining_bytes_for("E", None), 10);
    ok(&mut c, "E=123");
    ok(&mut c, "E=45");
    assert_eq!(c.remaining_bytes_for("E", None), 5);
    // An unknown Task has nothing held.
    assert_eq!(c.remaining_bytes_for("E", Some(1)), 10);
}

#[test]
fn path_size_is_the_larger_of_reported_and_stored() {
    // Reported `"/mnt/p/a"` is 10 bytes; stored `"P:\\a"` serializes to 7.
    let rules = [rule(PathFormat::Windows, "P:\\", "/mnt/p")];
    let declared = [var("{name: P, type: PATH}")];
    let mut c = OutputCollector::new(&declared, 9, &rules, PathFormat::Posix, None).unwrap();
    assert_eq!(
        err(&mut c, "P=\"/mnt/p/a\"", OutputErrorKind::Size),
        "Output variable 'P': the value's 10 bytes take the Task's output values to 10 bytes, \
         over the limit of 9 bytes"
    );
    // Reported `"/m/a"` is 6 bytes; stored `"P:\\longer\\a"` serializes to 15.
    let rules = [rule(PathFormat::Windows, "P:\\longer", "/m")];
    let mut c = OutputCollector::new(&declared, 14, &rules, PathFormat::Posix, None).unwrap();
    assert_eq!(
        err(&mut c, "P=\"/m/a\"", OutputErrorKind::Size),
        "Output variable 'P': the value's 15 bytes take the Task's output values to 15 bytes, \
         over the limit of 14 bytes"
    );
    let mut c = OutputCollector::new(&declared, 15, &rules, PathFormat::Posix, None).unwrap();
    ok(&mut c, "P=\"/m/a\"");
}

#[test]
fn list_path_size_uses_the_stored_list() {
    // Reported `["/m/a"]` is 8 bytes; stored `["P:\\longer\\a"]` is 17.
    let rules = [rule(PathFormat::Windows, "P:\\longer", "/m")];
    let declared = [var("{name: L, type: 'LIST[PATH]'}")];
    let mut c = OutputCollector::new(&declared, 16, &rules, PathFormat::Posix, None).unwrap();
    assert_eq!(
        err(&mut c, "L=[\"/m/a\"]", OutputErrorKind::Size),
        "Output variable 'L': the value's 17 bytes take the Task's output values to 17 bytes, \
         over the limit of 16 bytes"
    );
}

// ══════════════════════════════════════════════════════════════
// PATH values (§Path values)
// ══════════════════════════════════════════════════════════════

#[test]
fn path_relative_is_invalid() {
    let rules = [rule(PathFormat::Posix, "/proj", "/mnt/proj")];
    let mut c = with_rules(&["{name: P, type: PATH}"], &rules, PathFormat::Posix);
    assert_eq!(
        err(&mut c, "P=\"out/a.exr\"", OutputErrorKind::Path),
        "Output variable 'P': value 'out/a.exr' is not a valid PATH output: the path is relative; a PATH value must be \
         an absolute path or a URI"
    );
}

#[test]
fn path_relative_is_invalid_without_rules() {
    let mut c = collector(&["{name: P, type: PATH}"]);
    assert_eq!(
        err(&mut c, "P=\"a\"", OutputErrorKind::Path),
        "Output variable 'P': value 'a' is not a valid PATH output: the path is relative; a PATH value must be an \
         absolute path or a URI"
    );
}

#[test]
fn path_with_no_round_trip_is_invalid() {
    let rules = [rule(PathFormat::Posix, "/proj", "/mnt/proj")];
    let mut c = with_rules(&["{name: P, type: PATH}"], &rules, PathFormat::Posix);
    assert_eq!(
        err(&mut c, "P=\"/tmp/session/out\"", OutputErrorKind::Path),
        "Output variable 'P': value '/tmp/session/out' is not a valid PATH output: no path mapping rule maps the path \
         back to the submission path context, so it names a location only this worker can see"
    );
}

#[test]
fn path_without_rules_is_stored_unchanged() {
    let mut c = collector(&["{name: P, type: PATH}"]);
    let v = ok(&mut c, "P=\"/a/./b\"");
    assert_eq!(path_parts(&v), ("/a/./b".to_string(), PathFormat::Posix));
}

#[test]
fn path_uri_is_stored_unchanged() {
    let rules = [rule(PathFormat::Posix, "/proj", "/mnt/proj")];
    let mut c = with_rules(&["{name: P, type: PATH}"], &rules, PathFormat::Windows);
    let v = ok(&mut c, "P=\"s3://bucket/key\"");
    assert_eq!(
        path_parts(&v),
        ("s3://bucket/key".to_string(), PathFormat::Uri)
    );
}

#[test]
fn path_on_a_windows_worker() {
    let rules = [rule(PathFormat::Posix, "/proj", "Z:\\proj")];
    let mut c = with_rules(&["{name: P, type: PATH}"], &rules, PathFormat::Windows);
    let v = ok(&mut c, "P=\"z:/proj/a/b.exr\"");
    assert_eq!(
        path_parts(&v),
        ("/proj/a/b.exr".to_string(), PathFormat::Posix)
    );
}

#[test]
fn an_invalid_path_is_quoted_cut_and_escaped() {
    let mut c = collector(&["{name: P, type: PATH}"]);
    assert_eq!(
        err(&mut c, "P=\"a\\nb\"", OutputErrorKind::Path),
        "Output variable 'P': value 'a\\nb' is not a valid PATH output: the path is relative; \
         a PATH value must be an absolute path or a URI"
    );
    let long = "d/".repeat(150);
    assert_eq!(
        err(&mut c, &format!("P=\"{long}\""), OutputErrorKind::Path),
        format!(
            "Output variable 'P': value '{}...' is not a valid PATH output: the path is \
             relative; a PATH value must be an absolute path or a URI",
            "d/".repeat(100)
        )
    );
}

#[test]
fn path_is_parsed_in_the_worker_format() {
    // On a Windows worker the reported path takes Windows separators before
    // it is mapped back, which shows in the message when it can't be.
    let rules = [rule(PathFormat::Posix, "/proj", "Z:\\proj")];
    let mut c = with_rules(&["{name: P, type: PATH}"], &rules, PathFormat::Windows);
    assert_eq!(
        err(&mut c, "P=\"C:/x/y\"", OutputErrorKind::Path),
        "Output variable 'P': value 'C:\\x\\y' is not a valid PATH output: no path mapping rule \
         maps the path back to the submission path context, so it names a location only this \
         worker can see"
    );
}

#[test]
fn path_constraints_apply_to_the_stored_form() {
    let rules = [rule(PathFormat::Windows, "P:\\", "/mnt/proj")];
    // The stored `P:\a` is 4 characters; the reported path is 11.
    let mut c = with_rules(
        &["{name: P, type: PATH, maxLength: 4, allowedValues: ['P:\\a']}"],
        &rules,
        PathFormat::Posix,
    );
    let v = ok(&mut c, "P=\"/mnt/proj/a\"");
    assert_eq!(path_parts(&v).0, "P:\\a");
    let mut c = with_rules(
        &["{name: P, type: PATH, maxLength: 4}"],
        &rules,
        PathFormat::Posix,
    );
    assert_eq!(
        err(&mut c, "P=\"/mnt/proj/ab\"", OutputErrorKind::Constraint),
        "Output variable 'P': value length 5 exceeds maximum 4"
    );
    let mut c = with_rules(
        &["{name: P, type: PATH, allowedValues: ['/mnt/proj/a']}"],
        &rules,
        PathFormat::Posix,
    );
    assert_eq!(
        err(&mut c, "P=\"/mnt/proj/a\"", OutputErrorKind::Constraint),
        "Output variable 'P': value 'P:\\a' is not in allowed values"
    );
}

#[test]
fn list_path_elements_are_mapped_back() {
    let rules = [
        rule(PathFormat::Windows, "P:\\", "/mnt/p"),
        rule(PathFormat::Uri, "s3://b", "/mnt/s3"),
    ];
    let mut c = with_rules(
        &["{name: L, type: 'LIST[PATH]', item: {maxLength: 8}}"],
        &rules,
        PathFormat::Posix,
    );
    let v = ok(&mut c, "L=[\"/mnt/p/x\", \"/mnt/s3/k\"]");
    match &v {
        ExprValue::ListPath(values, format, _) => {
            assert_eq!(values, &["P:\\x".to_string(), "s3://b/k".to_string()]);
            // The list takes its first element's format.
            assert_eq!(*format, PathFormat::Windows);
        }
        other => panic!("not a list[path]: {other:?}"),
    }
    let mut c = with_rules(
        &["{name: L, type: 'LIST[PATH]', item: {maxLength: 8}}"],
        &rules,
        PathFormat::Posix,
    );
    assert_eq!(
        err(
            &mut c,
            "L=[\"/mnt/p/x\", \"/mnt/p/abcdefgh\"]",
            OutputErrorKind::Constraint
        ),
        "Output variable 'L': item[1] length 11 exceeds maximum 8"
    );
    let mut c = with_rules(
        &["{name: L, type: 'LIST[PATH]'}"],
        &rules,
        PathFormat::Posix,
    );
    assert_eq!(
        err(&mut c, "L=[\"/mnt/p/x\", \"/tmp/y\"]", OutputErrorKind::Path),
        "Output variable 'L': item[1] '/tmp/y' is not a valid PATH output: no path mapping rule maps the path back to the \
         submission path context, so it names a location only this worker can see"
    );
}

#[test]
fn append_path_elements_are_mapped_back() {
    let rules = [rule(PathFormat::Windows, "P:\\", "/mnt/p")];
    let mut c = with_rules(
        &["{name: E, type: 'LIST[PATH]', collect: APPEND}"],
        &rules,
        PathFormat::Posix,
    );
    let v = ok(&mut c, "E=\"/mnt/p/a\"");
    assert_eq!(path_parts(&v), ("P:\\a".to_string(), PathFormat::Windows));
    ok(&mut c, "E=\"/mnt/p/b\"");
    match &only_task(c)["E"] {
        ExprValue::ListPath(values, format, _) => {
            assert_eq!(values, &["P:\\a".to_string(), "P:\\b".to_string()]);
            assert_eq!(*format, PathFormat::Windows);
        }
        other => panic!("not a list[path]: {other:?}"),
    }
}

#[test]
fn empty_list_path_takes_the_worker_format() {
    let c = with_rules(
        &["{name: E, type: 'LIST[PATH]', collect: APPEND}"],
        &[],
        PathFormat::Windows,
    );
    match &only_task(c)["E"] {
        ExprValue::ListPath(values, format, _) => {
            assert!(values.is_empty());
            assert_eq!(*format, PathFormat::Windows);
        }
        other => panic!("not a list[path]: {other:?}"),
    }
}

// ══════════════════════════════════════════════════════════════
// Chunked Steps (§Chunked Steps)
// ══════════════════════════════════════════════════════════════

const RENDER_TIME: &str = "{name: RenderTimeS, type: FLOAT, unit: s}";

#[test]
fn chunk_selector_required() {
    let mut c = chunked(&[RENDER_TIME], &[1, 2, 3]);
    assert_eq!(
        err(&mut c, "RenderTimeS=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS': a task selector, as in 'RenderTimeS[<value>]', is \
         required in a Step with a CHUNK[INT] parameter"
    );
}

#[test]
fn chunk_of_one_task_still_requires_the_selector() {
    let mut c = chunked(&[RENDER_TIME], &[7]);
    assert_eq!(
        err(&mut c, "RenderTimeS=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS': a task selector, as in 'RenderTimeS[<value>]', is \
         required in a Step with a CHUNK[INT] parameter"
    );
    ok(&mut c, "RenderTimeS[7]=1.5");
}

#[test]
fn selector_forbidden_without_a_chunk() {
    let mut c = collector(&[RENDER_TIME]);
    assert_eq!(
        err(&mut c, "RenderTimeS[1]=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS[1]': a task selector is only allowed in a Step with a \
         CHUNK[INT] parameter"
    );
}

#[test]
fn selector_outside_the_chunk() {
    let mut c = chunked(&[RENDER_TIME], &[10, 11, 12]);
    assert_eq!(
        err(&mut c, "RenderTimeS[13]=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS[13]': 13 is not a value of the chunk being run (chunk \
         values 10, 11, 12)"
    );
    let values: Vec<i64> = (1..=12).collect();
    let mut c = chunked(&[RENDER_TIME], &values);
    assert_eq!(
        err(&mut c, "RenderTimeS[0]=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS[0]': 0 is not a value of the chunk being run (chunk \
         values 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, ... (12 values))"
    );
    let values: Vec<i64> = (1..=11).collect();
    let mut c = chunked(&[RENDER_TIME], &values);
    assert_eq!(
        err(&mut c, "RenderTimeS[0]=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS[0]': 0 is not a value of the chunk being run (chunk \
         values 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, ... (11 values))"
    );
    let values: Vec<i64> = (1..=10).collect();
    let mut c = chunked(&[RENDER_TIME], &values);
    assert_eq!(
        err(&mut c, "RenderTimeS[0]=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS[0]': 0 is not a value of the chunk being run (chunk \
         values 1, 2, 3, 4, 5, 6, 7, 8, 9, 10)"
    );
    let mut c = chunked(&[RENDER_TIME], &[]);
    assert_eq!(
        err(&mut c, "RenderTimeS[0]=1.5", OutputErrorKind::Selector),
        "Output variable 'RenderTimeS[0]': 0 is not a value of the chunk being run (the chunk \
         is empty)"
    );
    assert!(finished(c).is_empty());
}

#[test]
fn selector_with_leading_zeros_matches_by_value() {
    let mut c = chunked(&[RENDER_TIME], &[12]);
    ok(&mut c, "RenderTimeS[012]=1.5");
    assert_eq!(finished(c)[&Some(12)]["RenderTimeS"], float(1.5));
}

#[test]
fn duplicate_chunk_values_are_rejected() {
    let declared = [var(RENDER_TIME)];
    let e = OutputCollector::new(
        &declared,
        MIN_OUTPUT_BYTES_LIMIT,
        &[],
        PathFormat::Posix,
        Some(vec![1, 2, 1]),
    )
    .unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Selector);
    assert_eq!(
        e.to_string(),
        "The chunk repeats the CHUNK[INT] value 1; in a Step with output variables each value \
         must name exactly one Task"
    );
}

#[test]
fn once_is_per_task() {
    let mut c = chunked(&[RENDER_TIME], &[12, 13]);
    ok(&mut c, "RenderTimeS[12]=1.5");
    ok(&mut c, "RenderTimeS[13]=2.5");
    assert_eq!(
        err(&mut c, "RenderTimeS[12]=3", OutputErrorKind::Repeated),
        "Output variable 'RenderTimeS[12]': a second value was reported, but its collect mode \
         is ONCE"
    );
}

#[test]
fn collect_modes_are_per_task() {
    let mut c = chunked(
        &[
            "{name: F, type: INT, collect: LAST}",
            "{name: E, type: 'LIST[INT]', collect: APPEND}",
        ],
        &[1, 2],
    );
    ok(&mut c, "F[1]=1");
    ok(&mut c, "F[2]=20");
    ok(&mut c, "F[1]=10");
    ok(&mut c, "E[2]=5");
    ok(&mut c, "E[2]=6");
    let all = finished(c);
    assert_eq!(all[&Some(1)]["F"], ExprValue::Int(10));
    assert_eq!(all[&Some(1)]["E"], ExprValue::ListInt(vec![]));
    assert_eq!(all[&Some(2)]["F"], ExprValue::Int(20));
    assert_eq!(all[&Some(2)]["E"], ExprValue::ListInt(vec![5, 6]));
}

#[test]
fn size_limit_is_per_task() {
    let mut c = with_limit(&["{name: S, type: STRING}"], 5, Some(vec![1, 2]));
    ok(&mut c, "S[1]=\"abc\"");
    ok(&mut c, "S[2]=\"abc\"");
    assert_eq!(c.remaining_bytes_for("S", Some(1)), 0);
    let mut c = with_limit(
        &["{name: S, type: STRING}", "{name: T, type: STRING}"],
        5,
        Some(vec![1, 2]),
    );
    ok(&mut c, "S[1]=\"abc\"");
    assert_eq!(
        err(&mut c, "T[1]=\"a\"", OutputErrorKind::Size),
        "Output variable 'T[1]': the value's 3 bytes take the Task's output values to 8 bytes, \
         over the limit of 5 bytes"
    );
    ok(&mut c, "T[2]=\"a\"");
}

#[test]
fn constraint_messages_name_the_task() {
    let mut c = chunked(&["{name: F, type: INT, maxValue: 3}"], &[4]);
    assert_eq!(
        err(&mut c, "F[4]=9", OutputErrorKind::Constraint),
        "Output variable 'F[4]': value 9 exceeds maximum 3"
    );
}

#[test]
fn conversion_messages_name_the_task() {
    let mut c = chunked(&["{name: F, type: INT}"], &[4]);
    assert_eq!(
        err(&mut c, "F[4]=null", OutputErrorKind::Conversion),
        "Output variable 'F[4]': value is null; output values cannot be or contain null"
    );
}

#[test]
fn a_missing_value_for_one_task_fails_finish() {
    let mut c = chunked(&[RENDER_TIME], &[1, 2, 3]);
    ok(&mut c, "RenderTimeS[1]=1.5");
    ok(&mut c, "RenderTimeS[3]=1.5");
    let e = c.finish().unwrap_err();
    assert_eq!(e.kind(), OutputErrorKind::Missing);
    assert_eq!(
        e.to_string(),
        "Output variable 'RenderTimeS[2]': no value was reported (collect mode ONCE)"
    );
}

#[test]
fn min_length_at_exit_is_per_task() {
    let mut c = chunked(
        &["{name: E, type: 'LIST[INT]', collect: APPEND, minLength: 1}"],
        &[1, 2],
    );
    ok(&mut c, "E[1]=1");
    assert_eq!(
        c.finish().unwrap_err().to_string(),
        "Output variable 'E[2]': list length 0 is less than minimum 1"
    );
}

#[test]
fn finish_is_in_chunk_order() {
    // The order the chunk was given (iteration order), not sorted, and not the
    // order the directives arrived in.
    let mut c = chunked(&[RENDER_TIME], &[5, 1, 3, 2]);
    for (frame, t) in [(2, "0.2"), (3, "0.3"), (1, "0.1"), (5, "0.5")] {
        ok(&mut c, &format!("RenderTimeS[{frame}]={t}"));
    }
    let all = finished(c);
    let keys: Vec<Option<i64>> = all.keys().copied().collect();
    assert_eq!(keys, [Some(5), Some(1), Some(3), Some(2)]);
    let values: Vec<String> = all
        .values()
        .map(|o| o["RenderTimeS"].to_display_string())
        .collect();
    assert_eq!(values, ["0.5", "0.1", "0.3", "0.2"]);
}

#[test]
fn negative_chunk_values() {
    let mut c = chunked(&[RENDER_TIME], &[-1, 0]);
    ok(&mut c, "RenderTimeS[-1]=1");
    ok(&mut c, "RenderTimeS[0]=2");
    assert_eq!(finished(c)[&Some(-1)]["RenderTimeS"], float(1.0));
}

#[test]
fn unchunked_finish_has_one_task_keyed_none() {
    let mut c = collector(&["{name: 'N', type: INT}"]);
    ok(&mut c, "N=1");
    let all = finished(c);
    assert_eq!(all.keys().copied().collect::<Vec<_>>(), [None]);
}

#[test]
fn no_declared_outputs() {
    let mut c = collector(&[]);
    assert_eq!(
        err(&mut c, "N=1", OutputErrorKind::Undeclared),
        "Output variable 'N': the Step does not declare it (the Step declares no output variables)"
    );
    assert!(only_task(c).is_empty());
}

#[test]
fn error_display_is_the_message() {
    let mut c = collector(&["{name: 'N', type: INT}"]);
    let e = directive(&mut c, "N=x").unwrap_err();
    assert_eq!(e.to_string(), e.message());
    let boxed: Box<dyn std::error::Error> = Box::new(e);
    assert!(boxed.to_string().starts_with("Output variable 'N'"));
}
