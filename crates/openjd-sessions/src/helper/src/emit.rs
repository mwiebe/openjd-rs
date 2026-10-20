// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Sending one framed line of child output to the parent: as a
//! `Response::Out` line; for an Action that may report output values, a
//! directive line too long for one response as `Response::OutLong` and `Data`
//! frames; and an `@` directive as `Response::OutputFile` and `Data` frames
//! that carry the file the helper read as the job user (RFC 0011 §File values
//! item 3).
//!
//! Shared by the Unix and Windows runners.

use crate::framer::{fit_out_payload, fit_payload, FramedLine};
use crate::output_file::{
    read_output_file, recognize_file_directive, FileDirective, DIRECTIVE_PREFIX,
};
use crate::protocol::{OutLongHeader, OutputFileHeader, OutputFiles, Response};
use std::path::Path;

/// The envelope of a `Data` frame, plus `send`'s trailing `\n`.
const DATA_ENVELOPE: usize = r#"{"data":""}"#.len() + 1;

/// Send one line of child output through `send`.
///
/// The framer has already stripped the line terminator, as the parent's
/// direct path does, so the directive grammar sees the same text on every
/// path. With `output_files` set (the Action may report output values):
///
/// * a line the helper recognizes as an `@` directive is resolved: the file is
///   read beneath `cwd` and sent as frames in place of the line. A line that
///   was truncated or lossily decoded is never resolved; it goes to the parent
///   with its flags, and the parent rejects it.
/// * any other directive line that does not fit one `Out` response once
///   escaped is sent whole, as `OutLong` and `Data` frames, so a valid line of
///   up to 64 KiB is never cut (RFC 0011 §Limits item 1).
///
/// Any other line that does not fit is cut and flagged `truncated`.
pub fn emit_line(
    line: FramedLine,
    output_files: Option<&OutputFiles>,
    cwd: &Path,
    send: &mut impl FnMut(&Response),
) {
    if let Some(output_files) = output_files {
        if !line.truncated && !line.lossy {
            if let Some(directive) = recognize_file_directive(&line.text) {
                emit_output_file(&line.text, directive, output_files.max_bytes, cwd, send);
                return;
            }
        }
    }
    let payload = fit_out_payload(&line.text);
    if payload.len() < line.text.len()
        && output_files.is_some()
        && line.text.starts_with(DIRECTIVE_PREFIX)
    {
        send(&Response::OutLong {
            out_long: OutLongHeader {
                len: line.text.len(),
                truncated: line.truncated,
                lossy: line.lossy,
            },
        });
        send_data(&line.text, send);
        return;
    }
    send(&Response::Out {
        truncated: line.truncated || payload.len() < line.text.len(),
        out: payload.to_string(),
        lossy: line.lossy,
    });
}

/// Send `text` as `Data` frames that each fit the response line limit.
fn send_data(text: &str, send: &mut impl FnMut(&Response)) {
    let mut rest = text;
    while !rest.is_empty() {
        let chunk = fit_payload(rest, DATA_ENVELOPE);
        send(&Response::Data {
            data: chunk.to_string(),
        });
        rest = &rest[chunk.len()..];
    }
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
    send_data(line, send);
    send_data(valid, send);
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

    /// The concatenated `data` of `frames`.
    fn data_of(frames: &[String]) -> String {
        frames
            .iter()
            .map(|f| {
                assert!(f.len() < MAX_RESPONSE_LINE_LENGTH, "frame over the limit");
                let v: serde_json::Value = serde_json::from_str(f).unwrap();
                v["data"].as_str().unwrap().to_string()
            })
            .collect()
    }

    const FILES: OutputFiles = OutputFiles { max_bytes: 10 };

    #[test]
    fn ordinary_line_is_out_unchanged() {
        let cwd = std::env::temp_dir();
        assert_eq!(
            emitted(line("a \t\u{0c}"), Some(&FILES), &cwd),
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
    fn ordinary_line_cut_to_fit_the_response_is_flagged() {
        let cwd = std::env::temp_dir();
        let text = "\u{01}".repeat(64 * 1024);
        for files in [None, Some(&FILES)] {
            let out = emitted(line(&text), files, &cwd);
            assert_eq!(out.len(), 1);
            assert!(out[0].ends_with(r#","truncated":true}"#));
            assert!(out[0].len() < MAX_RESPONSE_LINE_LENGTH);
        }
        // Without output_files, a directive line is cut like any other.
        let text = format!("openjd_output_var: S=\"{}\"", "\\".repeat(65_510));
        let out = emitted(line(&text), None, &cwd);
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with(r#","truncated":true}"#));
    }

    #[test]
    fn long_directive_line_is_sent_whole() {
        let cwd = std::env::temp_dir();
        // Under the 64 KiB line cap, but over 128 KiB once escaped: each
        // backslash escapes to two bytes.
        let text = format!("openjd_output_var: S=\"{}\"", "\\".repeat(65_510));
        assert!(text.len() <= 64 * 1024);
        let out = emitted(line(&text), Some(&FILES), &cwd);
        assert_eq!(out[0], format!(r#"{{"outLong":{{"len":{}}}}}"#, text.len()));
        assert!(out.len() >= 2, "header then data frames");
        assert!(data_of(&out[1..]) == text, "the data is the whole line");
        // Flags travel on the header.
        let mut l = line(&format!(
            "openjd_output_var: S=\"{}",
            "\u{01}".repeat(30_000)
        ));
        l.lossy = true;
        let out = emitted(l.clone(), Some(&FILES), &cwd);
        assert_eq!(
            out[0],
            format!(r#"{{"outLong":{{"len":{},"lossy":true}}}}"#, l.text.len())
        );
        // A cut line that escaping makes too long for one response keeps its
        // truncated flag, so the parent rejects it.
        let mut l = line(&format!("openjd_output_var: S={}", "\\".repeat(65_515)));
        l.truncated = true;
        let out = emitted(l.clone(), Some(&FILES), &cwd);
        assert_eq!(
            out[0],
            format!(
                r#"{{"outLong":{{"len":{},"truncated":true}}}}"#,
                l.text.len()
            )
        );
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
                format!(r#"{{"data":"{text}"}}"#),
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

        let frames = emitted(line("openjd_output_var: N=@v"), Some(&files), &cwd);
        assert!(frames[0].starts_with(r#"{"outputFile":{"lineLen":23,"#));
        assert!(frames[0].contains(r#""size":65536,"validUtf8":65536"#));
        assert!(frames.len() > 3, "data split into frames");
        assert_eq!(
            data_of(&frames[1..]),
            format!("openjd_output_var: N=@v{content}")
        );

        // Invalid UTF-8: only the valid prefix travels, and validUtf8 says
        // where the first invalid byte is.
        let frames = emitted(line("openjd_output_var: N=@bad"), Some(&files), &cwd);
        assert!(
            frames[0].contains(r#""size":5,"validUtf8":2"#),
            "{}",
            frames[0]
        );
        assert_eq!(
            frames[1..],
            [
                r#"{"data":"openjd_output_var: N=@bad"}"#.to_string(),
                r#"{"data":"ab"}"#.to_string()
            ]
        );

        // At most max_bytes + 1 bytes are read.
        let small = OutputFiles { max_bytes: 3 };
        let frames = emitted(line("openjd_output_var: N=@v"), Some(&small), &cwd);
        assert!(
            frames[0].contains(r#""size":4,"validUtf8":4"#),
            "{}",
            frames[0]
        );
        std::fs::remove_dir_all(&cwd).unwrap();
    }
}
