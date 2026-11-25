// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use super::Stamp;

const DIR_FLAGS: libc::c_int = libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK;

pub(super) fn open_root(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(DIR_FLAGS)
        .open(path)
}

pub(super) fn stamp(file: &File) -> io::Result<Stamp> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(Stamp {
        device: meta.dev(),
        identity: u128::from(meta.ino()),
        len: meta.len(),
        mtime: meta.mtime().clamp(0, i64::from(u32::MAX)) as u32,
        times: [
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        ],
    })
}

pub(super) fn open_relative(root: &File, path: &[u8]) -> io::Result<File> {
    let mut components = path.split(|byte| *byte == b'/').peekable();
    let device = root.metadata()?.dev();
    let mut parent = None;
    while let Some(component) = components.next() {
        // Also refuse dot components at the syscall boundary.
        if component.is_empty() || component == b"." || component == b".." {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let name = CString::new(component).map_err(|_| io::ErrorKind::InvalidInput)?;
        let directory = parent.as_ref().unwrap_or(root);
        let leaf = components.peek().is_none();
        let flags = if leaf {
            check_regular(directory, &name)?;
            libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            DIR_FLAGS
        };
        // SAFETY: directory owns a live descriptor and name is NUL terminated.
        // O_CREAT is absent, so openat takes no mode argument.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        let handle = unsafe { File::from_raw_fd(fd) };
        let metadata = handle.metadata()?;
        if metadata.dev() != device || (leaf && !metadata.is_file()) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        if leaf {
            return Ok(handle);
        }
        parent = Some(handle);
    }
    Err(io::ErrorKind::InvalidInput.into())
}

fn check_regular(directory: &File, name: &std::ffi::CStr) -> io::Result<()> {
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the descriptor and terminated name are valid and info is writable.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            info.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat initialized info on success.
    let info = unsafe { info.assume_init() };
    if info.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(io::ErrorKind::Unsupported.into());
    }
    #[cfg(target_os = "macos")]
    if info.st_flags & 0x4000_0000 != 0 {
        // Darwin SF_DATALESS. Opening a dataless file for reading can fetch it.
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(())
}
