// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Opening a live volume as somewhere `$MFT` can be read from.
//!
//! The only part of this strategy that touches a device, and the only part that
//! cannot be tested without one. Everything it produces — geometry, run lists,
//! records, the tree — is parsed by modules that take bytes and are under test
//! on every platform. This file is the seam and nothing else: it opens a
//! handle, reads at offsets, and hands the bytes to [`mft`](super::mft), which
//! does not know or care that they came from a device rather than a file.
//!
//! **Read-only, and structurally so.** The handle asks for `GENERIC_READ` and
//! nothing else, `kosha-core` contains no write path at any level, and a scan
//! running as administrator does not widen what an *action* may do — that still
//! requires a `ReadWriteGrant` from `kosha-fsops`, which an elevated scan has no
//! way to produce. Elevation and read-write are separate axes on purpose.
//!
//! **Unbuffered.** `FILE_FLAG_NO_BUFFERING` keeps a gigabyte of `$MFT` out of
//! the system file cache, which a metadata scan has no business filling with
//! bytes nobody will read again — the machine must not feel slower afterwards
//! than it did before. The price is that every read has to be aligned three
//! ways: the offset, the length, and the *buffer's address*. All three are
//! honoured by `mft`, which reads whole clusters out of a sector-aligned
//! buffer — so the requirement that only this file has is met by code that
//! runs everywhere, and costs a file nothing.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Component, Path};

use windows_sys::Win32::Foundation::{GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// An open NTFS volume.
pub struct Volume {
    file: File,
}

impl super::mft::Source for Volume {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buffer)
    }

    // `len` is left as `None`, which is "unknown" and means the volume's own
    // numbers are taken as read here.
    //
    // A device handle will usually answer a seek to its end, so the length could
    // be had. It is not asked for because the check it feeds exists for a *file*
    // — an image somebody was sent or a download that stopped early — and this
    // path is a live volume that took administrator rights to open, whose boot
    // sector Windows is not rewriting. Putting a number that has never been read
    // off a real device in front of an elevated scan would risk refusing the one
    // case this file exists to serve, to guard the one case it cannot be.
}

impl Volume {
    /// Open `\\.\C:`.
    ///
    /// Fails for a path with no drive letter and — the usual case — for a
    /// process without administrator rights, where `CreateFileW` refuses the
    /// device with `ERROR_ACCESS_DENIED` before anything here gets a chance to
    /// look at it.
    ///
    /// Whether the volume is NTFS is *not* decided here, though it used to be.
    /// That answer comes from the boot sector, reading the boot sector is the
    /// first thing [`Mft::open`](super::mft::Mft::open) does, and asking twice
    /// would mean two places that have to agree about what NTFS looks like.
    pub fn open(drive: &OsStr) -> io::Result<Volume> {
        let mut device = OsString::from(r"\\.\");
        device.push(drive);
        let wide = to_wide(&device);

        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
        // call; the security-attributes and template arguments are null, which
        // this call accepts. The returned handle is checked below and then
        // owned by a `File`, which closes it.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                // Share write as well as read: the volume is in use by the
                // running system, and refusing to share would mean refusing to
                // open. Measuring a disk must not disturb it.
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` is a fresh, valid handle that nothing else owns.
        let file = unsafe { File::from_raw_handle(handle.cast()) };
        Ok(Volume { file })
    }
}

/// The drive a path lives on, as `C:`, or `None` when it has no drive letter.
///
/// A UNC path, a verbatim-UNC path and a path on a volume mounted into a
/// directory all answer `None` here, and all of them are volumes this strategy
/// cannot open by letter — which is the same answer.
pub fn drive_of(path: &Path) -> Option<OsString> {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            std::path::Prefix::Disk(letter) | std::path::Prefix::VerbatimDisk(letter) => {
                Some(OsString::from(format!("{}:", letter as char)))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The MFT record number of a path, so a scan of a subdirectory can start
/// somewhere other than the volume's root.
///
/// The low 48 bits of the file index are the record number; the top 16 are a
/// sequence number that counts how often the record has been reused, and are
/// not part of the address.
pub fn record_of(path: &Path) -> Option<u32> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        GetFileInformationByHandle,
    };

    /// Metadata only: no data, no directory listing.
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;

    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is valid and kept alive by `file` across the call, and
    // `info` is a live, zeroed, correctly aligned struct for the API to fill.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &raw mut info) };
    if ok == 0 {
        return None;
    }
    let index = u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow);
    u32::try_from(index & 0x0000_FFFF_FFFF_FFFF).ok()
}

fn to_wide(s: &OsStr) -> Vec<u16> {
    let mut wide: Vec<u16> = s.encode_wide().collect();
    wide.push(0);
    wide
}
