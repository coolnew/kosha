// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::Packaging::Appx::{
    GetPackageFullName, PACKAGE_FULL_NAME_MAX_LENGTH,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessId, QueryFullProcessImageNameW,
    WaitForSingleObject,
};

#[derive(Clone, Copy, Debug)]
pub enum PipeEnd {
    Server,
    Client,
}

#[derive(Clone, Copy, Debug)]
pub enum PackageMode {
    /// Both endpoints must belong to the same installed package version.
    SamePackage,
    /// The private standalone replay uses two unpackaged executables.
    Unpackaged,
}

/// Check a connected local pipe against the independently pinned peer process.
/// The caller must obtain and authorize that process handle before opening
/// the channel, then keep it alive for the whole request. A handle opened
/// from the pipe's own reported PID cannot establish the expected peer.
///
/// The process handle must include query and synchronization rights.
/// Supply a fixed sibling executable name from the caller's role policy.
/// For Store runs, require both endpoints to have the same package full name.
pub fn verify_peer(
    pipe: BorrowedHandle<'_>,
    end: PipeEnd,
    expected_process: BorrowedHandle<'_>,
    executable: &OsStr,
    package_mode: PackageMode,
) -> io::Result<()> {
    let mut actual_pid = 0;
    // SAFETY: pipe is held live by the caller and actual_pid is writable.
    let ok = unsafe {
        match end {
            PipeEnd::Server => GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut actual_pid),
            PipeEnd::Client => GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut actual_pid),
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let expected = expected_process.as_raw_handle();
    // SAFETY: expected_process is borrowed for the duration of this check.
    let expected_pid = unsafe { GetProcessId(expected) };
    if expected_pid == 0 {
        return Err(io::Error::last_os_error());
    }
    if actual_pid != expected_pid {
        return Err(denied("unexpected fast NTFS pipe process"));
    }
    // SAFETY: expected is the pinned live process handle supplied above.
    if unsafe { WaitForSingleObject(expected, 0) } != WAIT_TIMEOUT {
        return Err(denied("fast NTFS pipe process is unavailable"));
    }

    // SAFETY: these functions return values for the running process.
    let (current, current_pid) = unsafe { (GetCurrentProcess(), GetCurrentProcessId()) };
    let mut current_session = 0;
    let mut peer_session = 0;
    // SAFETY: both out-parameters are writable and both PIDs are nonzero.
    if unsafe { ProcessIdToSessionId(current_pid, &mut current_session) } == 0
        || unsafe { ProcessIdToSessionId(expected_pid, &mut peer_session) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if current_session != peer_session || current_session == 0 {
        return Err(denied(
            "fast NTFS pipe peer is outside the interactive session",
        ));
    }

    let owner = crate::platform::process_owner(current)
        .ok_or_else(|| denied("cannot determine the initiating account"))?;
    let peer_owner = crate::platform::process_owner(expected)
        .ok_or_else(|| denied("cannot determine the fast NTFS peer account"))?;
    if owner != peer_owner {
        return Err(denied("fast NTFS requires same-account elevation"));
    }
    verify_executable(current, expected, executable, package_mode)?;
    Ok(())
}

fn verify_executable(
    current: HANDLE,
    peer: HANDLE,
    executable: &OsStr,
    package_mode: PackageMode,
) -> io::Result<()> {
    if Path::new(executable).file_name() != Some(executable) {
        return Err(denied(
            "expected fast NTFS executable must be a sibling name",
        ));
    }
    let current_image = image_path(current)?;
    let sibling = current_image
        .parent()
        .ok_or_else(|| denied("cannot determine the fast NTFS installation directory"))?
        .join(executable);
    let peer_image = image_path(peer)?;
    if !crate::snapshot::store::same_root(&sibling, &peer_image) {
        return Err(denied("unexpected fast NTFS peer executable"));
    }

    let current_package = package_name(current)?;
    let peer_package = package_name(peer)?;
    match package_mode {
        PackageMode::SamePackage
            if current_package.is_some() && current_package == peer_package =>
        {
            Ok(())
        }
        PackageMode::Unpackaged if current_package.is_none() && peer_package.is_none() => Ok(()),
        _ => Err(denied("unexpected fast NTFS peer package identity")),
    }
}

pub(super) fn image_path(process: HANDLE) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    // SAFETY: process is live, buffer and its input/output length are writable.
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let path = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
    path.canonicalize().map(crate::path::simplify)
}

pub(super) fn package_name(process: HANDLE) -> io::Result<Option<OsString>> {
    let mut buffer = [0u16; (PACKAGE_FULL_NAME_MAX_LENGTH + 1) as usize];
    let mut length = buffer.len() as u32;
    // SAFETY: process is live; the UTF-16 buffer and its length are writable.
    let status = unsafe { GetPackageFullName(process, &mut length, buffer.as_mut_ptr()) };
    match status {
        APPMODEL_ERROR_NO_PACKAGE => Ok(None),
        0 if length > 1 && length as usize <= buffer.len() => {
            Ok(Some(OsString::from_wide(&buffer[..length as usize - 1])))
        }
        0 => Err(denied("invalid fast NTFS process package name")),
        error => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

fn denied(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}
