# Path Mapping

## Overview

Path mapping transforms filesystem paths between different formats and locations. This
is essential for render farms where jobs are authored on one OS but executed on another,
or where storage mount points differ between submission and execution hosts.

Defined in `path_mapping.rs` (PathFormat, PathMappingRule, PathMapper,
InvalidPathMappingRule, and the reverse mapping
`unapply_rules`/`unapply_rules_with_source_format`/`UnmapError`),
`windows_upcase.rs` (the Windows uppercase table) and `uri_path.rs` (URI-aware
path operations).

## PathFormat

```rust
pub enum PathFormat {
    Posix,    // Forward slashes, case-sensitive
    Windows,  // `\` and `/` separators (`/` normalized to `\`), case-insensitive matching
    Uri,      // scheme://authority/path — no normalization
}
```

`PathFormat::host()` returns the format for the current platform (`Posix` on Linux/macOS,
`Windows` on Windows).

The evaluator carries a `PathFormat` that controls how path values are normalized. PATH
values carry their own format and are validated against the evaluator's format. URI paths
bypass normalization entirely.

## PathMappingRule

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathMappingRule {
    pub source_path_format: PathFormat,
    pub source_path: String,
    pub destination_path: String,
}
```

A rule maps paths under `source_path` to the same relative location under
`destination_path`. Rules are applied through a [`PathMapper`](#pathmapper).

### Source paths must be absolute

`source_path` must be absolute for `source_path_format`, and
`rule.validate()` (or `PathMapper::new`) rejects one that is not:

| Format | Absolute means | Rejected examples |
|--------|----------------|-------------------|
| `POSIX` | starts with `/` | `home/user`, `` (empty) |
| `WINDOWS` | starts with a drive and separator (`C:\`, `c:/`) or a UNC share (`\\server\share`, either separator) | `\projects` (rooted on the current drive), `C:projects` (drive-relative), `\\server` (no share) |
| `URI` | starts with `<scheme>://`, where the scheme is at least two characters (see [`is_uri`](#scheme-must-be-at-least-two-characters)) | `bucket/prefix`, `C://data` (a Windows path) |

A relative source path could only match relative input paths, and what a
relative path refers to depends on a working directory the rule cannot know.
Requiring absolute source paths removes that ambiguity. Input paths are not
validated: a path that is not absolute for a format simply matches none of
that format's rules.

The error is `InvalidPathMappingRule { index, reason }`. Its `Display` is
`path mapping rule <index>: source_path '<path>' must be <what>` from
`PathMapper::new`, and the same without the `path mapping rule <index>: `
prefix from `validate`, which sets `index` to `None`.

### Path length limit

`path_mapping::MAX_PATH_LEN` is 32 KiB (32,768 bytes). `validate` and
`PathMapper::new` reject a rule whose `source_path` or `destination_path` is
longer, with `source_path is <n> bytes long, more than the limit of 32768
bytes` (or `destination_path ...`). An input path longer than the limit
matches no rule, and `apply_path_mapping` raises `apply_path_mapping: the
path is <n> bytes long, more than the limit of 32768 bytes` for one.

32 KiB covers a Windows extended-length path (up to 32,767 UTF-16 code units)
of ASCII characters and is eight times Linux's `PATH_MAX` (4096). It bounds
the work for one path, and the trie size limit below bounds the rules as a
whole.

### Trie size limit

Any number of rules is allowed. What is limited is the estimated size of the
trie, `PathMapper::trie_bytes()`: each node counts its key's length plus 224
bytes for the node and its entry in the parent's map (measured at about 230
bytes per one-character component; the estimate is within about 15% for
chains of single-child nodes and above the real size for wide trees). A rule
adds only the components it does not share with earlier rules, so rules
under a common root, as real rule sets are, cost little.

`PathMapper::new` limits the estimate to
`path_mapping::DEFAULT_MAX_TRIE_BYTES` (30 MiB);
`PathMapper::with_max_trie_bytes(rules, max)` takes the limit. When a rule
takes the estimate past the limit, it is rejected with
`path mapping rule <i>: the rules' trie would need about <n> bytes, more than
the limit of <max> bytes`. Without the limit, a 10 MB rules file (which the
CLI accepts) of rules near 32 KiB that share nothing would build a trie of
about 1.2 GB. `openjd-sessions` takes the limit from
`SessionLimits::max_path_mapping_trie_bytes`.

### Reverse mapping

`unapply_rules(mapper, path, worker_format) -> Result<String, UnmapError>` maps a
path reported on a worker back to the submission path context, the form in which
`PATH` job parameters are given. The CONTROL_FLOW extension stores `PATH` output
values in that form (RFC 0012 §Path values, RFC 0012 Design Choice 5). The RFC states the
requirement on the outcome: the stored value must map forward, with the producing
Session's rules, to the reported path. `unapply_rules` implements the RFC's
reference inverse:

1. A URI (`is_uri`) is returned unchanged, with or without rules.
2. Any other path must be absolute under `worker_format`, or the result is
   `UnmapError::Relative`. This holds with no rules too: a relative path would be
   relative to a Session Working Directory no consumer can see.
3. With no rules, the absolute path is returned unchanged, not normalized.
4. Otherwise the path and each rule's `destination_path` are lexically normalized
   and compared under `worker_format`'s rules. A rule matches when its
   `destination_path` is the path or a prefix of it ending on a component
   boundary. Its candidate is the remaining components joined with the separator
   of its `source_path_format` (`\` for Windows, `/` for POSIX and URI), appended to
   `source_path` with one more separator unless `source_path` already ends with
   one, or `source_path` itself when no components remain. Components keep the
   spelling they have in the reported path, and are appended to a URI unchanged.
5. Candidates are tried by decreasing `destination_path` length, counted in
   normalized components, then in the order of `mapper.rules()`. The first candidate
   that `mapper.map_with_format(candidate, worker_format)` maps to a path equal to
   the reported one is returned. If none does, the result is
   `UnmapError::NoRoundTrip`. The common case is a path under the Session Working
   Directory, which no rule's `destination_path` covers.

The round trip is what makes the result correct, and it is needed because rules can
nest: with `P:\` → `/mnt/p` and `P:\x` → `/data/x`, the candidate for `/mnt/p/x/f`
is `P:\x\f`, which maps forward to `/data/x/f`, so the result is `NoRoundTrip`.
The round trip forward-maps with the `mapper` it is given, so `mapper` must be the
producing Session's own `PathMapper` (`openjd-sessions` passes the one it maps
`PATH` parameters with). A mapper built from other rules reproduces the wrong forward
mapping and can accept a value the Session maps elsewhere. Forward precedence is the
mapper's (see [Which rule wins](#which-rule-wins)), so the result does not depend on
the order of the rules except where the mapper's own precedence does: of the rules
`P:\a` → `/x` and `P:/a` → `/y`, which name the same location, the mapper keeps the
later one, so `/y/f` gives `P:/a\f` and `/x/f` is `NoRoundTrip`. Among candidates
from destinations of the same length, `mapper.rules()` order breaks ties; that is the
RFC's "the order the rules are listed".

**Lexical normalization** removes `.` and empty components (runs of separators,
trailing separators) and applies `..` to the preceding component, without resolving
symbolic links. A `..` directly below a root is dropped, as `/..` is `/`. The anchor
is kept separately and is never stripped, so a root keeps its separator. The POSIX
anchor is `/`, however many leading slashes there are. POSIX lets exactly two be an
implementation-defined root, and `path()` preserves that spelling, but here it is
folded into `/`, as forward matching treats `//a` as `/a` (see
[Divergence from Python](#divergence-from-python)). Windows anchors are `X:\` (either separator),
`\\server\share`, and, for non-absolute `destination_path` values only, `X:` and `\`.
A UNC anchor's server ends at the first separator, so `\\srv\\share` has an empty
share, as Python 3.12's `PureWindowsPath` parses it. Normalization is applied to both sides of each
comparison. It matters on the forward side because forward mapping appends the
remaining components to `destination_path` as written and keeps a trailing
separator, so `destination_path` `/mnt/./p` gives `/mnt/./p/x`.

**Comparison** under `worker_format`: Windows paths compare case-insensitively
with ASCII case folding, with `\` and `/` both separators;
POSIX paths compare case-sensitively with `/` only. `PathFormat::Uri` as the worker
format is treated as POSIX.

**Absoluteness** agrees with `PurePosixPath`/`PureWindowsPath.is_absolute()` on
ordinary forms: a leading `/` on POSIX; a drive with a root (`C:\`, `C:/`) or a UNC path with a non-empty
server and share (`\\server\share`) on Windows. A Windows path rooted without a
drive (`\x`), drive-relative (`C:x`), with a non-letter drive (`1:\x`), or a UNC
path with an empty server or share (`\\\share\x`, `\\srv\\share`) is `Relative`, and
so is `\\srv\share` on POSIX, where `\` is an ordinary character. The same Windows
anchor parser serves absoluteness and normalization, so the two agree. Python
versions disagree on the edge forms, so this follows neither exactly: Python 3.9
treats `\\srv\` (a UNC server with an empty share) as absolute, and Python 3.12
treats `1:\x`, `\\\share\x`, and `\\srv\\share` as absolute; here all four are
`Relative`, as none names a usable Windows location. Forward matching has its own
test, for an absolute rule `source_path` (see
[Source paths must be absolute](#source-paths-must-be-absolute)).

**A candidate no rule matches.** Forward mapping returns a path no rule matches
unchanged, so such a candidate passes the round trip exactly when it equals the
reported path. That follows the RFC, whose requirement is on the forward mapping's
result. The plan (`docs/control-flow-prototype-plan.md` §E1) asked for a forward
helper that reports whether a rule matched; the RFC wins where they disagree. Every
candidate is a rule's absolute `source_path` (a `PathMapper` rejects any other) or a
path under it, so forward mapping finds a rule for it unless the candidate is longer
than `MAX_PATH_LEN`, which matches no rule and so fails the round trip unless it
equals the reported path.

**Source format filtering is not implemented.** The RFC says a runtime that knows the
submission path format SHOULD use only rules whose `source_path_format` is that
format or `URI`. `unapply_rules` takes no submission format, since no openjd runtime
knows it (a Session has its rules but not the Job's submission path format), and a
caller MUST NOT
build `mapper` from filtered rules: the RFC defines the no-rules case "before any filtering by
source format" and the round trip on the Session's own rules, so a list filtered to
empty would wrongly return a worker-only path unchanged. Without the filter a
candidate may be in another format than the submission's; the round trip still
guarantees it maps forward to the reported path.

**Rejected but valid spellings.** Normalization is lexical, as the RFC defines it, so
some paths that name a location under a destination are still `NoRoundTrip`: a
Windows `\\?\C:\...` extended-length path against a `C:\...` destination, and a
non-ASCII component that differs only in case (folding here is ASCII-only, where
forward matching uses the Windows uppercase table). Nothing is accepted wrongly; the
round trip prevents that.

`unapply_rules_with_source_format` takes the same arguments and returns the stored
form together with its path format: `PathFormat::Uri` for a URI returned unchanged,
`worker_format` for an absolute path returned unchanged because there are no rules,
and otherwise the `source_path_format` of the rule whose candidate was returned.
`unapply_rules` is this function without the format. A caller that stores the result
as an `ExprValue::Path`, as the CONTROL_FLOW output collector does
(`specs/model/outputs.md`), uses the format so the value carries the format of the
submission path context it is in (RFC 0012 §Path values).

`UnmapError` implements `Display` and `std::error::Error`. The caller, which knows
the path and the output's name, builds the user-facing message around it.

### Serde

`PathMappingRule` implements `Serialize` and `Deserialize` for JSON interchange:

```json
{
    "source_path_format": "WINDOWS",
    "source_path": "C:/projects",
    "destination_path": "/mnt/projects"
}
```

Deserializing does not validate `source_path`; `PathMapper::new` does.

## PathMapper

```rust
#[derive(Debug, Clone)]
pub struct PathMapper { /* rules, precedence order, one trie per format */ }

impl Default for PathMapper; // no rules

impl PathMapper {
    pub fn new(rules: Vec<PathMappingRule>) -> Result<Self, InvalidPathMappingRule>;
    pub fn with_max_trie_bytes(
        rules: Vec<PathMappingRule>,
        max_trie_bytes: usize,
    ) -> Result<Self, InvalidPathMappingRule>;
    pub fn trie_bytes(&self) -> usize;
    pub fn rules(&self) -> &[PathMappingRule];
    pub fn rules_by_precedence(&self) -> impl Iterator<Item = &PathMappingRule> + '_;
    pub fn is_empty(&self) -> bool;
    pub fn apply(&self, path: &str) -> Option<String>;
    pub fn apply_with_format(&self, path: &str, output_format: PathFormat) -> Option<String>;
    pub fn map(&self, path: &str) -> String;
    pub fn map_with_format(&self, path: &str, output_format: PathFormat) -> String;
}
```

`PathMapper::new` validates every rule and builds one trie per source format,
keyed by path component. A rule's source path is stored at the node reached
by its components. To map a path, the mapper splits it once per source format
and walks its components down that format's trie, remembering the deepest
node that holds a rule. The work is linear in the length of the path,
whatever the number of rules. `apply_path_mapping` charges `path.len()`
string operations for the match and, when a rule applies, the length of the
result for building it (see [function-library.md](function-library.md), which
describes `count_string_ops`). This port follows the trie in
[deadline-cloud-job-attachments `_PathMappingRuleApplier`](https://github.com/aws-deadline/deadline-cloud-job-attachments/blob/2ae3635c6f5642846e9a98cdf1075f867886133a/src/deadline/job_attachments/_path_mapping.py),
extended to URI rules and to rules of several formats at once. The tries
are stored as one flat array of nodes that refer to each other by index, so
building, dropping, cloning or formatting a mapper never recurses, however
many components a rule has.

`apply` and `apply_with_format` return `None` when no rule matches; `map`
and `map_with_format` return the path unchanged instead. `apply` and `map`
join components with the host separator; the `_with_format` forms take the
output format (`\` for `Windows`, `/` for `Posix` and `Uri`).

### Components

How a path is split depends on the source format of the trie being searched:

- **POSIX**: the anchor is `/`, and components are split on `/` only. A
  backslash is an ordinary filename character, so `home\user` is one
  component: the rule `/home/user` does not match `/home\user/f`, and the
  rule `/home\user` matches `/home\user/f` but not `/home/user/f`. This
  follows `PurePosixPath("/home\\user/f").parts == ('/', 'home\\user', 'f')`.
  Components compare exactly.
- **WINDOWS**: `\` and `/` are interchangeable separators. The anchor is a
  drive (`C:`) or a UNC share (`\\server\share`), and it is one component, so
  `\\srv\share` never matches `\srv\share\x` (a path on the current drive) or
  `\\srv\other\x`. Components, including the drive letter, server and share,
  compare the way Windows compares file names: each UTF-16 code unit is
  mapped through the Windows uppercase table (see [Windows case](#windows-case)).
- **URI**: the anchor is `scheme://authority`, compared ASCII
  case-insensitively (RFC 3986). Path segments are split on `/` and compare
  exactly. Empty segments are kept, because URI paths are
  [opaque](#why-uri-paths-are-opaque): `s3://b/a//f` has the segments `a`,
  ``, `f`, and the rule `s3://b/a/` (segments `a`, ``) matches `s3://b/a/` and
  `s3://b/a//f` but not `s3://b/a/f`.

For POSIX and WINDOWS, empty components (from repeated or trailing
separators) and `.` are dropped, as `pathlib` does, so `/a/./b`, `/a//b` and
`/a/b/` all name `/a/b`. `..` is a component and is not collapsed, because
doing so safely needs the filesystem (symbolic links).

A path is matched against every format that has rules. `//srv/share/x` is
both a POSIX path and a Windows UNC path, and can match rules of both
formats.

### Which rule wins

The matching rule whose source path has the most names wins. The anchor
counts as no names for the POSIX root `/`, one for a drive (`C:`) or a URI's
`scheme://authority`, and two for a UNC share (its server and share); every
component after the anchor counts as one. Rules whose source paths have the
same components name the same location (`/mnt/p`, `/mnt/p/` and `/mnt//p`
are one location; so are `C:\Data` and `c:/data`), and the rule given last
wins.

How an anchor counts matters only when a path matches rules of more than one
format. Within one format it adds the same amount to every rule that can
match a path (a path cannot match both a drive rule and a UNC rule). Across
formats, the only overlap is a path like `//srv/share/x`, which is a POSIX
path and a UNC path. `\\srv\share` beats `/srv`, and `\\srv\share\x` beats
`/srv/share`. The POSIX rule `/srv/share` and the WINDOWS rule `\\srv\share`
both have two names; between rules of different formats with the same number
of names, the WINDOWS rule wins, because `//srv/share/...` is the UNC
spelling. This tie is not broken by list order, because the two rules are
different locations, so reordering the rules (as a reader of
`rules_by_precedence()` may) must not change the result.

This replaces the earlier order, "decreasing length of the source path" as a
string (How-Jobs-Are-Run, "Applying Path Mapping Rules"). For rules spelled
without redundant separators or `.` components, a rule with more components
that matches the same path is also the longer string, so the two orders pick
the same rule. They differ for redundant spellings: string length prefers
`/aaaa///` over `/aaaa/b`, and between `/mnt/p` and `/mnt/p/` it prefers the
longer spelling instead of the rule given last.

`rules()` returns the rules in the order given. `rules_by_precedence()`
returns the rules that can apply: the most names first, then WINDOWS before
POSIX before URI, then the one given last first. A rule replaced by a later
rule for the same location is left out, so each location appears once.
Applying the first rule in that order that matches a path gives the same
result as `apply`. Building a new mapper from that list, in that order or any
other, also gives the same result: the list has no two rules for one
location, and the remaining ties are broken by format, not by order. A
consumer that reads the rules (such as a job script reading
`Session.PathMappingRulesFile`) therefore picks the same rule as the mapper
whether it takes the first match or follows these rules, as long as it splits
and compares paths the same way (see
[Divergence from Python](#divergence-from-python) for where pathlib differs).

### Output

The result is `destination_path`, as written, followed by the path's
components after the matched source path, each preceded by the output
separator. When `destination_path` already ends with a separator (`\` or `/`
for WINDOWS output, `/` otherwise), the first component reuses it, so
`/local/` and `/mnt/a` give `/local/a`, not `/local//a`. Components keep the
case they have in the input. A trailing separator on the input (`/` for
POSIX and URI, `\` or `/` for WINDOWS) is kept, unless the result already
ends with a separator.

### Windows case

Windows compares file names by mapping each UTF-16 code unit through its
uppercase table (`RtlUpcaseUnicodeChar`; NTFS keeps a copy in `$UpCase`, and
`CompareStringOrdinal` with `bIgnoreCase` uses the same mapping). The table
is generated into `windows_upcase.rs` by
`scripts/generate_windows_upcase_table.py`, which reads it from a running
Windows system and checks every entry against `CompareStringOrdinal`; a
Windows-only unit test re-checks the checked-in table the same way. The
mapping applies on every host, so a WINDOWS rule matches the same paths on a
Linux worker as on a Windows one.

The table is not a Unicode case folding:

- It maps one code unit to one code unit, so `ß` does not match `SS` or `ẞ`.
- It maps a letter only to an uppercase or titlecase letter that maps back to
  it, so the
  dotless `ı` does not match `I`, the long `ſ` does not match `S`, final `ς`
  does not match `Σ`, and no non-ASCII character matches an ASCII one (the
  KELVIN SIGN `K` does not match `k`).
- It predates several Unicode case pairs, such as Cherokee lowercase and
  Georgian Mtavruli, which do not fold.
- It leaves surrogates alone, so characters outside the BMP never fold.

Python's `PureWindowsPath` compares with `str.lower()`, which folds more
pairs than Windows does (including KELVIN SIGN to `k`), and
openjd-sessions-for-python compares Windows components with an ASCII-only
fold. See [Divergence from Python](#divergence-from-python).

## URI Path Operations

`uri_path.rs` provides URI-aware path manipulation for paths with `scheme://authority/path`
structure. These are used when `PathFormat::Uri` is detected.

All functions live in the `uri_path` module — callers use `uri_path::name(p)`,
`uri_path::parent(p)`, etc. No `uri_`-prefixed names exist at the function level
because the module already provides that namespace.

| Function | Purpose |
|----------|---------|
| `is_uri(s)` | Check if string has a `scheme://` prefix matching `^[a-zA-Z][a-zA-Z0-9+.-]+://` (scheme ≥ 2 chars) |
| `parse(s) -> Option<UriParts>` | Parse into `{ authority, path_parts }` or `None` |
| `name(s)` | Last component (like `Path.name`) |
| `parent(s)` | Parent URI (like `Path.parent`) |
| `suffix(s)` | File extension |
| `suffixes(s)` | All extensions (e.g., `.tar.gz` → `[".tar", ".gz"]`) |
| `stem(s)` | Filename without extension |
| `parts(s)` | Split into components: `[authority, seg1, seg2, ...]` |
| `join(s, child)` | Append a child component to a URI path. One trailing slash is dropped first, and a bare authority gives `scheme://host/child`. Interior empty segments are kept |
| `from_parts(parts)` | Reconstruct URI from `parts(...)` output |

### Scheme must be at least two characters

`is_uri` and `parse` require the scheme to match `^[a-zA-Z][a-zA-Z0-9+.-]+://`.
RFC 3986 permits a one-letter scheme, but `C://scenes/a.ma` is also a valid Windows
absolute path (a drive letter with a doubled separator), and users produce that
spelling by hand-editing or pasting from tools that emit forward slashes. Treating it
as a URI with scheme `c` would silently bypass path mapping and relative-path
resolution for a real local file. Single-letter schemes are essentially unused in
practice, so such values are handled as filesystem paths. This matches the
Expression Language spec §1.2.1 and RFC 0006.

### Why URI paths are opaque

URI paths are not normalized: consecutive slashes, `.`, and `..` are preserved
verbatim. The rationale is threefold:

- **Round-trip fidelity.** URIs can encode application-specific meaning in slash
  patterns (S3 prefixes, HTTP path parameters); folding `//` into `/` would silently
  rewrite a user-supplied key.
- **Authority preservation.** The `scheme://host[:port]` portion is a single opaque
  unit that must survive every operation. URI parsing splits it once up front
  (`parse`) and never touches it again.
- **Percent-encoded segments.** Segments may contain `%2F` which decodes to `/` but
  must not be treated as a separator. Rather than try to teach the path functions
  about URI encoding, we leave every byte of the path exactly as the user supplied
  it.

## Expression Language Integration

Path-related operations in the expression language:

### Path Properties (via `__property_NAME__`)

| Property | Type | Description |
|----------|------|-------------|
| `.name` | string | Last component of the path |
| `.stem` | string | Last component without extension |
| `.suffix` | string | File extension (including dot) |
| `.suffixes` | list[string] | All extensions |
| `.parent` | path | Parent directory |
| `.parts` | list[string] | All path components |

### Path Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `with_name(name)` | `(path, string) -> path` | Replace last component |
| `with_stem(stem)` | `(path, string) -> path` | Replace stem, keep extension |
| `with_suffix(suffix)` | `(path, string) -> path` | Replace extension |
| `with_number(n)` | `(path, int) -> path` / `(string, int) -> string` | Put frame number `n` into the stem (see below) |
| `as_posix()` | `(path) -> string` | Convert to POSIX string |
| `is_absolute()` | `(path) -> bool` | Check if path is absolute |
| `is_relative_to(other)` | `(path, path) -> bool` | Check prefix relationship |
| `relative_to(other)` | `(path, path) -> path` | Compute relative path |

`with_number(n)` rewrites the stem (the extension is kept) by the first
rule that matches:

1. The stem ends in `%d`: replace it with `n`.
2. The stem ends in `%0Nd`: replace it with `n` padded to width `N`.
3. The stem contains `#`: keep the stem up to the last run of `#` and
   replace that run with `n` padded to the run's length. Any text after the
   run is dropped (`f_####_v2` becomes `f_0007`); this is a known bug.
4. The stem ends in ASCII digits: replace them with `n` padded to their
   count.
5. Otherwise append `_` and `n` padded to width 4.

Padding adds leading zeros; a negative `n` keeps the width with its `-`
counted (`-7` at width 4 is `-007`), and an `n` wider than the width is
written in full. A `%0Nd` or `#` width above 32 raises `ExpressionError`;
a trailing digit run of any length is replaced at its own width.

> **URI paths in path methods.** The path properties and the
> `with_*` methods all detect URI inputs (`uri_path::is_uri`) and
> route to `uri_path::*` so the result preserves URI grammar —
> `/` separator, opaque authority, no normalization — regardless
> of the evaluator's `PathFormat`. A URI value passed through any
> of these methods stays a URI; it does not pick up the host's
> backslashes when the evaluator is in `PathFormat::Windows`. See
> the conformance test
> `2023-09/EXPR/jobs/expr2.3.2--uri-path-ops.test.yaml` for the
> end-to-end pin.

> **Empty-name guard.** `with_name`, `with_stem`, `with_suffix`,
> and `with_number` raise `ExpressionError` with the message
> `with_<op>: '<path>' has an empty name` when the input has no
> final component to operate on. That covers filesystem roots
> (`/` on POSIX, `C:\` and `\\server\share` on Windows) and bare
> URI authorities (`s3://bucket`, `s3://bucket/`). Without this
> guard the operators silently invent a filename against an
> empty stem and emit nonsensical results
> (`path("/").with_name("x")` → `"//x"`,
> `path("s3://bucket").with_suffix(".png")` → `"s3://bucket/.png"`).
> Matches Python pathlib's behaviour:
> `PurePosixPath('/').with_name('x')` raises
> `ValueError: PurePosixPath('/') has an empty name`.

> **Replacement-argument validation.** `with_name` and
> `with_stem` raise with `Invalid name '<arg>'` if the supplied
> name is empty, equal to `.`, or contains a path separator
> (`/` always; `\` on Windows). `..` is *accepted* (matches
> pathlib — it's a filename that happens to look like the
> parent-dir indicator). `with_suffix` raises with
> `Invalid suffix '<arg>'` unless the suffix is empty (which
> strips the existing suffix) or starts with `.` and is not
> just `.` and contains no separator. Multi-dot suffixes like
> `.tar.gz` are accepted. `with_stem('')` raises one of two
> messages depending on whether the path has a suffix:
> `Invalid name ''` if there's no suffix to keep, or
> `'<path>' has a non-empty suffix` if there is — both align
> with pathlib's diagnostics.

> **Relative paths stay relative.** `with_name`, `with_stem`,
> `with_suffix`, and `with_number` preserve the input's
> absoluteness. A single-component relative path
> (`path("foo").with_name("x")`) produces `"x"`, not `"/x"`,
> matching pathlib. Filesystem anchors (`/`, `C:\`, UNC
> `\\server\share\`) are preserved without doubling the
> separator (`path("/foo").with_name("x")` → `"/x"`,
> `path("C:\\foo").with_name("x")` → `"C:\\x"`).

> **Filesystem path normalization on construction.** The
> `path()` constructor canonicalizes filesystem inputs the
> same way Python pathlib does on construction:
>
> - leading `./` is stripped (`path("./foo")` → `"foo"`)
> - interior `/./` segments are stripped (`path("a/./b")` → `"a/b"`)
> - trailing separators are stripped (`path("a/b/")` → `"a/b"`)
> - runs of separators are collapsed (`path("a//b")` → `"a/b"`)
> - 3+ leading slashes collapse to 1 (`path("///foo")` → `"/foo"`)
> - exactly 2 leading POSIX slashes are preserved (`path("//foo")` → `"//foo"`)
>   per IEEE Std 1003.1-2017 §3.271 (POSIX double-slash root)
> - on Windows, all `/` become `\` (`path("foo/bar")` → `"foo\\bar"`)
> - UNC anchors always end with `\` (`path("\\\\srv\\share")` → `"\\\\srv\\share\\"`)
> - empty input becomes `"."` (`path("")` → `"."`)
> - `..` segments are NOT resolved (pathlib doesn't simplify
>   `..` because it can't safely do so without filesystem access
>   to detect symbolic links)
>
> URI inputs are explicitly NOT normalized — see the URI-paths
> note above. The OpenJD specification (wiki §1.2.1) preserves
> URI path components verbatim because schemes like S3 treat
> `a//b` and `a/b` as distinct keys.

> **Drive-relative path disambiguation on Windows.** When
> `with_name` produces a name on a relative-path parent and the
> name itself begins with `<letter>:`
> (e.g., `path("foo").with_name("x:y")`), pathlib prepends `.\`
> to disambiguate from a drive-relative path: the result is
> `".\\x:y"`, not `"x:y"`. We follow the same convention. POSIX
> has no analogous ambiguity.

### Path Operators

| Operator | Signature | Description |
|----------|-----------|-------------|
| `/` | `(path, string) -> path` | Join path components |
| `/` | `(path, path) -> path` | Join paths |
| `+` | `(path, string) -> path` | Append to last component |

An empty operand is the empty path, as in pathlib, whether it is written `""`
(an empty PATH value) or `"."` (how `path("")` displays). An empty left operand
gives the right operand alone, so with `Param.P` empty `Param.P / "k"` and
`path("") / "k"` both give `k`, not `/k` or `./k`. An empty right operand gives
the left operand unchanged (`path("a") / ""` is `a`, and a URI stays as it is),
and two empty operands give `.`. `functions::path::join` and `non_uri_join`
apply this before any other rule.

On Windows filesystem paths, a root-relative right operand (for example,
`\renders`) replaces the left path below its root. A drive or UNC root from the
left operand is retained; if the left operand has no drive or UNC root, the right
operand replaces it entirely. The same absolute-right rule applies to URI left
operands, so `/renders` replaces the URI entirely under POSIX and URI formats.
Under Windows, a single leading `/` or `\` is root-relative rather than absolute;
for a URI left operand it replaces only the path below the authority. For example,
`path("s3://bucket/prefix") / "/renders"` produces `s3://bucket/renders` under
Windows and `/renders` under POSIX. A Windows drive-relative right operand replaces
a URI left operand entirely, so `path("s3://bucket/prefix") / "C:render.exr"`
produces `C:render.exr`.

### apply_path_mapping

`apply_path_mapping(path_string)` is a host-context-only function that applies
the `PathMapper` captured in its implementing closure to a string, returning a
path value. It is registered on a [`FunctionLibrary`](../../crates/openjd-expr/src/function_library.rs)
obtained from [`FunctionLibrary::for_profile`](../../crates/openjd-expr/src/default_library.rs)
with an [`ExprProfile`](../../crates/openjd-expr/src/profile.rs) whose
`host_context` is `HostContext::WithRules(Arc<PathMapper>)`, usually built
with the fallible `HostContext::with_rules(rules)` — see
[function-library.md § Host Context](function-library.md#host-context).
During template validation, when rules aren't available yet, callers build
the profile with `HostContext::Unresolved` instead, which registers a stub
that returns `Unresolved(path)` for type checking.

The function charges `path.len()` string operations before mapping, and the
result's length after a rule applies. The trie walk is linear in the path
length whatever the number of rules, and building the result is linear in its
length, so the charges grow with the whole cost. A charged operation covers 256
bytes, and matching costs more per byte than most string functions (each
WINDOWS component is folded before lookup), so the time per operation is
several times that of, say, `upper()`. The earlier rule-by-rule loop charged
only `path.len()` while its cost also grew with the rule count (openjd-rs#210).

## Divergence from Python

The Python openjd-sessions `PathMappingRule.apply` uses `PurePosixPath` /
`PureWindowsPath` and is applied rule by rule. The Rust `PathMapper` splits
paths itself and walks a trie. They agree on how input paths split into
components (`.` and empty components dropped, `..` kept, the POSIX backslash
rule, UNC anchors), with these differences:

- **Device paths.** Rust has no special case for the Windows device
  namespace: `\\?\C:\x` is a UNC path with server `?` and share `C:`, and
  `\\?\UNC\srv\share` is the share `\\?\UNC` followed by `srv` and
  `share`. `PureWindowsPath` treats both prefixes as part of the drive. Both
  keep such paths apart from `C:\x` and `\\srv\share`.

- **Windows case.** Rust uses the Windows uppercase table. Python
  openjd-sessions uses an ASCII-only fold, chosen to match the earlier Rust
  behavior. `PureWindowsPath` itself uses `str.lower()`. All three agree on
  ASCII.
- **Source paths.** Rust rejects a relative source path. Python accepts one,
  and it then matches only relative inputs.
- **Length.** Rust limits rule paths and input paths to 32 KiB
  ([Path length limit](#path-length-limit)); Python has no limit.
- **Two leading slashes.** `PurePosixPath('//a')` keeps `//` as its own root,
  so it is not relative to `/a`. Rust treats `//a` as `/a`, as Linux and
  macOS do. So in Rust, but not in Python, a POSIX rule can match a
  `//server/share/...` path, where it competes with WINDOWS UNC rules (see
  [Which rule wins](#which-rule-wins)).
- **Ties.** Between rules for the same location, Rust takes the rule given
  last. Python sorts rules by component count, and among rules for the same
  location takes the one given first.
- **Destination.** Rust appends the remaining components to
  `destination_path` as written, reusing a separator it ends with. Python
  normalizes the destination for every rule format, because it keeps it as a
  `PurePath`: for the destination `D:\x\\y`, Rust gives `D:\x\\y\f` and
  Python `D:\x\y\f`, and the same holds for a URI rule's destination. When
  the whole source path matches and the input has no trailing separator, Rust
  keeps a separator a non-root destination ends with and Python drops it:
  `/mnt` → `/local/` maps `/mnt` to `/local/` in Rust and `/local` in Python.
  Python also reads a remaining component that looks like a drive as one:
  with the rule `C:\a` → `D:\m`, `C:\a\b:c` gives `D:\m\b:c` in Rust and
  `b:c` in Python. These differences predate `PathMapper`.
- **URI rules.** Python accepts a one-letter scheme (`C://data`) for a URI
  rule; Rust rejects it, because `C://data` is a Windows path (see
  [is_uri](#scheme-must-be-at-least-two-characters)). For precedence, Python
  strips slashes from both ends of a URI path, so it counts `s3://b/a/` as
  two components; Rust keeps the empty segment and counts three. For a URI
  rule, Python joins `str(destination_path) + separator + rest`, and a
  `PurePath` keeps a trailing separator only on a root, so a root
  destination still gives a doubled one (`s3://b` → `/` maps `s3://b/a` to
  `//a`, and `s3://b` → `D:\` gives `D:\\a`), where Rust gives `/a` and
  `D:\a`; and a whole-source match without a trailing separator drops one a
  non-root destination ends with (`s3://b` → `/local/` maps `s3://b` to
  `/local`), where Rust keeps it.
  Matching for a single URI rule is the same.

The reverse mapping, `unapply_rules` and `unapply_rules_with_source_format`, has no
Python counterpart yet: it is part of the draft CONTROL_FLOW extension (RFC 0012 §Path values), which this crate prototypes first.
