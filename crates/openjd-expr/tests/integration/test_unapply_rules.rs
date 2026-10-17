// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Reverse path mapping (`path_mapping::unapply_rules`), the reference inverse
//! of the CONTROL_FLOW RFC's "Path values" section. See
//! `specs/expr/path-mapping.md` § Reverse mapping.

use openjd_expr::path_mapping::{
    unapply_rules, unapply_rules_with_source_format, PathFormat, PathMappingRule, UnmapError,
};

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

// Steps follow specs/expr/path-mapping.md § Reverse mapping.

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

#[test]
fn doubled_separator_drive_path_is_not_a_uri() {
    // `is_uri` needs a scheme of two or more characters, so `C://proj/a` is a
    // Windows drive path and is mapped back, not stored unchanged.
    let rules = [posix("/proj", "C:\\proj")];
    assert_eq!(
        unmap_windows(&rules, "C://proj/a"),
        Ok("/proj/a".to_string())
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
    // Rooted without a drive (with one- and multi-character names),
    // drive-relative, bare drive, non-letter drive, UNC without a share, UNC
    // without a server, and UNC with an empty share (the server ends at the
    // first separator).
    for path in [
        "\\x\\f",
        "\\ab\\cd\\f",
        "/x/f",
        "C:x",
        "C:",
        "1:\\x",
        "\\\\srv",
        "\\\\srv\\",
        "\\\\\\share\\x",
        "\\\\srv\\\\share\\x",
    ] {
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

#[test]
fn windows_mixed_separator_unc_is_absolute() {
    // The path `\/srv/sh/f` has the `PureWindowsPath` anchor `\\srv\sh\`.
    assert_eq!(
        unmap_windows(&[], "\\/srv/sh/f"),
        Ok("\\/srv/sh/f".to_string())
    );
    let rules = [posix("/p", "\\\\srv\\sh")];
    assert_eq!(unmap_windows(&rules, "\\/srv/sh/f"), Ok("/p/f".to_string()));
}

// === Step 3: no rules, absolute path returned unchanged ===

#[test]
fn no_rules_returns_absolute_path_unchanged_without_normalizing() {
    assert_eq!(
        unmap_posix(&[], "/a/./b/../c/"),
        Ok("/a/./b/../c/".to_string())
    );
}

// === Steps 4–5: candidates and the round trip ===

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
    // Forward mapping with this rule gives `/mnt/./proj//b` (it does not
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
fn posix_worker_leading_slashes_fold_to_one_root() {
    // POSIX's implementation-defined `//` root is folded into `/`, so a
    // reported `//x` or `///x` matches a destination under `/`.
    let rules = [windows("P:\\", "/mnt/proj")];
    assert_eq!(unmap_posix(&rules, "//mnt/proj/a"), Ok("P:\\a".to_string()));
    assert_eq!(
        unmap_posix(&rules, "///mnt/proj/a"),
        Ok("P:\\a".to_string())
    );
}

#[test]
fn posix_root_destination_round_trips() {
    // Forward mapping onto a root destination gives `//a/b` (it appends a
    // separator after one), which must compare equal to the reported `/a/b`.
    let rules = [windows("P:\\", "/")];
    assert_eq!(unmap_posix(&rules, "/a/b"), Ok("P:\\a\\b".to_string()));
    assert_eq!(unmap_posix(&rules, "/"), Ok("P:\\".to_string()));
    let rules = [posix("/proj", "/")];
    assert_eq!(unmap_posix(&rules, "/a"), Ok("/proj/a".to_string()));
}

#[test]
fn windows_root_destinations_round_trip() {
    let rules = [posix("/proj", "C:\\")];
    assert_eq!(
        unmap_windows(&rules, "C:\\a\\b"),
        Ok("/proj/a/b".to_string())
    );
    let rules = [posix("/proj", "\\\\srv\\share\\")];
    assert_eq!(
        unmap_windows(&rules, "\\\\SRV\\share\\a"),
        Ok("/proj/a".to_string())
    );
}

#[test]
fn posix_round_trip_compares_case_sensitively() {
    // The candidate `/s/f` maps forward through the first rule to `/M/c/f`,
    // which differs from the reported `/m/c/f` only in case.
    let rules = [posix("/s/f", "/M/c/f"), posix("/s", "/m/c")];
    assert_eq!(unmap_posix(&rules, "/m/c/f"), Err(UnmapError::NoRoundTrip));
}

#[test]
fn posix_destination_matching_is_case_sensitive() {
    // Only destination matching decides: `/M/Q` must not match `/m/q/f`, or
    // its candidate `S3://B/f` would be tried first (equal length, listed
    // first), and it would pass the round trip, because the second rule (its
    // scheme and authority compare case-insensitively) maps it forward to
    // `/m/q/f`.
    let rules = [
        rule(PathFormat::Uri, "S3://B/", "/M/Q"),
        rule(PathFormat::Uri, "s3://b", "/m/q"),
    ];
    assert_eq!(unmap_posix(&rules, "/m/q/f"), Ok("s3://b/f".to_string()));
}

#[test]
fn posix_worker_splits_components_on_slash_only() {
    // Under POSIX `proj\x` is one directory name, so `/mnt/proj\x/f` is not
    // under `/mnt/proj`.
    let rules = [posix("/proj", "/mnt/proj")];
    assert_eq!(
        unmap_posix(&rules, "/mnt/proj\\x/f"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn round_trip_compares_anchors() {
    // The candidate `s3://bk/a` matches no rule (forward `apply_uri` does not
    // match under a source ending in `/`), so it maps forward to itself. Its
    // parts equal the reported path's, but its anchor (none) does not (`/`).
    let rules = [rule(PathFormat::Uri, "s3://bk/", "/s3:/bk")];
    assert_eq!(
        unmap_posix(&rules, "/s3:/bk/a"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn windows_case_folding_is_ascii_only() {
    // Documented under "Rejected but valid spellings".
    let rules = [posix("/p", "C:\\Proj\\\u{c4}")];
    assert_eq!(
        unmap_windows(&rules, "c:\\proj\\\u{e4}\\f"),
        Err(UnmapError::NoRoundTrip)
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
    // Both the share and the server must match.
    assert_eq!(
        unmap_windows(&rules, "\\\\fs\\other\\proj\\a"),
        Err(UnmapError::NoRoundTrip)
    );
    assert_eq!(
        unmap_windows(&rules, "\\\\other\\Share\\proj\\a"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn windows_worker_dotdot_stays_at_drive_root() {
    let rules = [posix("/proj", "C:\\proj")];
    assert_eq!(
        unmap_windows(&rules, "C:\\..\\proj\\a"),
        Ok("/proj/a".to_string())
    );
}

#[test]
fn posix_relative_destination_never_matches() {
    // Forward mapping `/a/f` gives `dst/f`, not the reported `/dst/f`.
    let rules = [posix("/a", "dst")];
    assert_eq!(unmap_posix(&rules, "/dst/f"), Err(UnmapError::NoRoundTrip));
}

#[test]
fn windows_root_relative_destination_never_matches_drive_path() {
    // `\proj` has a root but no drive; its anchor `\` differs from `C:\`.
    let rules = [posix("/p", "\\proj")];
    assert_eq!(
        unmap_windows(&rules, "C:\\proj\\a"),
        Err(UnmapError::NoRoundTrip)
    );
}

#[test]
fn windows_drive_relative_destination_never_matches() {
    // `C:proj` is drive-relative; its anchor `C:` differs from `C:\`.
    let rules = [posix("/proj", "C:proj")];
    assert_eq!(
        unmap_windows(&rules, "C:\\proj\\a"),
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

#[test]
fn uri_worker_format_is_treated_as_posix() {
    let rules = [posix("/a", "/M")];
    assert_eq!(
        unapply_rules(&rules, "/m/f", PathFormat::Uri),
        Err(UnmapError::NoRoundTrip)
    );
    assert_eq!(
        unapply_rules(&rules, "/M/f", PathFormat::Uri),
        Ok("/a/f".to_string())
    );
}

#[test]
fn posix_source_ending_in_backslash_gets_a_separator() {
    // Under POSIX `\` is not a separator, so `/src\` needs a `/` appended.
    let rules = [posix("/src\\", "/dst")];
    assert_eq!(unmap_posix(&rules, "/dst/f"), Ok("/src\\/f".to_string()));
}

#[test]
fn windows_source_ending_in_forward_slash_gets_no_extra_separator() {
    // `P:/` already ends with a Windows separator, so none is added.
    let rules = [windows("P:/", "/mnt/p")];
    assert_eq!(unmap_posix(&rules, "/mnt/p/a"), Ok("P:/a".to_string()));
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
fn destination_length_is_counted_in_normalized_components() {
    // `/d/./b/` is longer as a string than `/d/b` but has the same components,
    // so the listed order decides.
    let x = posix("/x", "/d/b");
    let y = posix("/y", "/d/./b/");
    assert_eq!(
        unmap_posix(&[x.clone(), y.clone()], "/d/b/f"),
        Ok("/x/f".to_string())
    );
    assert_eq!(unmap_posix(&[y, x], "/d/b/f"), Ok("/y/f".to_string()));
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
fn listed_order_breaks_ties_among_many_candidates() {
    // Enough candidates, with mixed destination depths, that an unstable sort
    // reorders equal lengths. No source nests in another, so their order
    // cannot change forward mapping.
    let depths = ["/dst", "/dst/f", "/dst/f/g"];
    let rules: Vec<PathMappingRule> = (0..300)
        .map(|i| posix(&format!("/s{i}"), depths[i % 3]))
        .collect();
    assert_eq!(unmap_posix(&rules, "/dst/f/g/h"), Ok("/s2/h".to_string()));
}

#[test]
fn session_order_matters_for_equal_length_sources() {
    // The round trip reproduces forward mapping with the rules as given. In
    // this order `P:/a\f` maps forward to `/x/f`, so `/y/f` has no stored
    // form; swapping the equal-length sources would accept it wrongly, which
    // is why `rules` must be in the Session's exact order.
    let rules = [windows("P:\\a", "/x"), windows("P:/a", "/y")];
    assert_eq!(unmap_posix(&rules, "/y/f"), Err(UnmapError::NoRoundTrip));
    assert_eq!(unmap_posix(&rules, "/x/f"), Ok("P:\\a\\f".to_string()));
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

#[test]
fn forward_result_deeper_than_reported_is_rejected() {
    // The only candidate is `/a`, which maps forward to `/m/z`, a descendant
    // of the reported `/m`.
    let rules = [posix("/a", "/m/z"), posix("/a", "/m")];
    assert_eq!(unmap_posix(&rules, "/m"), Err(UnmapError::NoRoundTrip));
}

#[test]
fn forward_result_shallower_than_reported_is_rejected() {
    // The first candidate `/a/f` maps forward to `/m`, an ancestor of the
    // reported `/m/x/f`, so the second candidate is the answer.
    let rules = [posix("/a/f", "/m"), posix("/a", "/m/x")];
    assert_eq!(unmap_posix(&rules, "/m/x/f"), Ok("/a/f/x/f".to_string()));
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

// === unapply_rules_with_source_format ===

#[test]
fn source_format_of_a_uri_is_uri() {
    let rules = [posix("/proj", "/mnt/proj")];
    assert_eq!(
        unapply_rules_with_source_format(&rules, "s3://b/k", PathFormat::Posix),
        Ok(("s3://b/k".to_string(), PathFormat::Uri))
    );
    assert_eq!(
        unapply_rules_with_source_format(&[], "s3://b/k", PathFormat::Windows),
        Ok(("s3://b/k".to_string(), PathFormat::Uri))
    );
}

#[test]
fn source_format_without_rules_is_the_worker_format() {
    assert_eq!(
        unapply_rules_with_source_format(&[], "C:\\a\\b", PathFormat::Windows),
        Ok(("C:\\a\\b".to_string(), PathFormat::Windows))
    );
    assert_eq!(
        unapply_rules_with_source_format(&[], "/a/b", PathFormat::Posix),
        Ok(("/a/b".to_string(), PathFormat::Posix))
    );
}

#[test]
fn source_format_is_the_returned_candidate_rules_format() {
    // Two rules match; the longer destination's candidate (a URI rule) is
    // returned, so its format is reported, not the other rule's.
    let rules = [
        rule(PathFormat::Uri, "s3://bucket/assets", "/mnt/p/assets"),
        windows("P:\\", "/mnt/p"),
    ];
    assert_eq!(
        unapply_rules_with_source_format(&rules, "/mnt/p/assets/a.exr", PathFormat::Posix),
        Ok(("s3://bucket/assets/a.exr".to_string(), PathFormat::Uri))
    );
    assert_eq!(
        unapply_rules_with_source_format(&rules, "/mnt/p/shot/a.exr", PathFormat::Posix),
        Ok(("P:\\shot\\a.exr".to_string(), PathFormat::Windows))
    );
}

#[test]
fn source_format_errors_match_unapply_rules() {
    let rules = [posix("/proj", "/mnt/proj")];
    assert_eq!(
        unapply_rules_with_source_format(&rules, "rel/x", PathFormat::Posix),
        Err(UnmapError::Relative)
    );
    assert_eq!(
        unapply_rules_with_source_format(&rules, "/tmp/x", PathFormat::Posix),
        Err(UnmapError::NoRoundTrip)
    );
}
