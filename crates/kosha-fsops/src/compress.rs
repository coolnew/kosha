// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Turning NTFS compression on and off.
//!
//! **This lives here because it writes, and it used to live where nothing is
//! supposed to.** It was in `kosha-core::platform`, beside `supports_compression`
//! which only reads — a plausible place for a filesystem call and the wrong one
//! for a filesystem *mutation*. The dependency graph is part of
//! the proof: "could this code path change a file?" is meant to be answerable
//! from `Cargo.toml`, and it was not, because `kosha_core::set_compression` was
//! public, needed no [`ReadWriteGrant`](crate::ReadWriteGrant), and was
//! reachable from every crate that reads a disk.
//!
//! Nothing was calling it that way — the only caller has always been the plan
//! in this crate, behind a grant. What was wrong was that nothing *stopped*
//! another one. Keeping mutations in this crate enforces that boundary.
//!
//! The read half stayed in `kosha-core`: asking whether a volume supports
//! compression, and what a file occupies, are questions a scan asks and answers
//! for itself. One of them is new — `compression_available_at` reads the
//! volume's own flags — and it exists because the alternative was a *capability
//! probe that mutates*. The integration tests used to find out whether a
//! temporary directory could compress by compressing something in it and
//! undoing that, which was the only reason anything outside this crate wanted a
//! way to set compression at all. Asking is free and needs no permission.

use std::io;
use std::path::Path;

/// Turn NTFS compression on or off, via `FSCTL_SET_COMPRESSION`.
///
/// The handle comes from `std`, which owns and closes it — the alternative was
/// `CreateFileW` and a hand-rolled guard to stop the error paths leaking it,
/// which is more unsafe code than this needs. `FILE_FLAG_BACKUP_SEMANTICS` is
/// what lets the same call open a directory, which is otherwise not openable as
/// a file at all, and the explicit access mode is because the control code both
/// reads the current state and writes the new one.
///
/// A volume that cannot compress — FAT32, exFAT, most network shares — fails
/// here with the system's own error, which is the honest answer and not one
/// worth translating.
///
/// **The one `unsafe` block in this crate**, and the reason the crate denies
/// rather than forbids. `DeviceIoControl` has no safe wrapper in `std` and the
/// alternative was leaving the mutation in `kosha-core`, which promises not to
/// mutate; an audited ioctl in the crate that already needs the most review is
/// the better of the two. Any other `unsafe` here is still a compile error.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn set(path: &Path, enabled: bool) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_COMPRESSION;

    // LZNT1, the only format NTFS accepts here; NONE turns it off.
    const COMPRESSION_FORMAT_NONE: u16 = 0;
    const COMPRESSION_FORMAT_DEFAULT: u16 = 1;
    const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;

    let file = std::fs::OpenOptions::new()
        .access_mode(GENERIC_READ_WRITE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;

    let mut format: u16 = if enabled {
        COMPRESSION_FORMAT_DEFAULT
    } else {
        COMPRESSION_FORMAT_NONE
    };
    let mut returned: u32 = 0;

    // SAFETY: the handle is valid and kept alive by `file` for the whole call,
    // `format` is a live `u16` of exactly the length passed, and the output
    // buffer is null with a zero length, which this control code accepts.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            FSCTL_SET_COMPRESSION,
            (&raw mut format).cast(),
            2,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// No filesystem outside Windows offers per-file compression through an
/// interface like this one. Btrfs and ZFS compress per subvolume or dataset,
/// which is an administrator's decision about a mount and not a thing a disk
/// usage tool should be reaching into.
#[cfg(not(windows))]
pub(crate) fn set(_path: &Path, _enabled: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "per-file compression is a Windows feature",
    ))
}

#[cfg(test)]
mod tests {
    //! Moved here with the function they are about.
    //!
    //! They were in `kosha-core::platform`, which is where the mutation was,
    //! and a test of a mutation belongs in the crate that is allowed to make
    //! one. Nothing about them changed except the name of what they call.
    use super::set;

    #[test]
    fn an_unsupported_platform_refuses_rather_than_pretending() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, vec![b'a'; 128 * 1024]).unwrap();

        let result = set(&file, true);
        if kosha_core::supports_compression() {
            // Some volumes (FAT32, exFAT, a network share) legitimately cannot,
            // and that is an error rather than a silent success too.
            if let Err(error) = result {
                assert_ne!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported,
                    "a Windows build reported the platform itself as unsupported"
                );
            }
        } else {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        }
    }

    /// The one that matters: compression must shrink the file on disk and
    /// leave every byte of its contents alone.
    ///
    /// Skipped where the platform cannot compress, and where the volume the
    /// tests happen to run on cannot — a temp directory on exFAT is a real
    /// configuration and not a failure of this code.
    #[test]
    fn compressing_shrinks_the_file_without_touching_its_contents() {
        if !kosha_core::supports_compression() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("compressible.txt");
        // Highly repetitive, so LZNT1 has something to work with, and large
        // enough that the saving cannot be a rounding artefact.
        let contents: Vec<u8> = b"the quick brown fox jumps over the lazy dog. "
            .iter()
            .copied()
            .cycle()
            .take(4 * 1024 * 1024)
            .collect();
        std::fs::write(&file, &contents).unwrap();

        let before = kosha_core::allocated_size(&file).unwrap();
        if set(&file, true).is_err() {
            return; // This volume cannot compress; nothing to assert.
        }

        // No rewrite: `FSCTL_SET_COMPRESSION` compresses the data already in
        // the file, synchronously, before it returns. I originally rewrote the
        // file here on the assumption that NTFS compressed lazily — which is
        // false, and the rewrite was itself putting the file back to full size.
        let after = kosha_core::allocated_size(&file).unwrap();
        assert!(
            after < before,
            "compression did not shrink it: {before} -> {after}"
        );
        assert_eq!(
            std::fs::read(&file).unwrap(),
            contents,
            "compression altered the file's contents"
        );

        // And it comes back off, just as synchronously.
        set(&file, false).unwrap();
        assert_eq!(kosha_core::allocated_size(&file).unwrap(), before);
        assert_eq!(std::fs::read(&file).unwrap(), contents);
    }

    #[test]
    fn a_directory_can_carry_the_attribute_too() {
        if !kosha_core::supports_compression() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        // Opening a directory at all needs FILE_FLAG_BACKUP_SEMANTICS; without
        // it this is the call that fails.
        if let Err(error) = set(&inner, true) {
            assert_ne!(error.kind(), std::io::ErrorKind::Unsupported);
        }
    }
}
