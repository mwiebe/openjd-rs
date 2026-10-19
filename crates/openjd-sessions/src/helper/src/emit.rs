// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Sending one framed line of child output to the parent: as a
//! `Response::Out` line, or, for an `@` directive of an Action that may report
//! output values, as `Response::OutputFile` frames that carry the file the
//! helper read as the job user (RFC 0010 §File values item 3).
//!
//! Shared by the Unix and Windows runners.

use crate::framer::{fit_out_payload, fit_payload, strip_cr, FramedLine};
use crate::output_file::{read_output_file, recognize_file_directive, FileDirective};
use crate::protocol::{OutputFileHeader, OutputFiles, Response};
use std::path::Path;

/// The envelope of an `OutputFileData` frame, plus `send`'s trailing `\n`.
const DATA_ENVELOPE: usize = r#"{"outputFileData":""}"#.len() + 1;

/// Send one line of child output through `send`.
///
/// Only the line terminator's `\r` is stripped, as on the Windows runner and
/// the parent's direct path, so the directive grammar sees the same text on
/// every path. With `output_files` set, a line the helper recognizes as an
/// `@` directive is resolved: the file is read beneath `cwd` and sent as frames
/// in place of the line. A line that was truncated or lossily decoded is never
/// resolved; it goes to the parent as an `Out` line with its flags, and the
/// parent rejects it.
pub fn emit_line(
    line: FramedLine,
    output_files: Option<&OutputFiles>,
    cwd: &Path,
    send: &mut impl FnMut(&Response),
) {
    let line = strip_cr(line);
    if let Some(output_files) = output_files {
        if !line.truncated && !line.lossy {
            if let Some(directive) = recognize_file_directive(&line.text) {
                emit_output_file(&line.text, directive, output_files.max_bytes, cwd, send);
                return;
            }
        }
    }
    let payload = fit_out_payload(&line.text);
    send(&Response::Out {
        truncated: line.truncated || payload.len() < line.text.len(),
        out: payload.to_string(),
        lossy: line.lossy,
    });
}

fn emit_output_file(
    line: &str,
    directive: FileDirective,
    max_bytes: usize,
    cwd: &Path,
    send: &mut impl FnMut(&Response),
) {
    let read = read_output_file(cwd, &line[directive.path.clone()], max_bytes);
    let (content, error) = match read {
        Ok(content) => (content, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let valid_utf8 = match std::str::from_utf8(&content) {
        Ok(_) => content.len(),
        Err(e) => e.valid_up_to(),
    };
    send(&Response::OutputFile {
        output_file: OutputFileHeader {
            line_len: line.len(),
            name: [directive.name.start, directive.name.end],
            task: directive.task,
            path: [directive.path.start, directive.path.end],
            error,
            size: content.len(),
            valid_utf8,
        },
    });
    let valid = std::str::from_utf8(&content[..valid_utf8]).expect("checked above");
    for part in [line, valid] {
        let mut rest = part;
        while !rest.is_empty() {
            let chunk = fit_payload(rest, DATA_ENVELOPE);
            send(&Response::OutputFileData {
                output_file_data: chunk.to_string(),
            });
            rest = &rest[chunk.len()..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framer::MAX_RESPONSE_LINE_LENGTH;

    fn line(text: &str) -> FramedLine {
        FramedLine {
            text: text.into(),
            ..Default::default()
        }
    }

    fn emitted(line: FramedLine, output_files: Option<&OutputFiles>, cwd: &Path) -> Vec<String> {
        let mut out = Vec::new();
        emit_line(line, output_files, cwd, &mut |r| {
            out.push(serde_json::to_string(r).unwrap())
        });
        out
    }

    const FILES: OutputFiles = OutputFiles { max_bytes: 10 };

    #[test]
    fn ordinary_line_is_out_with_only_cr_stripped() {
        let cwd = std::env::temp_dir();
        assert_eq!(
            emitted(line("a \t\u{0c}\r"), Some(&FILES), &cwd),
            vec![r#"{"out":"a \t\f"}"#.to_string()]
        );
    }

    #[test]
    fn flags_are_forwarded_and_flagged_lines_are_not_resolved() {
        let cwd = std::env::temp_dir();
        let mut l = line("openjd_output_var: N=@f");
        l.truncated = true;
        assert_eq!(
            emitted(l, Some(&FILES), &cwd),
            vec![r#"{"out":"openjd_output_var: N=@f","truncated":true}"#.to_string()]
        );
        let mut l = line("openjd_output_var: N=@f");
        l.lossy = true;
        assert_eq!(
            emitted(l, Some(&FILES), &cwd),
            vec![r#"{"out":"openjd_output_var: N=@f","lossy":true}"#.to_string()]
        );
    }

    #[test]
    fn without_output_files_a_directive_is_out() {
        let cwd = std::env::temp_dir();
        assert_eq!(
            emitted(line("openjd_output_var: N=@f"), None, &cwd),
            vec![r#"{"out":"openjd_output_var: N=@f"}"#.to_string()]
        );
    }

    #[test]
    fn line_cut_to_fit_the_response_is_flagged() {
        let cwd = std::env::temp_dir();
        let text = "\u{01}".repeat(64 * 1024);
        let out = emitted(line(&text), None, &cwd);
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with(r#","truncated":true}"#));
        assert!(out[0].len() < MAX_RESPONSE_LINE_LENGTH);
    }

    #[test]
    fn missing_file_sends_header_with_error_and_the_line() {
        let cwd = std::env::temp_dir();
        let text = "openjd_output_var: N[2]=@openjd_emit_test_missing_file";
        assert_eq!(
            emitted(line(text), Some(&FILES), &cwd),
            vec![
                format!(
                    r#"{{"outputFile":{{"lineLen":{},"name":[19,20],"task":2,"path":[25,{}],"error":"does not exist","size":0,"validUtf8":0}}}}"#,
                    text.len(),
                    text.len()
                ),
                format!(r#"{{"outputFileData":"{text}"}}"#),
            ]
        );
    }

    #[test]
    fn file_content_follows_the_line_and_long_data_is_split() {
        let cwd = std::env::temp_dir().join(format!("openjd_emit_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir(&cwd).unwrap();
        // Escapes to 6 bytes per char, so 64 KiB needs several frames.
        let content = "\u{01}".repeat(64 * 1024);
        std::fs::write(cwd.join("v"), content.as_bytes()).unwrap();
        std::fs::write(cwd.join("bad"), b"ab\xffcd").unwrap();
        let files = OutputFiles { max_bytes: 1 << 20 };

        let mut frames = Vec::new();
        emit_line(
            line("openjd_output_var: N=@v"),
            Some(&files),
            &cwd,
            &mut |r| frames.push(serde_json::to_string(r).unwrap()),
        );
        assert!(frames[0].starts_with(r#"{"outputFile":{"lineLen":23,"#));
        assert!(frames[0].contains(r#""size":65536,"validUtf8":65536"#));
        assert!(frames.len() > 3, "data split into frames");
        let mut data = String::new();
        for f in &frames[1..] {
            assert!(f.len() < MAX_RESPONSE_LINE_LENGTH);
            let v: serde_json::Value = serde_json::from_str(f).unwrap();
            data.push_str(v["outputFileData"].as_str().unwrap());
        }
        assert_eq!(data, format!("openjd_output_var: N=@v{content}"));

        // Invalid UTF-8: only the valid prefix travels, and validUtf8 says
        // where the first invalid byte is.
        let mut frames = Vec::new();
        emit_line(
            line("openjd_output_var: N=@bad"),
            Some(&files),
            &cwd,
            &mut |r| frames.push(serde_json::to_string(r).unwrap()),
        );
        assert!(
            frames[0].contains(r#""size":5,"validUtf8":2"#),
            "{}",
            frames[0]
        );
        assert_eq!(
            frames[1..],
            [
                r#"{"outputFileData":"openjd_output_var: N=@bad"}"#.to_string(),
                r#"{"outputFileData":"ab"}"#.to_string()
            ]
        );
        std::fs::remove_dir_all(&cwd).unwrap();
    }
}
