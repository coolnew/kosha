// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_NON_DIRECTORY_FILE, FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, NtOpenFile,
};
use windows_sys::Win32::Foundation::{OBJ_DONT_REPARSE, RtlNtStatusToDosError, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo,
    GetFileInformationByHandleEx, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::Stamp;
use crate::ntfs::record::filetime_to_unix;
use crate::platform::{NOT_RESIDENT, file_identity};

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const UNSAFE_ATTRIBUTES: u32 = FILE_ATTRIBUTE_REPARSE_POINT | NOT_RESIDENT;

pub(super) fn open_root(path: &Path) -> io::Result<File> {
    let root = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(SHARE)
        .custom_flags(
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
        )
        .open(path)?;
    let meta = root.metadata()?;
    if !meta.is_dir() || meta.file_attributes() & UNSAFE_ATTRIBUTES != 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(root)
}

pub(super) fn open_relative(root: &File, path: &[u8]) -> io::Result<File> {
    let path = std::str::from_utf8(path).map_err(|_| io::ErrorKind::Unsupported)?;
    if path.split('/').any(|part| {
        part.is_empty() || matches!(part, "." | "..") || part.contains(['\\', ':', '\0'])
    }) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut name: Vec<u16> = path
        .encode_utf16()
        .map(|unit| {
            if unit == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                unit
            }
        })
        .collect();
    let length =
        u16::try_from(name.len() * size_of::<u16>()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_DONT_REPARSE,
        ..Default::default()
    };
    let observed = open(&attributes, FILE_READ_ATTRIBUTES)?;
    let expected = stamp(&observed)?;
    let data = open(&attributes, FILE_READ_ATTRIBUTES | FILE_READ_DATA)?;
    if stamp(&data)? != expected {
        return Err(super::changed());
    }
    Ok(data)
}

fn open(attributes: &OBJECT_ATTRIBUTES, access: u32) -> io::Result<File> {
    let mut handle = null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: the root, name buffer and structures remain live throughout the
    // synchronous call. The options reject directory opens and prevent recall.
    let result = unsafe {
        NtOpenFile(
            &mut handle,
            access | SYNCHRONIZE,
            attributes,
            &mut status,
            SHARE,
            FILE_NON_DIRECTORY_FILE
                | FILE_OPEN_NO_RECALL
                | FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT,
        )
    };
    if result < 0 {
        // SAFETY: this converts an NTSTATUS value without dereferencing memory.
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32,
        ));
    }
    // SAFETY: the successful synchronous open returned a newly owned handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(super) fn stamp(file: &File) -> io::Result<Stamp> {
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: file owns a live handle, and basic is writable and correctly sized.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&raw mut basic).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let meta = file.metadata()?;
    if !meta.is_file() || (basic.FileAttributes | meta.file_attributes()) & UNSAFE_ATTRIBUTES != 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let (device, identity) = file_identity(file).ok_or(io::ErrorKind::Unsupported)?;
    Ok(Stamp {
        device,
        identity,
        len: meta.len(),
        mtime: filetime_to_unix(basic.LastWriteTime as u64),
        times: [basic.LastWriteTime, basic.ChangeTime, basic.CreationTime, 0],
    })
}
