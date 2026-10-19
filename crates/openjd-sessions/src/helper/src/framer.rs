// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Bounded line framer for the cross-user helper's stdout reader.
//!
//! Used by both the Unix and Windows runners.
//!
//! Turns arbitrary byte chunks (from `fill_buf`) into bounded logical lines so
//! a newline-less workload cannot grow the line buffer without limit. Splits on
//! `\n` only and strips one `\r` before it, as the parent's direct path does (a
//! lone `\r` elsewhere stays in the line); each line yields one
//! [`FramedLine`], so the caller emits exactly one response per line. A line
//! carries flags that say whether it was cut to fit a limit or had to be
//! decoded lossily, because an `openjd_output_var` directive on such a line
//! is invalid (RFC 0010 §Limits item 1, §Processing a directive item 9).
//!
//! Deliberately `std` + `serde_json` only: it compiles into the standalone
//! helper binary (a nested crate that cannot depend on `openjd-sessions`) and
//! is also `#[path]`-included into `openjd-sessions` under `cfg(test)` for unit
//! tests and the `decode_backslashreplace` equivalence check.
//! So it references nothing from `openjd-sessions`, `nix`, or `tokio`.

use std::borrow::Cow;

/// Max bytes of a raw logical line before truncation; the remainder is
/// discarded.
///
/// Mirrors `subprocess.rs`'s `LOG_LINE_MAX_LENGTH` (duplicated because the
/// helper is a separate crate); the two must stay in sync.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Max bytes of a serialized JSON response line the parent accepts.
///
/// Mirrors the parent's hard limit (`Read::take(128 KiB)`): a longer line
/// becomes a per-line `SessionError::HelperCommunication`. [`fit_out_payload`]
/// keeps every `Response::Out` under it.
pub const MAX_RESPONSE_LINE_LENGTH: usize = 128 * 1024;

/// Lowercase hex digits by nibble value, for rendering bytes as `\xNN`.
const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

/// Decode bytes as UTF-8, escaping each invalid byte as `\xNN` (lowercase hex).
///
/// Ported verbatim from `subprocess.rs::decode_backslashreplace` (the helper
/// cannot depend on `openjd-sessions`); an equivalence test there pins the two.
///
/// Mirrors CPython's `bytes.decode("utf-8", errors="backslashreplace")`.
/// Escaping (vs U+FFFD) preserves the original byte values in the log, which
/// helps identify a subprocess emitting a non-UTF-8 code page (e.g. cp1252).
/// Valid UTF-8 passes through and is returned borrowed without allocating.
pub fn decode_backslashreplace(bytes: &[u8]) -> Cow<'_, str> {
    // Fast path: fully valid UTF-8 needs no allocation.
    if let Ok(valid) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(valid);
    }

    // `utf8_chunks` yields (valid prefix, invalid bytes) pairs. Like CPython,
    // escape every byte of an invalid sequence individually.
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for &byte in chunk.invalid() {
            out.push('\\');
            out.push('x');
            // Nibbles are always < 16.
            out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
            out.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
        }
    }
    Cow::Owned(out)
}

/// Truncate `s` to at most `max` bytes on a UTF-8 char boundary.
///
/// std-only equivalent of `subprocess.rs::truncate_line`'s
/// `floor_char_boundary`; never splits a multi-byte character.
fn truncate_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// One framed line of child output, without its `\n`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FramedLine {
    /// The decoded text, at most [`MAX_LINE_BYTES`] bytes.
    pub text: String,
    /// The line was longer than a limit and the rest was discarded: the raw
    /// line exceeded [`MAX_LINE_BYTES`], or it did once decoded, or (set by
    /// the caller) its response would have exceeded
    /// [`MAX_RESPONSE_LINE_LENGTH`].
    pub truncated: bool,
    /// The kept part of the raw line was not valid UTF-8, so `text` holds
    /// `\xNN` escapes. For a `truncated` line this can differ from the
    /// parent's direct path, which decodes the whole line before it cuts:
    /// invalid bytes past the cut flag the line there but not here, and the
    /// last few characters of the text can differ. A truncated directive is
    /// invalid either way, so only the logged text differs.
    pub lossy: bool,
}

/// Finish one raw line (without its `\n`) and decode it, as the parent's
/// direct path does: strip one trailing `\r`, then cap at `MAX_LINE_BYTES`.
///
/// `buf` holds at most `MAX_LINE_BYTES + 1` bytes, so a CRLF line whose text
/// is exactly at the cap is not cut. `discarded` says the framer already
/// dropped the rest of an over-cap line; its `\r`, if any, was dropped with
/// it. Only the terminator's `\r` is stripped: the `openjd_output_var`
/// grammar decides which other whitespace around a value is allowed, so every
/// reader path must hand it the same text (RFC 0010 §The `openjd_output_var`
/// directive).
///
/// A cut can split a multi-byte character; the bytes of that character before
/// the cut are dropped, so a valid UTF-8 line is cut on a character boundary
/// and is not reported lossy, as on the direct path, which decodes the whole
/// line and then cuts (for a line with invalid bytes, see
/// [`FramedLine::lossy`]). An incomplete sequence at the end of a line that
/// was not cut is escaped as any invalid bytes are. Decoding can expand up to 4x (each bad byte becomes
/// `\xNN`), so the decoded text is cut to the cap again.
fn finish_line(mut bytes: Vec<u8>, discarded: bool) -> FramedLine {
    if !discarded && bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let mut truncated = discarded;
    if bytes.len() > MAX_LINE_BYTES {
        bytes.truncate(MAX_LINE_BYTES);
        truncated = true;
    }
    if truncated {
        if let Err(e) = std::str::from_utf8(&bytes) {
            // `error_len() == None`: the bytes end in an incomplete sequence.
            if e.error_len().is_none() {
                bytes.truncate(e.valid_up_to());
            }
        }
    }
    let decoded = decode_backslashreplace(&bytes);
    let lossy = matches!(decoded, Cow::Owned(_));
    let text = truncate_char_boundary(&decoded, MAX_LINE_BYTES);
    FramedLine {
        truncated: truncated || text.len() < decoded.len(),
        text: text.to_string(),
        lossy,
    }
}

/// serde_json's escaped byte length for one `char` (an equivalence test pins
/// this against serde_json).
///
/// `"` and `\` and the five named controls (`\b \t \n \f \r`) escape to 2 bytes;
/// other controls below `0x20` to `\uXXXX` (6); everything else (incl. `0x7F`
/// and non-ASCII) passes through as raw UTF-8.
fn json_escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' => 2,
        '\u{08}' | '\u{09}' | '\u{0A}' | '\u{0C}' | '\u{0D}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// The largest envelope of a `Response::Out`: `{"out":"..."}` with both flags
/// set, plus `send`'s trailing `\n`. The delimiting `"` are envelope; only the
/// payload's escaped chars count against the budget.
pub const OUT_ENVELOPE: usize = r#"{"out":"","truncated":true,"lossy":true}"#.len() + 1;

/// JSON-expansion guard: longest prefix of `s` whose serialized `Response::Out`
/// (envelope with both flags, plus `send`'s trailing `\n`) fits
/// [`MAX_RESPONSE_LINE_LENGTH`].
///
/// A sub-`MAX_LINE_BYTES` line can still overflow once serialized: control
/// characters expand 6x (`\u0001`), so 64 KiB decoded can reach 384 KiB.
/// Summing escaped lengths and stopping at the budget keeps every line valid
/// and avoids a per-line `HelperCommunication` error. A result shorter than
/// `s` means the line was cut, which the caller reports as truncation.
pub fn fit_out_payload(s: &str) -> &str {
    fit_payload(s, OUT_ENVELOPE)
}

/// Longest prefix of `s` whose JSON-escaped length fits
/// [`MAX_RESPONSE_LINE_LENGTH`] less `envelope` bytes, cut on a char boundary.
pub fn fit_payload(s: &str, envelope: usize) -> &str {
    let budget = MAX_RESPONSE_LINE_LENGTH - envelope;
    let mut used = 0;
    for (i, c) in s.char_indices() {
        let l = json_escaped_len(c);
        if used + l > budget {
            return &s[..i];
        }
        used += l;
    }
    s
}

/// Byte-stream to bounded-logical-line state machine.
///
/// [`push`](LineFramer::push) takes `fill_buf` chunks and emits one
/// [`FramedLine`] per `\n`-terminated line. An over-cap line keeps its
/// truncated 64 KiB prefix, discards the rest to the next `\n`, and still emits
/// its single line once at the line end (same timing as any line), flagged
/// `truncated`; the discarded remainder never becomes a line of its own. [`finish`](LineFramer::finish)
/// flushes a trailing partial line at EOF.
#[derive(Default)]
pub struct LineFramer {
    /// Raw bytes of the current line, capped at `MAX_LINE_BYTES + 1` (room for
    /// a CRLF's `\r` after a line at the cap).
    buf: Vec<u8>,
    /// Set once an over-cap line filled `buf`: the rest up to the next `\n` is
    /// discarded and the `Out` still emits at the line end, not at the cap.
    discarding: bool,
}

impl LineFramer {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            discarding: false,
        }
    }

    /// Feed one chunk, emitting each complete line through `emit`. A chunk may
    /// hold any number of newlines; a line may span many chunks.
    pub fn push<F: FnMut(FramedLine)>(&mut self, mut data: &[u8], emit: &mut F) {
        while !data.is_empty() {
            match data.iter().position(|&b| b == b'\n') {
                Some(nl) => {
                    // Accumulate up to the newline (no-op while discarding),
                    // then emit the kept buffer once at the line end, giving an
                    // over-cap line the same timing as an ordinary one.
                    self.accumulate(&data[..nl]);
                    let line = std::mem::take(&mut self.buf);
                    emit(finish_line(line, self.discarding));
                    // Newline ends the line; reset (buf already emptied above).
                    self.discarding = false;
                    data = &data[nl + 1..];
                }
                None => {
                    self.accumulate(data);
                    return;
                }
            }
        }
    }

    /// Flush the final partial line at EOF (a trailing newline-less line is
    /// still delivered). Emits once when `buf` holds a partial or over-cap
    /// prefix, then resets.
    pub fn finish<F: FnMut(FramedLine)>(&mut self, emit: &mut F) {
        if self.discarding || !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            emit(finish_line(line, self.discarding));
        }
        // buf is already empty here.
        self.discarding = false;
    }

    /// Append a newline-free segment to `buf`. On exceeding
    /// `MAX_LINE_BYTES + 1`, keep the prefix and start discarding the rest; the
    /// line emits later at the line end. While discarding, further bytes are
    /// dropped.
    fn accumulate(&mut self, data: &[u8]) {
        if self.discarding {
            return;
        }
        let room = MAX_LINE_BYTES + 1 - self.buf.len();
        if data.len() <= room {
            self.buf.extend_from_slice(data);
        } else {
            // Over cap: keep the prefix, discard the rest; emit at line end.
            self.buf.extend_from_slice(&data[..room]);
            self.discarding = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── JSON-expansion guard tests ───────────────────────────────────

    const ENVELOPE: usize = OUT_ENVELOPE; // {"out":"...", both flags} + send()'s \n

    #[test]
    fn fit_guard_control_chars_six_x_expansion() {
        // 0x01 serializes to \u0001: 6x expansion.
        let s = "\u{01}".repeat(64 * 1024);
        let fitted = fit_out_payload(&s);
        let json = serde_json::to_string(&fitted).unwrap();
        assert!(json.len() - 2 + ENVELOPE <= MAX_RESPONSE_LINE_LENGTH);
        assert!(fitted.chars().count() + 1 > (MAX_RESPONSE_LINE_LENGTH - ENVELOPE) / 6);
    }

    #[test]
    fn fit_guard_backslashreplace_expansion() {
        // 0x97 -> `\x97` (4 chars), then JSON-escaped backslash: 5 bytes/byte.
        let raw = vec![0x97u8; 64 * 1024];
        let decoded = decode_backslashreplace(&raw);
        let fitted = fit_out_payload(&decoded);
        let json = serde_json::to_string(&fitted).unwrap();
        assert!(json.len() - 2 + ENVELOPE <= MAX_RESPONSE_LINE_LENGTH);
    }

    #[test]
    fn fit_guard_noop_for_plain_ascii() {
        let s = "a".repeat(64 * 1024);
        assert_eq!(fit_out_payload(&s), &s[..]);
    }

    #[test]
    fn fit_guard_cuts_on_char_boundary() {
        let mut s = "\"".repeat((MAX_RESPONSE_LINE_LENGTH - ENVELOPE) / 2 - 1);
        s.push_str("😀😀😀");
        let fitted = fit_out_payload(&s); // not panicking proves it cuts on a char boundary
        let json = serde_json::to_string(&fitted).unwrap();
        assert!(json.len() - 2 + ENVELOPE <= MAX_RESPONSE_LINE_LENGTH);
    }

    #[test]
    fn escaped_len_matches_serde_json_exactly() {
        // Per-char lengths must sum to serde_json's length (minus 2 quotes).
        let s = "abc\"\\\u{08}\u{09}\u{0A}\u{0C}\u{0D}\u{01}\u{1F}€😀\u{7F}";
        let computed: usize = s.chars().map(json_escaped_len).sum();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(computed, json.len() - 2);

        // Exhaustive check: every scalar 0..=0xFF plus representatives at each
        // UTF-8 length must match serde_json's length (minus 2 quotes).
        let representative = [
            0x7Fu32,
            0x80,
            0x7FF,
            0x800,
            0xFFFF,
            0x10000,
            0x10FFFF,
            '€' as u32,
            '😀' as u32,
        ];
        for cp in (0u32..=0xFF).chain(representative) {
            let Some(c) = char::from_u32(cp) else {
                continue; // defensive: no surrogates exist in these ranges
            };
            let serialized = serde_json::to_string(&c.to_string()).unwrap();
            assert_eq!(
                json_escaped_len(c),
                serialized.len() - 2,
                "mismatch for U+{cp:04X}"
            );
        }
    }

    #[test]
    fn out_envelope_within_response_limit_for_worst_cases() {
        // Mirror of the untagged `Response::Out` (`{"out":"..."}`), serialized
        // identically to the real response. fit_out_payload must keep it plus
        // send()'s `\n` within the response limit for worst-case expansions.
        #[derive(serde::Serialize)]
        struct OutMirror<'a> {
            out: &'a str,
            truncated: bool,
            lossy: bool,
        }

        let all_ctrl = "\u{01}".repeat(64 * 1024);
        let backslashreplace = decode_backslashreplace(&vec![0x97u8; 64 * 1024]).into_owned();
        let quote_heavy = "\"".repeat(64 * 1024);

        for payload in [&all_ctrl, &backslashreplace, &quote_heavy] {
            let fitted = fit_out_payload(payload);
            let serialized = serde_json::to_vec(&OutMirror {
                out: fitted,
                truncated: true,
                lossy: true,
            })
            .unwrap();
            // Strict `<` leaves room for send()'s trailing newline.
            assert!(
                serialized.len() < MAX_RESPONSE_LINE_LENGTH,
                "serialized length {} + newline exceeds the response limit",
                serialized.len()
            );
        }
    }

    // ── Framing behavior tests ───────────────────────────────────────

    /// Push each chunk through a fresh framer and collect every emitted line.
    fn frame_lines(chunks: &[&[u8]]) -> Vec<FramedLine> {
        let mut framer = LineFramer::new();
        let mut out = Vec::new();
        for chunk in chunks {
            framer.push(chunk, &mut |s| out.push(s));
        }
        out
    }

    /// The text of each line [`frame_lines`] emits.
    fn frame_chunks(chunks: &[&[u8]]) -> Vec<String> {
        frame_lines(chunks).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn plain_lines_split_on_newline() {
        assert_eq!(
            frame_chunks(&[b"a\nb\n"]),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn crlf_line_end_is_stripped_once() {
        // One `\r` before the `\n` is the line terminator; a second stays.
        assert_eq!(frame_chunks(&[b"a\r\n"]), vec!["a".to_string()]);
        assert_eq!(frame_chunks(&[b"a\r\r\n"]), vec!["a\r".to_string()]);
        // Split across pushes.
        assert_eq!(frame_chunks(&[b"x\r", b"\n"]), vec!["x".to_string()]);
        assert_eq!(frame_chunks(&[b"\r\n"]), vec![String::new()]);
        // At EOF too, as the direct path's read_until does.
        let mut framer = LineFramer::new();
        let mut out = Vec::new();
        framer.push(b"e\r", &mut |l| out.push(l.text));
        framer.finish(&mut |l| out.push(l.text));
        assert_eq!(out, vec!["e".to_string()]);
    }

    #[test]
    fn other_trailing_whitespace_is_kept() {
        // The directive grammar decides what is allowed (the Unix runner used
        // to trim all trailing whitespace).
        assert_eq!(
            frame_chunks(&[b"a \t\x0c\n"]),
            vec!["a \t\u{0c}".to_string()]
        );
    }

    #[test]
    fn lone_cr_stays_in_line() {
        // A bare \r (progress-bar carriage return) does not split a line.
        assert_eq!(
            frame_chunks(&[b"p 10%\rp 20%\n"]),
            vec!["p 10%\rp 20%".to_string()]
        );
    }

    #[test]
    fn multibyte_utf8_across_push_boundary() {
        // "€" (E2 82 AC) split across two pushes must reassemble into one line.
        assert_eq!(
            frame_chunks(&[b"\xE2\x82", b"\xAC\n"]),
            vec!["€".to_string()]
        );
    }

    #[test]
    fn over_cap_line_emits_single_truncated_out() {
        // 1 MiB, no newline: nothing emits while the line is open; the single
        // truncated Out (<= MAX_LINE_BYTES) emits only at the newline.
        let data = vec![b'a'; 1024 * 1024];
        let mut framer = LineFramer::new();
        let mut out = Vec::new();
        framer.push(&data, &mut |s| out.push(s));
        assert!(
            out.is_empty(),
            "no Out until the over-cap line ends, got {}",
            out.len()
        );
        framer.push(b"\n", &mut |s| out.push(s));
        assert_eq!(out.len(), 1, "over-cap line must emit exactly one Out");
        assert_eq!(out[0].text.len(), MAX_LINE_BYTES);
        assert!(out[0].truncated, "an over-cap line is flagged truncated");
        assert!(!out[0].lossy);
    }

    #[test]
    fn over_cap_line_flushed_once_on_finish() {
        // Over-cap line ending at EOF emits exactly one truncated Out via finish().
        let data = vec![b'a'; 1024 * 1024];
        let mut framer = LineFramer::new();
        let mut out = Vec::new();
        framer.push(&data, &mut |s| out.push(s));
        assert!(
            out.is_empty(),
            "no Out while the over-cap line is still open"
        );
        framer.finish(&mut |s| out.push(s));
        assert_eq!(
            out.len(),
            1,
            "over-cap line must flush exactly one Out at EOF"
        );
        assert_eq!(out[0].text.len(), MAX_LINE_BYTES);
        assert!(out[0].truncated, "an over-cap line is flagged truncated");
    }

    #[test]
    fn after_discard_next_line_normal() {
        // After an over-cap line, the following line frames normally.
        let mut data = vec![b'a'; 1024 * 1024];
        data.extend_from_slice(b"\nnext\n");
        let mut framer = LineFramer::new();
        let mut out = Vec::new();
        framer.push(&data, &mut |s| out.push(s));
        assert_eq!(out.len(), 2);
        assert!(out[0].text.len() <= MAX_LINE_BYTES);
        assert!(out[0].truncated);
        assert_eq!(
            out[1],
            FramedLine {
                text: "next".into(),
                truncated: false,
                lossy: false
            },
            "the line after an over-cap one is not flagged"
        );
    }

    #[test]
    fn finish_flushes_partial_line() {
        // A trailing line with no newline is delivered on finish() (EOF).
        let mut framer = LineFramer::new();
        let mut out = Vec::new();
        framer.push(b"partial", &mut |s| out.push(s));
        assert!(out.is_empty());
        framer.finish(&mut |s| out.push(s));
        assert_eq!(
            out,
            vec![FramedLine {
                text: "partial".into(),
                truncated: false,
                lossy: false
            }]
        );
    }

    #[test]
    fn empty_line_emits_empty_out() {
        assert_eq!(frame_chunks(&[b"\n"]), vec![String::new()]);
    }

    #[test]
    fn invalid_utf8_decoded_backslashreplace() {
        // b"a\x97b\n" -> the decoded line "a\x97b", flagged lossy.
        assert_eq!(
            frame_lines(&[b"a\x97b\n"]),
            vec![FramedLine {
                text: r"a\x97b".into(),
                truncated: false,
                lossy: true
            }]
        );
    }

    #[test]
    fn valid_utf8_line_is_not_lossy() {
        let lines = frame_lines(&["a€b\n".as_bytes()]);
        assert!(!lines[0].lossy && !lines[0].truncated);
    }

    #[test]
    fn line_at_cap_is_not_truncated() {
        // Exactly MAX_LINE_BYTES fits: it is not cut, so it is not flagged,
        // with a LF or a CRLF line end.
        for end in [&b"\n"[..], b"\r\n"] {
            let mut data = vec![b'a'; MAX_LINE_BYTES];
            data.extend_from_slice(end);
            let lines = frame_lines(&[&data]);
            assert_eq!(lines[0].text.len(), MAX_LINE_BYTES);
            assert!(!lines[0].truncated, "{end:?}");
        }
        // One byte more is cut and flagged, with either line end.
        for end in [&b"\n"[..], b"\r\n", b"\r\r\n"] {
            let mut data = vec![b'a'; MAX_LINE_BYTES + 1];
            data.extend_from_slice(end);
            let lines = frame_lines(&[&data]);
            assert_eq!(lines[0].text.len(), MAX_LINE_BYTES);
            assert!(lines[0].truncated, "{end:?}");
        }
    }

    #[test]
    fn cut_inside_a_multibyte_character_is_not_lossy() {
        // "€" is 3 bytes; put one across the cap so the cut splits it. The
        // direct path decodes the whole line, then cuts on a character
        // boundary, so the helper must give the same text and no lossy flag.
        let mut data = vec![b'a'; MAX_LINE_BYTES - 1];
        data.extend_from_slice("€€".as_bytes());
        data.push(b'\n');
        let lines = frame_lines(&[&data]);
        assert_eq!(lines[0].text, "a".repeat(MAX_LINE_BYTES - 1));
        assert!(lines[0].truncated);
        assert!(!lines[0].lossy);
    }

    #[test]
    fn incomplete_sequence_at_the_end_of_an_uncut_line_is_escaped() {
        // Not cut, so the partial "€" is invalid UTF-8, as on the direct path.
        assert_eq!(
            frame_lines(&[b"a\xE2\x82\n"]),
            vec![FramedLine {
                text: r"a\xe2\x82".into(),
                truncated: false,
                lossy: true
            }]
        );
    }

    #[test]
    fn cut_line_keeps_an_earlier_invalid_byte() {
        // Only an incomplete sequence at the cut is dropped; an invalid byte
        // earlier in the line is escaped, and the text is cut to the cap.
        let mut data = vec![0x97u8];
        data.extend(vec![b'a'; MAX_LINE_BYTES]);
        data.push(b'\n');
        let lines = frame_lines(&[&data]);
        assert!(lines[0].text.starts_with(r"\x97aaa"));
        assert_eq!(lines[0].text.len(), MAX_LINE_BYTES);
        assert!(lines[0].truncated && lines[0].lossy);
    }

    #[test]
    fn line_that_grows_past_cap_when_decoded_is_truncated_and_lossy() {
        // 20 000 invalid bytes are under the raw cap but decode to 80 000
        // bytes of `\xNN`, so the decoded line is cut back to the cap.
        let mut data = vec![0x97u8; 20_000];
        data.push(b'\n');
        let lines = frame_lines(&[&data]);
        assert!(lines[0].text.len() <= MAX_LINE_BYTES);
        assert!(lines[0].truncated, "cut after decoding");
        assert!(lines[0].lossy);
    }

    #[test]
    fn fit_payload_honors_the_envelope() {
        let s = "a".repeat(MAX_RESPONSE_LINE_LENGTH);
        assert_eq!(fit_payload(&s, 100).len(), MAX_RESPONSE_LINE_LENGTH - 100);
        assert_eq!(
            fit_out_payload(&s).len(),
            MAX_RESPONSE_LINE_LENGTH - OUT_ENVELOPE
        );
    }
}
