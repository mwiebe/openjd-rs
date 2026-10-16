// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Path format and mapping rules.
//!
//! Mirrors Python `openjd.expr._path_mapping`.

use serde::{Deserialize, Serialize};

/// Path format (POSIX, Windows, or URI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PathFormat {
    #[serde(alias = "posix", alias = "Posix")]
    Posix,
    #[serde(alias = "windows", alias = "Windows")]
    Windows,
    #[serde(alias = "URI", alias = "uri", alias = "Uri")]
    Uri,
}

impl PathFormat {
    pub fn host() -> Self {
        if cfg!(windows) {
            PathFormat::Windows
        } else {
            PathFormat::Posix
        }
    }
}

/// A path mapping rule.
///
/// Serializes to/from the JSON format specified in the OpenJD spec:
/// <https://github.com/OpenJobDescription/openjd-specifications/wiki/How-Jobs-Are-Run#path-mapping>
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathMappingRule {
    pub source_path_format: PathFormat,
    pub source_path: String,
    pub destination_path: String,
}

impl PathMappingRule {
    /// Apply this rule using host-native output separators.
    /// Equivalent to Python's behavior (uses `os.name` to pick separator).
    pub fn apply(&self, path: &str) -> Option<String> {
        self.apply_with_format(path, PathFormat::host())
    }

    /// Apply this rule, using `output_format` to determine the output separator.
    /// - `Posix` / `Uri` → `/`
    /// - `Windows` → `\`
    pub fn apply_with_format(&self, path: &str, output_format: PathFormat) -> Option<String> {
        let sep = match output_format {
            PathFormat::Windows => '\\',
            _ => '/',
        };
        match self.source_path_format {
            PathFormat::Uri => self.apply_uri(path, sep),
            PathFormat::Posix => self.apply_filesystem(path, false, sep),
            PathFormat::Windows => self.apply_filesystem(path, true, sep),
        }
    }

    /// Find the byte offset where the path component begins in a URI (after `scheme://authority`).
    /// Returns `None` if there is no `://` separator.
    fn uri_path_start(uri: &str) -> Option<usize> {
        let authority_start = uri.find("://")? + 3;
        Some(
            uri[authority_start..]
                .find('/')
                .map_or(uri.len(), |i| authority_start + i),
        )
    }

    fn apply_uri(&self, path: &str, sep: char) -> Option<String> {
        // Per RFC 3986: scheme and authority are case-insensitive, path is case-sensitive.
        let src_path_start = Self::uri_path_start(&self.source_path).unwrap_or(0);
        let inp_path_start = Self::uri_path_start(path).unwrap_or(0);

        // Scheme+authority must match case-insensitively
        if !path[..inp_path_start].eq_ignore_ascii_case(&self.source_path[..src_path_start]) {
            return None;
        }
        // Path portion must match case-sensitively
        let src_path = &self.source_path[src_path_start..];
        let inp_path = &path[inp_path_start..];
        if !inp_path.starts_with(src_path) {
            return None;
        }
        let remainder = &inp_path[src_path.len()..];
        if !remainder.is_empty() && !remainder.starts_with('/') {
            return None;
        }
        let child_parts: Vec<&str> = if remainder.is_empty() {
            Vec::new()
        } else {
            remainder[1..].split('/').collect()
        };
        let mut result = self.destination_path.clone();
        for part in &child_parts {
            result.push(sep);
            result.push_str(part);
        }
        if path.ends_with('/') && !result.ends_with(sep) {
            result.push(sep);
        }
        Some(result)
    }

    /// Compare and rewrite a filesystem path against this rule.
    ///
    /// `windows` names the semantics of the rule's *source* format, and drives
    /// four decisions that all follow from it: which characters separate
    /// components, whether components compare case-insensitively, what counts
    /// as a rooted path, and what counts as a trailing separator. This
    /// parameter used to be called `case_insensitive` and was reused for the
    /// other three; the values coincide today because only `PathFormat::Windows`
    /// is case-insensitive, but the concepts are distinct, so it is named for
    /// the format rather than for one of its consequences.
    fn apply_filesystem(&self, path: &str, windows: bool, sep: char) -> Option<String> {
        // A relative path must never match an absolute rule (or vice versa).
        // Python's pathlib.is_relative_to gets this from the root in .parts;
        // split_path_parts discards it, so compare rootedness explicitly.
        if is_rooted(&self.source_path, windows) != is_rooted(path, windows) {
            return None;
        }

        let source_parts = split_path_parts(&self.source_path, windows);
        let path_parts = split_path_parts(path, windows);

        if path_parts.len() < source_parts.len() {
            return None;
        }

        for (sp, pp) in source_parts.iter().zip(path_parts.iter()) {
            let matches = if windows {
                sp.eq_ignore_ascii_case(pp)
            } else {
                sp == pp
            };
            if !matches {
                return None;
            }
        }

        let remaining = &path_parts[source_parts.len()..];
        let mut result = self.destination_path.clone();
        for part in remaining {
            result.push(sep);
            result.push_str(part);
        }

        if has_trailing_slash(path, windows) && !result.ends_with(sep) {
            result.push(sep);
        }

        Some(result)
    }
}

/// Split a path into parts on the separators the format actually recognizes.
///
/// Windows accepts both `/` and `\`. POSIX accepts only `/` — there, a
/// backslash is an ordinary filename character, so `home\user` is one
/// component and not two. Splitting on `\` unconditionally let a POSIX rule
/// for `/home/user` match the input `/home\user/f`, which Python rejects:
/// `PurePosixPath("/home\\user/f").parts` is `('/', 'home\\user', 'f')` and
/// `is_relative_to("/home/user")` is `False`.
///
/// Preserves drive letters as the first part (e.g., "C:" from "C:\foo").
fn split_path_parts(path: &str, windows: bool) -> Vec<&str> {
    let separators: &[char] = if windows { &['/', '\\'] } else { &['/'] };
    path.split(separators).filter(|s| !s.is_empty()).collect()
}

/// Whether a path is anchored at a root separator. Drive-anchored Windows
/// paths need no special case here: `split_path_parts` keeps `C:` as the
/// first segment, so drive mismatches already fail the segment comparison.
fn is_rooted(path: &str, windows: bool) -> bool {
    path.starts_with('/') || (windows && path.starts_with('\\'))
}

/// Check if a path has a trailing slash.
/// For Windows paths, both `\` and `/` count.
fn has_trailing_slash(path: &str, windows: bool) -> bool {
    if windows {
        path.ends_with('\\') || path.ends_with('/')
    } else {
        path.ends_with('/')
    }
}

/// Apply a list of path mapping rules to a path. First match wins.
///
/// Rules must be pre-sorted by decreasing `source_path` length (longest match first).
/// The session layer handles this sorting; see `Session::with_config()`.
#[must_use]
pub fn apply_rules(rules: &[PathMappingRule], path: &str) -> String {
    apply_rules_with_format(rules, path, PathFormat::host())
}

/// Apply rules with an explicit output format.
///
/// Rules must be pre-sorted by decreasing `source_path` length (longest match first).
#[must_use]
pub fn apply_rules_with_format(
    rules: &[PathMappingRule],
    path: &str,
    output_format: PathFormat,
) -> String {
    for rule in rules {
        if let Some(mapped) = rule.apply_with_format(path, output_format) {
            return mapped;
        }
    }
    path.to_string()
}

/// Why [`unapply_rules`] could not map a path back to the submission path
/// context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UnmapError {
    /// The path is neither a URI nor absolute under the worker's path format.
    #[error("the path is relative; a PATH value must be an absolute path or a URI")]
    Relative,
    /// The Session has path mapping rules, and no candidate built from them
    /// maps forward to the reported path.
    #[error(
        "no path mapping rule maps the path back to the submission path context, \
         so it names a location only this worker can see"
    )]
    NoRoundTrip,
}

/// Map a path reported on the worker back to the submission path context.
///
/// Implements the reference inverse of the CONTROL_FLOW RFC's "Path values"
/// section:
///
/// 1. A URI (see [`is_uri`]) is returned unchanged.
/// 2. Any other path must be absolute under `worker_format`, or the result is
///    [`UnmapError::Relative`].
/// 3. With no rules, the absolute path is returned unchanged.
/// 4. Otherwise the path and each rule's `destination_path` are lexically
///    normalized (`.` removed, `..` applied, trailing separators dropped) and
///    compared under `worker_format`'s rules (Windows: case-insensitive, `\`
///    and `/` both separators). Each rule whose `destination_path` is the path,
///    or a prefix of it ending on a component boundary, gives a candidate: the
///    remaining components joined with the separator of the rule's
///    `source_path_format` (`/` for `URI`) and appended to `source_path`, or
///    `source_path` itself when none remain.
/// 5. Candidates are tried by decreasing `destination_path` length (in
///    components), then in the order of `rules`. The first candidate that
///    [`apply_rules_with_format`] maps forward, with output separators of
///    `worker_format`, to a path equal to the reported one after normalization
///    is returned. If none does, the result is [`UnmapError::NoRoundTrip`].
///    Forward mapping is compared normalized because it does not normalize: a
///    `destination_path` ending in a separator gives `/mnt/p//x`.
///
/// `rules` MUST be the producing Session's full rule list in the exact order
/// the Session applies them (as `openjd_sessions::Session::path_mapping_rules`
/// returns it: sorted by decreasing `source_path` length, ties in a fixed
/// order), because the round trip forward-maps with `rules` and must
/// reproduce the Session's own forward mapping. A different order, even among
/// equal-length sources, can accept a value that the Session maps elsewhere.
/// Do not filter `rules` (for example by source format) before the call: the
/// no-rules case and the round trip are defined on the Session's full rules.
/// The RFC's SHOULD to use only rules from the submission path format is not
/// implemented, since no openjd runtime knows that format. Given the Session's
/// order, the order of equal-length destinations only breaks ties among
/// candidates. `PathFormat::Uri` as `worker_format` is treated as POSIX.
pub fn unapply_rules(
    rules: &[PathMappingRule],
    path: &str,
    worker_format: PathFormat,
) -> Result<String, UnmapError> {
    unapply_rules_with_source_format(rules, path, worker_format).map(|(stored, _)| stored)
}

/// [`unapply_rules`], also returning the path format of the stored form.
///
/// The format is [`PathFormat::Uri`] for a URI returned unchanged,
/// `worker_format` for an absolute path returned unchanged because there are
/// no rules, and otherwise the `source_path_format` of the rule whose
/// candidate was returned. A caller that stores the result as an
/// `ExprValue::Path` uses it as the value's format, so the value carries the
/// format of the submission path context it is in.
pub fn unapply_rules_with_source_format(
    rules: &[PathMappingRule],
    path: &str,
    worker_format: PathFormat,
) -> Result<(String, PathFormat), UnmapError> {
    if is_uri(path) {
        return Ok((path.to_string(), PathFormat::Uri));
    }
    let windows = worker_format == PathFormat::Windows;
    if !is_absolute_on_worker(path, windows) {
        return Err(UnmapError::Relative);
    }
    if rules.is_empty() {
        return Ok((path.to_string(), worker_format));
    }

    let reported = NormalPath::new(path, windows);
    // (destination component count, candidate, its rule's source format),
    // in the caller's rule order.
    let mut candidates: Vec<(usize, String, PathFormat)> = rules
        .iter()
        .filter_map(|rule| {
            let dest = NormalPath::new(&rule.destination_path, windows);
            let remaining = reported.strip_prefix(&dest, windows)?;
            Some((
                dest.parts.len(),
                build_candidate(rule, remaining),
                rule.source_path_format,
            ))
        })
        .collect();
    // A stable sort, so equal lengths keep the caller's order.
    candidates.sort_by_key(|(len, _, _)| std::cmp::Reverse(*len));

    candidates
        .into_iter()
        .map(|(_, candidate, format)| (candidate, format))
        .find(|(candidate, _)| {
            let forward = apply_rules_with_format(rules, candidate, worker_format);
            NormalPath::new(&forward, windows).equals(&reported, windows)
        })
        .ok_or(UnmapError::NoRoundTrip)
}

/// Join the components left after a rule's `destination_path` onto its
/// `source_path`, with the separator of its `source_path_format`.
fn build_candidate(rule: &PathMappingRule, remaining: &[&str]) -> String {
    if remaining.is_empty() {
        return rule.source_path.clone();
    }
    let source_windows = rule.source_path_format == PathFormat::Windows;
    let sep = if source_windows { '\\' } else { '/' };
    let mut candidate = rule.source_path.clone();
    if !has_trailing_slash(&candidate, source_windows) {
        candidate.push(sep);
    }
    candidate.push_str(&remaining.join(&sep.to_string()));
    candidate
}

/// Whether a filesystem path is absolute in the worker's format.
///
/// POSIX: a leading `/`. Windows: a drive with a root (`C:\`, `C:/`) or a UNC
/// path with a non-empty server and share (`\\server\share`). A Windows path
/// rooted without a drive (`\x`) or drive-relative (`C:x`) is not absolute.
/// This agrees with `pathlib` on ordinary forms; Python versions disagree on
/// some edge forms (see `specs/expr/path-mapping.md`).
/// This differs from `is_rooted`, which serves forward matching and does not
/// treat drive paths as rooted.
fn is_absolute_on_worker(path: &str, windows: bool) -> bool {
    if !windows {
        return path.starts_with('/');
    }
    match windows_anchor(path) {
        WindowsAnchor::Drive { rooted, .. } => rooted,
        WindowsAnchor::Unc { server, share, .. } => !server.is_empty() && !share.is_empty(),
        WindowsAnchor::Root { .. } | WindowsAnchor::None => false,
    }
}

/// The anchor of a Windows path, as `PureWindowsPath` parses it.
enum WindowsAnchor<'a> {
    /// `C:` (drive-relative) or `C:\` (`rooted`), followed by `rest`.
    Drive {
        drive: &'a str,
        rooted: bool,
        rest: &'a str,
    },
    /// `\\server\share`, followed by `rest`. One separator ends the server.
    Unc {
        server: &'a str,
        share: &'a str,
        rest: &'a str,
    },
    /// A root without a drive, `\x`.
    Root { rest: &'a str },
    /// A relative path.
    None,
}

fn windows_anchor(path: &str) -> WindowsAnchor<'_> {
    let is_sep = |b: u8| b == b'\\' || b == b'/';
    let bytes = path.as_bytes();
    // Every slice below follows ASCII bytes, so it falls on a char boundary.
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let rooted = bytes.get(2).is_some_and(|&b| is_sep(b));
        return WindowsAnchor::Drive {
            drive: &path[..2],
            rooted,
            rest: &path[if rooted { 3 } else { 2 }..],
        };
    }
    if bytes.len() >= 2 && is_sep(bytes[0]) && is_sep(bytes[1]) {
        let rest = &path[2..];
        let (server, after_server) = match rest.find(['\\', '/']) {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let share_end = after_server.find(['\\', '/']).unwrap_or(after_server.len());
        return WindowsAnchor::Unc {
            server,
            share: &after_server[..share_end],
            rest: &after_server[share_end..],
        };
    }
    if bytes.first().is_some_and(|&b| is_sep(b)) {
        return WindowsAnchor::Root { rest: &path[1..] };
    }
    WindowsAnchor::None
}

/// A filesystem path in lexical normal form: its anchor and its components.
///
/// `.` components and empty components (runs of separators, trailing
/// separators) are dropped, and `..` removes the preceding component. A `..`
/// directly below an anchor that has a root is dropped, as the OS resolves
/// `/..` to `/`. Symbolic links are not resolved. (`..` handling under an
/// anchor without a root, and the `\`/empty anchors, only arise for a
/// non-absolute `destination_path`, which never matches; they are kept so the
/// normal form is defined for every input.)
///
/// The anchor is kept with canonical separators: POSIX `/` (any number of
/// leading slashes); Windows `X:\`, `X:`, `\\server\share\`, or `\`.
struct NormalPath<'a> {
    anchor: String,
    parts: Vec<&'a str>,
}

impl<'a> NormalPath<'a> {
    fn new(path: &'a str, windows: bool) -> Self {
        let (anchor, rest) = split_anchor(path, windows);
        let rooted = anchor.ends_with(['/', '\\']);
        let mut parts: Vec<&str> = Vec::new();
        for part in split_path_parts(rest, windows) {
            match part {
                "." => {}
                ".." => match parts.last() {
                    Some(&last) if last != ".." => {
                        parts.pop();
                    }
                    _ if rooted => {}
                    _ => parts.push(part),
                },
                _ => parts.push(part),
            }
        }
        NormalPath { anchor, parts }
    }

    /// If `prefix` names this path or an ancestor of it, the components of this
    /// path below it.
    fn strip_prefix(&self, prefix: &NormalPath<'_>, windows: bool) -> Option<&[&'a str]> {
        if !component_eq(&self.anchor, &prefix.anchor, windows)
            || prefix.parts.len() > self.parts.len()
        {
            return None;
        }
        let (head, tail) = self.parts.split_at(prefix.parts.len());
        head.iter()
            .zip(&prefix.parts)
            .all(|(a, b)| component_eq(a, b, windows))
            .then_some(tail)
    }

    fn equals(&self, other: &NormalPath<'_>, windows: bool) -> bool {
        self.parts.len() == other.parts.len() && self.strip_prefix(other, windows).is_some()
    }
}

/// Compare two components (or anchors) under the worker format's rules.
fn component_eq(a: &str, b: &str, windows: bool) -> bool {
    if windows {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Split a filesystem path into its canonical anchor and the rest.
///
/// POSIX's implementation-defined `//` root is folded into `/`: forward mapping
/// onto a root `destination_path` gives `//x` (it appends a separator to one
/// that already ends with one), and that must compare equal to `/x`.
fn split_anchor(path: &str, windows: bool) -> (String, &str) {
    if !windows {
        return match path.strip_prefix('/') {
            Some(_) => ("/".to_string(), path.trim_start_matches('/')),
            None => (String::new(), path),
        };
    }
    match windows_anchor(path) {
        WindowsAnchor::Drive {
            drive,
            rooted,
            rest,
        } => (
            if rooted {
                format!("{drive}\\")
            } else {
                drive.to_string()
            },
            rest,
        ),
        WindowsAnchor::Unc {
            server,
            share,
            rest,
        } => (format!("\\\\{server}\\{share}\\"), rest),
        WindowsAnchor::Root { rest } => ("\\".to_string(), rest),
        WindowsAnchor::None => (String::new(), path),
    }
}

/// Check if a string is a URI (has a `scheme://` prefix).
///
/// Delegates to [`crate::uri_path::is_uri`]: the scheme must be at least two
/// characters, so `C://data/x` is a Windows drive path, not a URI.
pub fn is_uri(path: &str) -> bool {
    crate::uri_path::is_uri(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec JSON from https://github.com/OpenJobDescription/openjd-specifications/wiki/How-Jobs-Are-Run#path-mapping
    const SPEC_JSON: &str = r#"{
        "version": "pathmapping-1.0",
        "path_mapping_rules": [
            {
                "source_path_format": "POSIX",
                "source_path": "/home/user",
                "destination_path": "/mnt/shared/user"
            },
            {
                "source_path_format": "WINDOWS",
                "source_path": "C:\\Users\\user",
                "destination_path": "/mnt/shared/user"
            }
        ]
    }"#;

    #[derive(Serialize, Deserialize)]
    struct PathMappingFile {
        version: String,
        path_mapping_rules: Vec<PathMappingRule>,
    }

    #[test]
    fn deserialize_spec_json() {
        let file: PathMappingFile = serde_json::from_str(SPEC_JSON).unwrap();
        assert_eq!(file.version, "pathmapping-1.0");
        assert_eq!(file.path_mapping_rules.len(), 2);

        assert_eq!(
            file.path_mapping_rules[0].source_path_format,
            PathFormat::Posix
        );
        assert_eq!(file.path_mapping_rules[0].source_path, "/home/user");
        assert_eq!(
            file.path_mapping_rules[0].destination_path,
            "/mnt/shared/user"
        );

        assert_eq!(
            file.path_mapping_rules[1].source_path_format,
            PathFormat::Windows
        );
        assert_eq!(file.path_mapping_rules[1].source_path, "C:\\Users\\user");
        assert_eq!(
            file.path_mapping_rules[1].destination_path,
            "/mnt/shared/user"
        );
    }

    #[test]
    fn serialize_roundtrip() {
        let file: PathMappingFile = serde_json::from_str(SPEC_JSON).unwrap();
        let json = serde_json::to_string(&file).unwrap();
        let roundtrip: PathMappingFile = serde_json::from_str(&json).unwrap();

        assert_eq!(
            roundtrip.path_mapping_rules.len(),
            file.path_mapping_rules.len()
        );
        for (a, b) in file
            .path_mapping_rules
            .iter()
            .zip(roundtrip.path_mapping_rules.iter())
        {
            assert_eq!(a.source_path_format, b.source_path_format);
            assert_eq!(a.source_path, b.source_path);
            assert_eq!(a.destination_path, b.destination_path);
        }
    }

    #[test]
    fn serialize_posix_format() {
        let rule = PathMappingRule {
            source_path_format: PathFormat::Posix,
            source_path: "/src".into(),
            destination_path: "/dst".into(),
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["source_path_format"], "POSIX");
    }

    #[test]
    fn serialize_windows_format() {
        let rule = PathMappingRule {
            source_path_format: PathFormat::Windows,
            source_path: "C:\\src".into(),
            destination_path: "/dst".into(),
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["source_path_format"], "WINDOWS");
    }

    #[test]
    fn serialize_uri_format() {
        let rule = PathMappingRule {
            source_path_format: PathFormat::Uri,
            source_path: "s3://bucket/assets".into(),
            destination_path: "/mnt/assets".into(),
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["source_path_format"], "URI");
    }

    #[test]
    fn deserialize_uri_format() {
        let json =
            r#"{"source_path_format":"URI","source_path":"s3://bucket","destination_path":"/mnt"}"#;
        let rule: PathMappingRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.source_path_format, PathFormat::Uri);
    }

    #[test]
    fn serialize_field_names_match_spec() {
        let rule = PathMappingRule {
            source_path_format: PathFormat::Posix,
            source_path: "/a".into(),
            destination_path: "/b".into(),
        };
        let json = serde_json::to_value(&rule).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("source_path_format"));
        assert!(obj.contains_key("source_path"));
        assert!(obj.contains_key("destination_path"));
        assert_eq!(obj.len(), 3, "no extra fields");
    }
}
