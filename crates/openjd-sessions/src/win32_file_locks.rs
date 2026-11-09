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
/// `ERROR_LOCK_VIOLATION`.
const LOCK_VIOLATION: i32 = 33;
/// The `DELETE` access right.
const DELETE: u32 = 0x0001_0000;
/// The `FILE_WRITE_DATA` access right.
const FILE_WRITE_DATA: u32 = 0x0002;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
const SHARE_ALL: u32 = 0x7;
/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a link itself, never its target.
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Whether deleting the file at `path` would fail now. Only opens it, and
/// never deletes or changes anything.
///
/// Opening for `DELETE` fails with a sharing violation when another handle
/// does not share deletion, and with access denied when the file's ACL does
/// not allow deleting it. A running program's image accepts `DELETE` but
/// cannot be deleted; opening it for writing as well fails with a sharing
/// violation. A read-only file fails that second open with access denied,
/// but can be deleted, so access denied there does not count.
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
        Some(SHARING_VIOLATION | LOCK_VIOLATION | ACCESS_DENIED) => true,
        Some(_) => false,
        None => matches!(
            refused(DELETE | FILE_WRITE_DATA),
            Some(SHARING_VIOLATION | LOCK_VIOLATION)
        ),
    }
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

        assert!(!cannot_delete_now(&dir.path().join("missing.bin")));
    }

    /// A running program's image accepts `DELETE` access but cannot be deleted.
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
