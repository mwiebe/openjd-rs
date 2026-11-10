// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Find which files cannot be deleted, and the processes holding files open
//! via the Windows Restart Manager.
//!
//! Used only to explain a failed cleanup in the log, so every failure here is
//! reported to the caller as a string and never stops the cleanup.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::RestartManager::{
    RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
    RM_PROCESS_INFO,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::tempdir::FileHolder;

/// `ERROR_ACCESS_DENIED`.
const ACCESS_DENIED: i32 = 5;
/// `ERROR_SHARING_VIOLATION`.
const SHARING_VIOLATION: i32 = 32;
/// The `DELETE` access right.
const DELETE: u32 = 0x0001_0000;
/// The `FILE_WRITE_DATA` access right.
const FILE_WRITE_DATA: u32 = 0x0002;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
const SHARE_ALL: u32 = 0x7;
/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a link itself, never its target.
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Whether deleting the file at `path` would fail now. Only opens and reads
/// it, and never deletes or changes anything.
///
/// Opening for `DELETE` fails with a sharing violation when another handle
/// does not share deletion, and with access denied when the ACLs do not allow
/// deleting it. A running program's image accepts `DELETE` but cannot be
/// deleted. For a program file (one starting with the `MZ` of a Windows
/// executable), opening for `DELETE` and writing as well tells it apart: that
/// fails with a sharing violation while the image is mapped. Other files skip
/// that check, because any handle that does not share writing fails it too,
/// although the file can be deleted. A program file held open that way by
/// something other than the loader is still reported. A read-only program
/// file refuses writing with access denied before the image check, so for one
/// the Restart Manager is asked whether any process has it open.
pub(crate) fn cannot_delete_now(path: &Path) -> bool {
    let refused = |access: u32| {
        std::fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(SHARE_ALL)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(path)
            .err()
            .and_then(|e| e.raw_os_error())
    };
    match refused(DELETE) {
        Some(SHARING_VIOLATION | ACCESS_DENIED) => true,
        Some(_) => false,
        None if !is_program_file(path) => false,
        None => match refused(DELETE | FILE_WRITE_DATA) {
            Some(SHARING_VIOLATION) => true,
            Some(ACCESS_DENIED) => {
                is_read_only(path)
                    && processes_holding(&[path.to_path_buf()]).is_ok_and(|h| !h.is_empty())
            }
            _ => false,
        },
    }
}

/// Whether the file has the read-only attribute. Reads metadata without
/// following a link.
fn is_read_only(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.permissions().readonly())
}

/// Whether the file starts with `MZ`, as every Windows executable image does.
fn is_program_file(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 2];
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && magic == *b"MZ"
}

/// Returns the processes that hold any of `files` open.
pub(crate) fn processes_holding(files: &[PathBuf]) -> Result<Vec<FileHolder>, String> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let wide: Vec<Vec<u16>> = files
        .iter()
        .map(|p| p.as_os_str().encode_wide().chain(Some(0)).collect())
        .collect();
    let names: Vec<PCWSTR> = wide.iter().map(|w| PCWSTR(w.as_ptr())).collect();

    let mut handle = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    // SAFETY: `handle` and `key` outlive the call; `key` has the documented
    // CCH_RM_SESSION_KEY + 1 capacity.
    let rc = unsafe { RmStartSession(&mut handle, None, PWSTR(key.as_mut_ptr())) };
    if rc != ERROR_SUCCESS {
        return Err(format!("RmStartSession failed ({})", rc.0));
    }
    let result = list_holders(handle, &names);
    // SAFETY: `handle` came from a successful RmStartSession.
    unsafe {
        let _ = RmEndSession(handle);
    }
    result
}

fn list_holders(handle: u32, names: &[PCWSTR]) -> Result<Vec<FileHolder>, String> {
    // SAFETY: `names` points into NUL-terminated buffers owned by the caller.
    let rc = unsafe { RmRegisterResources(handle, Some(names), None, None) };
    if rc != ERROR_SUCCESS {
        return Err(format!("RmRegisterResources failed ({})", rc.0));
    }
    let mut infos: Vec<RM_PROCESS_INFO> = Vec::new();
    // The list can grow between calls, so ask again a few times on ERROR_MORE_DATA.
    for _ in 0..3 {
        let mut needed = 0u32;
        let mut count = infos.len() as u32;
        let mut reasons = 0u32;
        let buf = if infos.is_empty() {
            None
        } else {
            Some(infos.as_mut_ptr())
        };
        // SAFETY: `buf` is null or points to `count` initialized entries.
        let rc = unsafe { RmGetList(handle, &mut needed, &mut count, buf, &mut reasons) };
        if rc == ERROR_SUCCESS {
            infos.truncate(count as usize);
            return Ok(infos.iter().map(holder_from).collect());
        }
        if rc != ERROR_MORE_DATA {
            return Err(format!("RmGetList failed ({})", rc.0));
        }
        infos = vec![RM_PROCESS_INFO::default(); needed as usize];
    }
    Err("RmGetList kept reporting more processes".into())
}

fn holder_from(info: &RM_PROCESS_INFO) -> FileHolder {
    let pid = info.Process.dwProcessId;
    let name = image_name(pid).unwrap_or_else(|| {
        let end = info
            .strAppName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.strAppName.len());
        String::from_utf16_lossy(&info.strAppName[..end])
    });
    FileHolder { pid, name }
}

/// The executable's file name, such as `UnrealShaderCompileWorker.exe`.
fn image_name(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess with a query-only right; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s and outlives the call.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `process` is a valid handle from OpenProcess.
    unsafe {
        let _ = CloseHandle(process);
    }
    ok.ok()?;
    let path = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn finds_this_process_holding_a_file() {
        let dir = crate::tempdir::TempDir::new(None, None, None).unwrap();
        let path = dir.path().join("held.bin");
        let _held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let holders = processes_holding(&[path]).unwrap();
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            holders.contains(&FileHolder {
                pid: std::process::id(),
                name
            }),
            "expected this process, by image name, in {holders:?}"
        );
    }

    /// With no process to query, the name falls back to the Restart
    /// Manager's application name, up to its NUL.
    #[test]
    fn holder_name_falls_back_to_the_app_name() {
        let mut info = RM_PROCESS_INFO::default();
        // Pid 0 is the System Idle Process, which OpenProcess always refuses.
        info.Process.dwProcessId = 0;
        for (slot, c) in info.strAppName.iter_mut().zip("app\0junk".encode_utf16()) {
            *slot = c;
        }
        assert_eq!(
            holder_from(&info),
            FileHolder {
                pid: 0,
                name: "app".to_string()
            }
        );
    }

    fn open_with_share(path: &Path, share: u32) -> std::fs::File {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(share)
            .open(path)
            .unwrap()
    }

    #[test]
    fn cannot_delete_now_flags_only_files_whose_deletion_would_fail() {
        let dir = crate::tempdir::TempDir::new(None, None, None).unwrap();
        let free = dir.path().join("free.bin");
        std::fs::write(&free, b"x").unwrap();
        assert!(!cannot_delete_now(&free), "a file nobody holds");

        let read_only = dir.path().join("read_only.bin");
        std::fs::write(&read_only, b"x").unwrap();
        let mut perms = std::fs::metadata(&read_only).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&read_only, perms).unwrap();
        assert!(
            !cannot_delete_now(&read_only),
            "remove_file deletes read-only files"
        );

        let shared = dir.path().join("shared.bin");
        let _shared = open_with_share(&shared, SHARE_ALL);
        assert!(!cannot_delete_now(&shared), "a holder that shares deletion");

        let held = dir.path().join("held.bin");
        let _held = open_with_share(&held, 0);
        assert!(cannot_delete_now(&held), "a holder that shares nothing");

        let rw = dir.path().join("rw.bin");
        let _rw = open_with_share(&rw, 0x1 | 0x2);
        assert!(
            cannot_delete_now(&rw),
            "a holder that shares reading and writing only"
        );

        // A holder sharing reading and deletion but not writing does not stop
        // deletion, so only a program file held that way is reported (see the
        // doc comment).
        let doc = dir.path().join("doc.txt");
        std::fs::write(&doc, b"text").unwrap();
        let _doc = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x4)
            .open(&doc)
            .unwrap();
        assert!(!cannot_delete_now(&doc), "a read holder sharing deletion");

        assert!(!cannot_delete_now(&dir.path().join("missing.bin")));
    }

    fn run_hidden(program: &str, args: &[&std::ffi::OsStr]) {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW, so the test opens no console.
        let status = std::process::Command::new(program)
            .args(args)
            .stdout(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .status()
            .unwrap();
        assert!(status.success(), "{program} {args:?}");
    }

    /// A file whose ACLs refuse deletion: the file denies `DELETE` and its
    /// directory denies deleting children, to everyone.
    #[test]
    fn cannot_delete_now_flags_a_file_the_acls_protect() {
        let dir = crate::tempdir::TempDir::new(None, None, None).unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let file = sub.join("protected.bin");
        std::fs::write(&file, b"x").unwrap();
        let deny = |path: &Path, right: &str| {
            let ace = format!("*S-1-1-0:({right})");
            run_hidden(
                "icacls",
                &[path.as_os_str(), "/deny".as_ref(), ace.as_ref()],
            )
        };
        deny(&file, "DE");
        deny(&sub, "DC");
        let flagged = cannot_delete_now(&file);
        let removed = std::fs::remove_file(&file);
        for path in [&sub, &file] {
            run_hidden(
                "icacls",
                &[path.as_os_str(), "/remove:d".as_ref(), "*S-1-1-0".as_ref()],
            );
        }
        assert!(removed.is_err(), "the protected file was deleted");
        assert!(flagged);
    }

    /// A running program's image accepts `DELETE` access but cannot be deleted.
    /// The Restart Manager names its holder by image file name, not by the
    /// program's description ("TCP/IP Ping Command").
    #[test]
    fn cannot_delete_now_flags_a_running_program() {
        use std::os::windows::process::CommandExt;
        let dir = crate::tempdir::TempDir::new(None, None, None).unwrap();
        let exe = dir.path().join("ping.exe");
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::fs::copy(Path::new(&system).join("System32").join("ping.exe"), &exe).unwrap();
        // CREATE_NO_WINDOW, so the test opens no console.
        let mut child = std::process::Command::new(&exe)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let flagged = cannot_delete_now(&exe);
        let holders = processes_holding(std::slice::from_ref(&exe));
        let removed = std::fs::remove_file(&exe);
        let pid = child.id();
        let _ = child.kill();
        let _ = child.wait();
        assert!(removed.is_err(), "the running image was deleted");
        assert!(flagged);
        // Other holders can appear too, such as a scanner inspecting the new
        // executable.
        let holders = holders.unwrap();
        let ping = FileHolder {
            pid,
            name: "ping.exe".to_string(),
        };
        assert!(holders.contains(&ping), "{holders:?}");
    }

    /// A read-only program file: not reported while nothing runs it, since
    /// `remove_file` deletes read-only files, and reported while it runs.
    #[test]
    fn cannot_delete_now_flags_a_running_read_only_program() {
        use std::os::windows::process::CommandExt;
        let dir = crate::tempdir::TempDir::new(None, None, None).unwrap();
        let exe = dir.path().join("ping.exe");
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::fs::copy(Path::new(&system).join("System32").join("ping.exe"), &exe).unwrap();
        let mut perms = std::fs::metadata(&exe).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&exe, perms).unwrap();
        assert!(!cannot_delete_now(&exe), "a read-only program nothing runs");
        // CREATE_NO_WINDOW, so the test opens no console.
        let mut child = std::process::Command::new(&exe)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let flagged = cannot_delete_now(&exe);
        let removed = std::fs::remove_file(&exe);
        let _ = child.kill();
        let _ = child.wait();
        assert!(removed.is_err(), "the running image was deleted");
        assert!(flagged);
    }

    #[test]
    fn a_file_nobody_holds_has_no_holders() {
        let dir = crate::tempdir::TempDir::new(None, None, None).unwrap();
        let path = dir.path().join("free.bin");
        std::fs::write(&path, b"x").unwrap();
        assert!(processes_holding(&[path]).unwrap().is_empty());
    }
}
