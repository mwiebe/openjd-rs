// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `@` file values of the `openjd_output_var` directive (RFC 0010 §File
//! values): recognizing the `@` form of a directive line, and reading the
//! file it names.
//!
//! Compiled into both the cross-user helper, which reads the file as the job
//! user, and `openjd-sessions` (through `#[path]`), which reads it directly
//! for a same-user Session. So it uses only `std`, `nix`, and `windows`, and
//! nothing from either crate.

use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// The prefix that makes a line an `openjd_output_var` directive.
pub const DIRECTIVE_PREFIX: &str = "openjd_output_var:";

/// Where the parts of an `@` directive are in its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDirective {
    /// The byte range of the output's name in the line.
    pub name: Range<usize>,
    /// The task selector's value, if the directive has one.
    pub task: Option<i64>,
    /// The byte range of the file path in the line: the text after `@`
    /// without trailing spaces and tabs, never empty.
    pub path: Range<usize>,
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Recognize `line` as an `@` directive: `openjd_output_var:`, optional spaces
/// or tabs, an identifier, an optional `[<Int>]` task selector, `=`, optional
/// spaces or tabs, `@`, and a non-empty path with trailing spaces and tabs
/// removed.
///
/// It accepts exactly the lines that `openjd_model::outputs::parse_output_directive`
/// parses (after the prefix) as `OutputDirective::File`, with the same name,
/// selector, and path; a test in `openjd-sessions` pins the two together. Any
/// other line, including a malformed directive, is `None`: the caller passes
/// it on unchanged, for the model parser to report.
pub fn recognize_file_directive(line: &str) -> Option<FileDirective> {
    let bytes = line.as_bytes();
    if !line.starts_with(DIRECTIVE_PREFIX) {
        return None;
    }
    let mut i = DIRECTIVE_PREFIX.len();
    while i < bytes.len() && is_ws(bytes[i]) {
        i += 1;
    }
    // <Identifier>: [A-Za-z_][A-Za-z0-9_]*
    let name_start = i;
    while i < bytes.len()
        && (bytes[i] == b'_'
            || bytes[i].is_ascii_alphabetic()
            || (i > name_start && bytes[i].is_ascii_digit()))
    {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = name_start..i;
    let task = if bytes.get(i) == Some(&b'[') {
        let close = i + 1 + line[i + 1..].find(']')?;
        let digits = &line[i + 1..close];
        let unsigned = digits.strip_prefix('-').unwrap_or(digits);
        if unsigned.is_empty() || !unsigned.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value = digits.parse::<i64>().ok()?;
        i = close + 1;
        Some(value)
    } else {
        None
    };
    if bytes.get(i) != Some(&b'=') {
        return None;
    }
    i += 1;
    while i < bytes.len() && is_ws(bytes[i]) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'@') {
        return None;
    }
    i += 1;
    let mut end = bytes.len();
    while end > i && is_ws(bytes[end - 1]) {
        end -= 1;
    }
    if end == i {
        return None;
    }
    Some(FileDirective {
        name,
        task,
        path: i..end,
    })
}

/// Read the file an `@` directive names, as the current user.
///
/// A relative `path` is resolved against `root`, the Session working
/// directory; an absolute one is used as written. The file actually opened
/// must be a regular file inside `root` after following symbolic links, which
/// is checked on the opened handle, so a link swapped in between a check and
/// the open can't redirect the read. The open does not block, so a FIFO is
/// refused instead of stalling the reader. At most `max_bytes + 1` bytes are
/// read, so a caller can tell a file that exceeds `max_bytes`.
///
/// The error is a phrase that completes "the @ file '<path>' ...", such as
/// "does not exist".
pub fn read_output_file(root: &Path, path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let file = open_inside(root, path)?;
    let metadata = file.metadata().map_err(|e| format!("can't be read: {e}"))?;
    if !metadata.is_file() {
        return Err("is not a regular file".to_string());
    }
    let mut content = Vec::new();
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(limit)
        .read_to_end(&mut content)
        .map_err(|e| format!("can't be read: {e}"))?;
    Ok(content)
}

/// The error phrase for a failed open.
fn open_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "does not exist".to_string(),
        _ => format!("can't be opened: {e}"),
    }
}

const OUTSIDE: &str = "is not inside the Session working directory";

/// Open `path` (relative to `root`, or absolute) for reading without blocking,
/// and check that the opened file is inside `root`.
fn open_inside(root: &Path, path: &str) -> Result<std::fs::File, String> {
    let canonical_root =
        std::fs::canonicalize(root).map_err(|e| format!("can't be checked: {e}"))?;
    let joined = root.join(path);

    #[cfg(target_os = "linux")]
    {
        // Resolve beneath a handle to the working directory: this follows
        // symbolic links but refuses any that leave it. It also refuses an
        // absolute link even when its target is inside, and some kernels or
        // sandboxes lack openat2, so those cases fall back to opening the
        // path and checking the handle's final path below.
        if let Some(relative) = relative_to(&joined, root, &canonical_root) {
            match open_beneath(&canonical_root, &relative) {
                Ok(file) => return Ok(file),
                Err(BeneathError::Fallback) => {}
                Err(BeneathError::Io(e)) => return Err(open_error(&e)),
            }
        }
    }

    let file = open_nonblocking(&joined).map_err(|e| open_error(&e))?;
    let final_path = final_path(&file).map_err(|e| format!("can't be checked: {e}"))?;
    if is_inside(&final_path, &canonical_root) {
        Ok(file)
    } else {
        Err(OUTSIDE.to_string())
    }
}

/// `joined` relative to `root` (as given or canonical), lexically, if it is
/// under it; `None` otherwise.
#[cfg(target_os = "linux")]
fn relative_to(joined: &Path, root: &Path, canonical_root: &Path) -> Option<PathBuf> {
    joined
        .strip_prefix(root)
        .or_else(|_| joined.strip_prefix(canonical_root))
        .ok()
        .filter(|r| !r.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// Whether `path` is `root` or inside it, comparing components
/// (case-insensitively on Windows). `root` itself is a directory, which the
/// regular-file check then refuses.
fn is_inside(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
        lower(path).starts_with(lower(root))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

#[cfg(target_os = "linux")]
enum BeneathError {
    /// openat2 refused for a reason the fallback can still decide.
    Fallback,
    Io(std::io::Error),
}

#[cfg(target_os = "linux")]
fn open_beneath(root: &Path, relative: &Path) -> Result<std::fs::File, BeneathError> {
    use nix::errno::Errno;
    use nix::fcntl::{open, openat2, OFlag, OpenHow, ResolveFlag};
    use nix::sys::stat::Mode;

    let dir = open(
        root,
        OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| BeneathError::Io(e.into()))?;
    let how = OpenHow::new()
        .flags(OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOCTTY | OFlag::O_CLOEXEC)
        .resolve(ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_MAGICLINKS);
    match openat2(&dir, relative, how) {
        Ok(fd) => Ok(std::fs::File::from(fd)),
        // EXDEV: the path or a link in it leaves the root, or is an absolute
        // link. ENOSYS, EPERM (seccomp), EINVAL, E2BIG: openat2 unavailable.
        Err(Errno::EXDEV | Errno::ENOSYS | Errno::EPERM | Errno::EINVAL | Errno::E2BIG) => {
            Err(BeneathError::Fallback)
        }
        Err(e) => Err(BeneathError::Io(e.into())),
    }
}

/// Open for reading without blocking on a FIFO, and without making a terminal
/// the controlling one.
#[cfg(unix)]
fn open_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY)
        .open(path)
}

/// Open for reading. Windows has no FIFOs in the file system namespace, and
/// opening a file does not block.
#[cfg(windows)]
fn open_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// The final path of an open file, after every symbolic link.
#[cfg(target_os = "linux")]
fn final_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_vendor = "apple")]
fn final_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    let mut path = PathBuf::new();
    nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_GETPATH(&mut path))?;
    Ok(path)
}

#[cfg(windows)]
fn final_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED};

    let handle = HANDLE(file.as_raw_handle());
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `handle` is an open file handle owned by `file`, and `buf`
        // is a valid, writable buffer of the length passed.
        let n = unsafe { GetFinalPathNameByHandleW(handle, &mut buf, FILE_NAME_NORMALIZED) };
        let n = n as usize;
        if n == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n < buf.len() {
            buf.truncate(n);
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
        }
        // Too small: `n` is the size needed, including the terminator.
        buf.resize(n + 1, 0);
    }
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn final_path(_file: &std::fs::File) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this platform can't check where an opened file is",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(line: &str) -> Option<(&str, Option<i64>, &str)> {
        recognize_file_directive(line).map(|d| (&line[d.name], d.task, &line[d.path]))
    }

    #[test]
    fn recognizes_the_at_forms() {
        assert_eq!(
            parts("openjd_output_var: K=@keys.json"),
            Some(("K", None, "keys.json"))
        );
        assert_eq!(parts("openjd_output_var:K=@f"), Some(("K", None, "f")));
        assert_eq!(
            parts("openjd_output_var:\t N[012]= \t@a b \t"),
            Some(("N", Some(12), "a b"))
        );
        assert_eq!(
            parts("openjd_output_var: N[-3]=@f"),
            Some(("N", Some(-3), "f"))
        );
        assert_eq!(parts("openjd_output_var: N=@ f"), Some(("N", None, " f")));
    }

    #[test]
    fn leaves_everything_else_alone() {
        for line in [
            "openjd_output_var: N=1",
            "openjd_output_var: N=@",
            "openjd_output_var: N=@ \t",
            "openjd_output_var: N =@f",
            "openjd_output_var: 1N=@f",
            "openjd_output_var: N[]=@f",
            "openjd_output_var: N[ 1]=@f",
            "openjd_output_var: N[+1]=@f",
            "openjd_output_var: N[99999999999999999999]=@f",
            "openjd_output_var: N[1=@f",
            " openjd_output_var: N=@f",
            "openjd_output_var N=@f",
            "openjd_output_var: N=\"@f\"",
        ] {
            assert_eq!(recognize_file_directive(line), None, "{line:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn reads_regular_files_inside_and_refuses_the_rest() {
        let tmp = tempfile_dir();
        let root = tmp.join("wd");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("v.json"), b"[1, 2]").unwrap();
        std::fs::write(tmp.join("outside.json"), b"1").unwrap();
        std::os::unix::fs::symlink("v.json", root.join("rel_link")).unwrap();
        std::os::unix::fs::symlink(root.join("v.json"), root.join("abs_link")).unwrap();
        std::os::unix::fs::symlink(tmp.join("outside.json"), root.join("out_link")).unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        nix::unistd::mkfifo(&root.join("fifo"), nix::sys::stat::Mode::S_IRWXU).unwrap();

        assert_eq!(read_output_file(&root, "v.json", 100).unwrap(), b"[1, 2]");
        let abs = root.join("v.json");
        assert_eq!(
            read_output_file(&root, abs.to_str().unwrap(), 100).unwrap(),
            b"[1, 2]"
        );
        assert_eq!(read_output_file(&root, "rel_link", 100).unwrap(), b"[1, 2]");
        assert_eq!(read_output_file(&root, "abs_link", 100).unwrap(), b"[1, 2]");
        // At most max_bytes + 1 bytes.
        assert_eq!(read_output_file(&root, "v.json", 2).unwrap(), b"[1,");

        assert_eq!(
            read_output_file(&root, "out_link", 100).unwrap_err(),
            OUTSIDE
        );
        assert_eq!(
            read_output_file(&root, "../outside.json", 100).unwrap_err(),
            OUTSIDE
        );
        let outside = tmp.join("outside.json");
        assert_eq!(
            read_output_file(&root, outside.to_str().unwrap(), 100).unwrap_err(),
            OUTSIDE
        );
        assert_eq!(
            read_output_file(&root, "dir", 100).unwrap_err(),
            "is not a regular file"
        );
        assert_eq!(
            read_output_file(&root, "fifo", 100).unwrap_err(),
            "is not a regular file"
        );
        // An absolute link makes openat2 refuse (EXDEV), so the fallback
        // opens the path; it must not block on the FIFO either.
        std::os::unix::fs::symlink(root.join("fifo"), root.join("abs_fifo")).unwrap();
        assert_eq!(
            read_output_file(&root, "abs_fifo", 100).unwrap_err(),
            "is not a regular file"
        );
        assert_eq!(
            read_output_file(&root, "missing", 100).unwrap_err(),
            "does not exist"
        );
        // The working directory itself is not a regular file.
        assert_eq!(
            read_output_file(&root, ".", 100).unwrap_err(),
            "is not a regular file"
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A fresh directory under the system temp dir (the helper crate has no
    /// `tempfile` dependency).
    #[cfg(unix)]
    fn tempfile_dir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "openjd_output_file_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }
}
