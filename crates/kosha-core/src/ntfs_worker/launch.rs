// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, Prefix};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, SYNCHRONIZE};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};

use super::peer::{image_path, package_name};
use super::{
    LocalServer, PackageMode, PipeEnd, Request, open_local_client, scan_once, verify_peer,
};

const WORKER: &str = "kosha-ntfs-worker.exe";
const FRONTENDS: [&str; 2] = ["kosha-gui.exe", "kosha.exe"];
const CONNECT_LIMIT: Duration = Duration::from_secs(10);
const SCAN_LIMIT: Duration = Duration::from_secs(30 * 60);

pub fn worker_available() -> bool {
    own_endpoint().ok().is_some_and(|(image, name)| {
        FRONTENDS.contains(&name.as_str())
            && image.parent().is_some_and(|dir| dir.join(WORKER).is_file())
    })
}

/// Offer the worker only for a mounted NTFS drive root in a package that
/// includes it. This does not request elevation.
pub fn request_for_root(root: &Path) -> Option<Request> {
    if !worker_available() {
        return None;
    }
    let letter = drive_root_letter(root)?;
    let drive = [u16::from(letter), b':' as u16, b'\\' as u16, 0];
    let mut filesystem = [0u16; 16];
    // SAFETY: drive is NUL-terminated and the only requested output is writable.
    let found = unsafe {
        GetVolumeInformationW(
            drive.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    };
    if found == 0 || filesystem[..5] != (*b"NTFS\0").map(u16::from) {
        return None;
    }
    let normalized = format!("{}:\\", char::from(letter));
    let identity = crate::volume_history::VolumeId::at_root(Path::new(&normalized))?;
    Request::new(letter, identity.as_str()).ok()
}

fn drive_root_letter(root: &Path) -> Option<u8> {
    let mut parts = root.components();
    let Component::Prefix(prefix) = parts.next()? else {
        return None;
    };
    let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
        return None;
    };
    (parts.next() == Some(Component::RootDir) && parts.next().is_none()).then_some(letter)
}

#[cfg(test)]
#[test]
fn worker_preflight_requires_a_whole_lettered_drive() {
    for path in [r"K:\", "k:/", r"\\?\K:\"] {
        assert_eq!(drive_root_letter(Path::new(path)), Some(b'K'), "{path}");
    }
    for path in ["K:", r"K:\folder", r"K:\..", r"\", ".", r"\\server\share\"] {
        assert_eq!(drive_root_letter(Path::new(path)), None, "{path}");
    }
}

/// Start one packaged scan after an explicit user action. Run on a
/// background thread since Windows may wait for UAC consent.
pub fn scan_with_worker(
    request: &Request,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<crate::SizeTree> {
    let (image, name) = own_endpoint()?;
    if !FRONTENDS.contains(&name.as_str()) {
        return Err(denied("fast NTFS must start from a Kosha front end"));
    }
    if cancelled() {
        return Err(aborted());
    }
    let directory = image
        .parent()
        .ok_or_else(|| denied("missing Kosha install directory"))?;
    let executable = directory.join(WORKER);
    if !executable.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the Fast NTFS worker is missing",
        ));
    }
    let listener = LocalServer::create()?;
    // The rendezvous name contains only fixed punctuation and hex digits.
    let args = format!("{} {} {name}", listener.name(), std::process::id());
    let worker = launch_elevated(&executable, directory, &args)?;
    let result = (|| {
        let stopped = || cancelled() || exited(&worker);
        let pipe = listener.accept(Instant::now() + CONNECT_LIMIT, &stopped)?;
        verify_peer(
            pipe.as_handle(),
            PipeEnd::Server,
            worker.as_handle(),
            OsStr::new(WORKER),
            PackageMode::SamePackage,
        )?;
        let deadline = Instant::now() + SCAN_LIMIT;
        request.write(pipe.transfer(deadline, cancelled))?;
        request
            .read_result(pipe.transfer(deadline, cancelled))
            .map_err(|error| match error {
                crate::snapshot::ReadError::Io(error) => error,
                error => io::Error::other(error),
            })
    })();
    // The pipe is closed on every exchange outcome. Give the worker time
    // to observe closure before a normal return or fallback starts.
    // SAFETY: worker owns a live process handle from ShellExecuteEx.
    let stopped = unsafe { WaitForSingleObject(worker.as_raw_handle(), 2000) } == WAIT_OBJECT_0;
    if cancelled() {
        return Err(aborted());
    }
    let tree = result?;
    if !stopped {
        return Err(io::Error::other("the Fast NTFS worker did not finish"));
    }
    let mut status = 0;
    // SAFETY: the process handle is live and status is a writable out-parameter.
    if unsafe { GetExitCodeProcess(worker.as_raw_handle(), &mut status) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if status != 0 {
        return Err(io::Error::other("the Fast NTFS worker failed"));
    }
    Ok(tree)
}

/// Worker entry point. Only a verified same-package front end may issue
/// a request. The command line carries connection information, never a
/// file path, operation, output destination or volume.
pub fn worker_main(args: impl IntoIterator<Item = OsString>) -> io::Result<()> {
    let mut args = args.into_iter();
    let pipe = args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .ok_or_else(invalid)?;
    let pid = args
        .next()
        .and_then(|arg| arg.to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != 0)
        .ok_or_else(invalid)?;
    let role = args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .ok_or_else(invalid)?;
    if args.next().is_some() || !FRONTENDS.contains(&role.as_str()) {
        return Err(invalid());
    }
    let (_, own_name) = own_endpoint()?;
    if own_name != WORKER || !crate::elevated() {
        return Err(denied(
            "the Fast NTFS worker requires packaged administrator consent",
        ));
    }
    // SAFETY: only query and synchronization access is requested.
    let parent = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if parent.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess returned a new owned handle.
    let parent = unsafe { OwnedHandle::from_raw_handle(parent) };
    let pipe = open_local_client(&pipe)?;
    verify_peer(
        pipe.as_handle(),
        PipeEnd::Client,
        parent.as_handle(),
        OsStr::new(&role),
        PackageMode::SamePackage,
    )?;
    let deadline = Instant::now() + SCAN_LIMIT;
    let next_check = Cell::new(Instant::now());
    let stopped = Cell::new(false);
    let cancelled = || {
        let now = Instant::now();
        if now >= next_check.get() {
            stopped.set(stopped.get() || exited(&parent) || !pipe.is_connected());
            next_check.set(now + Duration::from_millis(20));
        }
        stopped.get() || now >= deadline
    };
    scan_once(
        pipe.transfer(Instant::now() + CONNECT_LIMIT, &cancelled),
        pipe.transfer(deadline, &cancelled),
        &cancelled,
    )
    .map_err(io::Error::other)
}

fn own_endpoint() -> io::Result<(std::path::PathBuf, String)> {
    // SAFETY: this pseudo-handle refers to the calling process.
    let process = unsafe { GetCurrentProcess() };
    if package_name(process)?.is_none() {
        return Err(denied(
            "Fast NTFS worker launch requires an installed Kosha package",
        ));
    }
    let image = image_path(process)?;
    let name = image
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(invalid)?
        .to_ascii_lowercase();
    Ok((image, name))
}

fn launch_elevated(executable: &Path, directory: &Path, args: &str) -> io::Result<OwnedHandle> {
    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain([0]).collect()
    }
    let file = wide(executable.as_os_str());
    let directory = wide(directory.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let args = wide(OsStr::new(args));
    // SAFETY: initialize an apartment for this call and release only a
    // successfully acquired COM initialization.
    let initialized = unsafe {
        CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if initialized < 0 && initialized != RPC_E_CHANGED_MODE {
        return Err(io::Error::other(format!(
            "COM initialization failed: 0x{initialized:08x}"
        )));
    }
    let mut request = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: args.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: 0,
        ..Default::default()
    };
    // SAFETY: all strings and the receiving structure are valid for this call.
    let launched = unsafe { ShellExecuteExW(&mut request) };
    let error = (launched == 0).then(io::Error::last_os_error);
    if initialized >= 0 {
        // SAFETY: balances the successful COM initialization above.
        unsafe { CoUninitialize() };
    }
    if let Some(error) = error {
        return Err(error);
    }
    if request.hProcess.is_null() {
        return Err(io::Error::other(
            "Windows did not return a Fast NTFS worker process",
        ));
    }
    // SAFETY: SEE_MASK_NOCLOSEPROCESS transfers this handle to the caller.
    Ok(unsafe { OwnedHandle::from_raw_handle(request.hProcess) })
}

fn exited(process: &OwnedHandle) -> bool {
    // SAFETY: process is a live synchronized handle.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) != WAIT_TIMEOUT }
}

fn denied(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid Fast NTFS worker arguments",
    )
}

fn aborted() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "Fast NTFS scan cancelled")
}
