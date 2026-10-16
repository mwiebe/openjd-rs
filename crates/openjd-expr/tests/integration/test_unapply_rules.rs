// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Reverse path mapping (`path_mapping::unapply_rules`), the reference inverse
//! of the CONTROL_FLOW RFC's "Path values" section. See
//! `specs/expr/path-mapping.md` § Reverse mapping.

use openjd_expr::path_mapping::{unapply_rules, PathFormat, PathMappingRule, UnmapError};

fn rule(format: PathFormat, src: &str, dst: &str) -> PathMappingRule {
    PathMappingRule {
        source_path_format: format,
        source_path: src.to_string(),
        destination_path: dst.to_string(),
    }
}

fn posix(src: &str, dst: &str) -> PathMappingRule {
    rule(PathFormat::Posix, src, dst)
}

fn windows(src: &str, dst: &str) -> PathMappingRule {
    rule(PathFormat::Windows, src, dst)
}

fn unmap_posix(rules: &[PathMappingRule], path: &str) -> Result<String, UnmapError> {
    unapply_rules(rules, path, PathFormat::Posix)
}

fn unmap_windows(rules: &[PathMappingRule], path: &str) -> Result<String, UnmapError> {
    unapply_rules(rules, path, PathFormat::Windows)
}

// === Step 1: URIs pass through ===

#[test]
fn uri_is_unchanged_with_rules() {
    let rules = [rule(PathFormat::Uri, "s3://bucket/assets", "/mnt/assets")];
    assert_eq!(
        unmap_posix(&rules, "s3://bucket/assets/a.exr"),
        Ok("s3://bucket/assets/a.exr".to_string())
    );
}

#[test]
fn uri_is_unchanged_without_rules() {
    assert_eq!(
        unmap_windows(&[], "https://host/a/./b"),
        Ok("https://host/a/./b".to_string())
    );
}

// === Step 2: relative paths are invalid ===

#[test]
fn posix_relative_is_relative_error() {
    let rules = [posix("/src", "/dst")];
    assert_eq!(unmap_posix(&rules, "out/f.exr"), Err(UnmapError::Relative));
    assert_eq!(unmap_posix(&rules, "./out"), Err(UnmapError::Relative));
    assert_eq!(unmap_posix(&rules, ""), Err(UnmapError::Relative));
}

#[test]
fn relative_is_error_even_without_rules() {
    assert_eq!(unmap_posix(&[], "out/f.exr"), Err(UnmapError::Relative));
    assert_eq!(unmap_windows(&[], "out\\f.exr"), Err(UnmapError::Relative));
}

#[test]
fn posix_backslashes_are_not_a_unc_root() {
    // Under POSIX `\` is an ordinary character, so `\\srv\share` is relative.
    assert_eq!(
        unmap_posix(&[], "\\\\srv\\share\\f"),
        Err(UnmapError::Relative)
    );
}

#[test]
fn posix_drive_spelling_is_relative() {
    assert_eq!(unmap_posix(&[], "C:\\x"), Err(UnmapError::Relative));
}

#[test]
fn windows_non_absolute_forms_are_relative() {
    // Rooted without a drive, drive-relative, bare drive, UNC without a share.
    for path in ["\\x\\f", "/x/f", "C:x", "C:", "\\\\srv", "\\\\srv\\"] {
        assert_eq!(
            unmap_windows(&[], path),
            Err(UnmapError::Relative),
            "{path:?}"
        );
    }
}

#[test]
fn windows_absolute_forms_are_accepted() {
    for path in ["C:\\x", "c:/x", "C:\\", "\\\\srv\\share", "//srv/share/f"] {
        assert_eq!(unmap_windows(&[], path), Ok(path.to_string()), "{path:?}");
    }
}

// === Step 6: no rules, absolute path stored unchanged ===

#[test]
fn no_rules_returns_absolute_path_unchanged_without_normalizing() {
    assert_eq!(
        unmap_posix(&[], "/a/./b/../c/"),
        Ok("/a/./b/../c/".to_string())
    );
}

// === Steps 3–5: candidates and the round trip ===

#[test]
fn rfc_conversion_table_row() {
    // RFC "Processing a directive" conversion table: `P="/mnt/proj/shot01/out"`
    // is stored as `P:\shot01\out` with a rule from `P:\` (WINDOWS) to `/mnt/proj`.
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "/mnt/proj/shot01/out"),
        Ok("P:\\shot01\\out".to_string())
    );
}

#[test]
fn root_source_path_with_no_remaining_components() {
    // No components remain, so the candidate is `source_path` itself, which
    // keeps its root separator. Forward mapping `P:\` gives `/mnt/proj/`, equal
    // to the reported path after normalization.
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(unmap_posix(&rules, "/mnt/proj"), Ok("P:\\".to_string()));
    assert_eq!(unmap_posix(&rules, "/mnt/proj/"), Ok("P:\\".to_string()));
}

#[test]
fn source_path_without_trailing_separator_gets_one() {
    let rules = [windows("P:\\proj", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "/mnt/proj/a/b"),
        Ok("P:\\proj\\a\\b".to_string())
    );
}

#[test]
fn rfc_nested_rule_example() {
    // RFC "Path values" step 4: with `P:\` → `/mnt/p` and `P:\x` → `/data/x`,
    // `/mnt/p/x/f` can't be stored as `P:\x\f`, which maps forward to
    // `/data/x/f`. Rules listed as the Session sorts them (longest source first).
    let rules = [windows("P:\\x", "/data/x"), windows("P:\\", "/mnt/p")];
    assert_eq!(
        unmap_posix(&rules, "/mnt/p/x/f"),
        Err(UnmapError::NoRoundTrip)
    );
    assert_eq!(unmap_posix(&rules, "/data/x/f"), Ok("P:\\x\\f".to_string()));
    assert_eq!(
        unmap_posix(&rules, "/mnt/p/y/f"),
        Ok("P:\\y\\f".to_string())
    );
}

#[test]
fn uri_source_rule_gives_uri() {
    let rules = [rule(PathFormat::Uri, "s3://bucket/assets", "/mnt/assets")];
    assert_eq!(
        unmap_posix(&rules, "/mnt/assets/shots/a.exr"),
        Ok("s3://bucket/assets/shots/a.exr".to_string())
    );
    assert_eq!(
        unmap_posix(&rules, "/mnt/assets"),
        Ok("s3://bucket/assets".to_string())
    );
}

#[test]
fn uri_source_rule_joins_with_slash_on_windows_worker() {
    let rules = [rule(PathFormat::Uri, "s3://bucket", "D:\\assets")];
    assert_eq!(
        unmap_windows(&rules, "D:\\assets\\shots\\a.exr"),
        Ok("s3://bucket/shots/a.exr".to_string())
    );
}

#[test]
fn path_outside_every_destination_is_no_round_trip() {
    // The usual case: a path under the Session Working Directory.
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "/tmp/session-1/out.exr"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn destination_matches_only_on_component_boundary() {
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "/mnt/projX/a"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn reported_path_is_normalized() {
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "/mnt/proj/./a/../b//c/"),
        Ok("P:\\b\\c".to_string())
    );
    // `..` above the root stays at the root.
    assert_eq!(
        unmap_posix(&rules, "/../mnt/proj/b"),
        Ok("P:\\b".to_string())
    );
    // `..` applied before matching: this path is not under the destination.
    assert_eq!(
        unmap_posix(&rules, "/mnt/proj/../other/b"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn destination_path_is_normalized() {
    // Forward mapping with this rule gives `/mnt/proj//b` (it does not
    // normalize), which still compares equal to `/mnt/proj/b`.
    let rules = [windows("P:\\", "/mnt/./proj/")];
    assert_eq!(unmap_posix(&rules, "/mnt/proj/b"), Ok("P:\\b".to_string()));
}

#[test]
fn posix_worker_compares_case_sensitively() {
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "/MNT/proj/a"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn posix_worker_double_slash_root_is_distinct() {
    // Exactly two leading slashes are the POSIX implementation-defined root,
    // kept distinct as `path()` keeps it; three or more collapse to `/`.
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "//mnt/proj/a"),
        Err(UnmapError::NoRoundTrip)
    );
    assert_eq!(
        unmap_posix(&rules, "///mnt/proj/a"),
        Ok("P:\\a".to_string())
    );
}

#[test]
fn windows_worker_compares_case_insensitively_with_both_separators() {
    let rules = [posix("/home/u", "C:\\Users\\U")];
    // Remaining components keep the reported spelling.
    assert_eq!(
        unmap_windows(&rules, "c:/users/u/Docs\\f.txt"),
        Ok("/home/u/Docs/f.txt".to_string())
    );
}

#[test]
fn windows_worker_unc_destination() {
    let rules = [posix("/proj", "\\\\fs\\Share\\proj")];
    assert_eq!(
        unmap_windows(&rules, "//FS/share/proj/a/b"),
        Ok("/proj/a/b".to_string())
    );
    assert_eq!(
        unmap_windows(&rules, "\\\\fs\\other\\proj\\a"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn windows_worker_drive_must_match() {
    let rules = [posix("/proj", "D:\\proj")];
    assert_eq!(
        unmap_windows(&rules, "C:\\proj\\a"),
        Err(UnmapError::NoRoundTrip)
    );
}

// === Candidate order ===

#[test]
fn longest_destination_is_tried_first() {
    // Both candidates map back; the longer destination wins in either listed order.
    let a = posix("/proj/shots", "/dst/a");
    let b = posix("/other", "/dst");
    assert_eq!(
        unmap_posix(&[a.clone(), b.clone()], "/dst/a/f"),
        Ok("/proj/shots/f".to_string())
    );
    assert_eq!(
        unmap_posix(&[b, a], "/dst/a/f"),
        Ok("/proj/shots/f".to_string())
    );
}

#[test]
fn equal_destinations_are_tried_in_listed_order() {
    let x = posix("/x", "/dst");
    let y = posix("/y", "/dst");
    assert_eq!(
        unmap_posix(&[x.clone(), y.clone()], "/dst/f"),
        Ok("/x/f".to_string())
    );
    assert_eq!(unmap_posix(&[y, x], "/dst/f"), Ok("/y/f".to_string()));
}

#[test]
fn failing_candidate_falls_back_to_next() {
    // The longest-destination candidate `/s/f` maps forward through the nested
    // rule `/s/f` to `/elsewhere`, so the shorter destination's candidate wins.
    let rules = [
        posix("/s/f", "/elsewhere"),
        posix("/s", "/m/c"),
        posix("/t", "/m"),
    ];
    assert_eq!(unmap_posix(&rules, "/m/c/f"), Ok("/t/c/f".to_string()));
}

#[test]
fn candidate_that_no_rule_matches_round_trips_by_identity() {
    // Forward mapping leaves a value no rule matches unchanged. With an empty
    // `source_path`, the candidate `/f` is matched by no rule (rootedness
    // differs), so it maps forward to itself: the reported path.
    let rules = [posix("", "/")];
    assert_eq!(unmap_posix(&rules, "/f"), Ok("/f".to_string()));
}

// === Errors ===

#[test]
fn error_messages() {
    assert_eq!(
        UnmapError::Relative.to_string(),
        "the path is relative; a PATH value must be an absolute path or a URI"
    );
    assert_eq!(
        UnmapError::NoRoundTrip.to_string(),
        "no path mapping rule maps the path back to the submission path context, \
         so it names a location only this worker can see"
    );
}
