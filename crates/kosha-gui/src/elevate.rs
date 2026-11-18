// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Starting kosha again, as administrator.
//!
//! An elevated scan reads the volume's Master File Table directly: several
//! times faster on a whole drive, exact on compressed and sparse files, and
//! able to see inside directories an ordinary user has no permission to open.
//! None of that can be granted to a process that is already running — Windows
//! decides what a token holds when the process starts — so the only way there
//! is to start a new one and stop this one.
//!
//! That makes it a deliberate, visible step rather than something that happens
//! quietly: the user sees the consent dialog with kosha's name on it, chooses,
//! and gets a window that says "Administrator" in the title bar for as long as
//! it lasts.
//!
//! **Elevation is not read-write mode.** The new window opens read-only like
//! every other one, because the two are separate choices and the combination of
//! both is the only one where a mistake cannot be undone. Nothing here touches
//! [`kosha_fsops::Session`].

/// Ask Windows to start kosha again with an elevated token.
///
/// Returns `Ok(())` once the new process has been launched, at which point the
/// caller should close this window: two kosha windows both claiming to be the
/// session is worse than either outcome.
///
/// The consent dialog is Windows', and a user who declines it comes back here
/// as an error rather than as a hang — `ERROR_CANCELLED`, which is reported as
/// the refusal it is and not as a failure.
#[cfg(windows)]
pub fn restart_as_administrator() -> Result<(), String> {
    use windows_sys::Win32::Foundation::ERROR_CANCELLED;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe().map_err(|e| format!("could not find kosha itself: {e}"))?;

    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    // Deliberately no arguments. The new process starts as a fresh session on
    // whatever it opens by default: carrying this window's scan path across
    // would make the restart look like a rescan, and carrying its state would
    // mean deciding what a *different privilege level* inherits, which is a
    // question with no good answer.
    let directory = std::env::current_dir().unwrap_or_default();
    let directory = wide(directory.as_os_str());

    // SAFETY: all four pointers are NUL-terminated UTF-16 buffers that outlive
    // the call, and the window handle is null, which this call accepts — the
    // consent dialog then parents itself to the desktop.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            directory.as_ptr(),
            SW_SHOWNORMAL,
        )
    };

    // The return is a fake `HINSTANCE` that is an error code when it is 32 or
    // less. This is the documented contract and it is as odd as it looks.
    let code = result as usize;
    if code > 32 {
        return Ok(());
    }
    if code as u32 == ERROR_CANCELLED {
        return Err("Administrator access was declined.".into());
    }
    Err(format!(
        "Windows would not start kosha as administrator (error {code})."
    ))
}

#[cfg(windows)]
fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = s.encode_wide().collect();
    wide.push(0);
    wide
}

/// Nothing to offer off Windows.
///
/// The elevated scan is an NTFS strategy, and Unix already gets from one
/// `lstat` most of what it buys: exact on-disk sizes from `st_blocks` and free
/// hardlink identity from `st_ino`. Running as root would let a scan read
/// directories it otherwise cannot, which is a real difference — but not one
/// worth a button that asks somebody to restart a GUI as root.
#[cfg(not(windows))]
pub fn restart_as_administrator() -> Result<(), String> {
    Err("kosha can only restart itself as administrator on Windows.".into())
}

/// Whether the interface should offer to restart elevated at all.
pub fn can_offer() -> bool {
    cfg!(windows) && !kosha_core::elevated()
}
