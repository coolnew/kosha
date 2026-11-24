// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Allocated runs from a live Unix file descriptor. Device offsets stay private.

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Fragmentation;

const MAX_MAPPINGS: usize = 100_000;

fn unsupported(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, reason)
}

fn invalid_map() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or non-advancing extent mapping",
    )
}

fn interrupted(stop: &AtomicBool) -> io::Result<()> {
    if stop.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "extent check cancelled",
        ));
    }
    Ok(())
}

fn unchanged(a: &Metadata, b: &Metadata) -> bool {
    b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

#[cfg(target_os = "macos")]
fn check_file(meta: &Metadata) -> io::Result<()> {
    use std::os::macos::fs::MetadataExt as _;
    // SF_DATALESS and UF_COMPRESSED describe storage outside the ordinary
    // data-fork mapping. The event-only open also avoids opening cloud data.
    if meta.st_flags() & (0x4000_0000 | 0x20) != 0 {
        return Err(unsupported(
            "layout is unavailable for dataless or compressed files",
        ));
    }
    Ok(())
}

pub(super) fn fragmentation(path: &Path, stop: &AtomicBool) -> io::Result<Fragmentation> {
    interrupted(stop)?;
    let before = path.symlink_metadata()?;
    if !before.is_file() {
        return Err(unsupported("layout requires a regular file"));
    }

    #[cfg(target_os = "linux")]
    let flags = libc::O_NOFOLLOW | libc::O_NONBLOCK;
    #[cfg(target_os = "macos")]
    let flags = {
        check_file(&before)?;
        libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_EVTONLY
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)?;
    let opened = file.metadata()?;
    if !unchanged(&before, &opened) {
        return Err(io::Error::other(
            "file changed before its layout could be checked",
        ));
    }
    #[cfg(target_os = "macos")]
    check_file(&opened)?;
    let layout = read_map(&file, opened.len(), stop)?;
    let after = file.metadata()?;
    if !unchanged(&opened, &after) || !unchanged(&opened, &path.symlink_metadata()?) {
        return Err(io::Error::other(
            "file changed while its layout was checked",
        ));
    }
    Ok(layout)
}

#[derive(Default)]
struct Map {
    end: u64,
    previous_device_end: Option<u64>,
    mapped: u64,
    extents: u64,
    holes: u64,
}

impl Map {
    fn allocation(&mut self, logical: u64, device: u64, length: u64) -> io::Result<()> {
        if length == 0 || logical < self.end {
            return Err(invalid_map());
        }
        let gap = logical > self.end;
        self.holes += u64::from(gap);
        self.extents += u64::from(gap || self.previous_device_end != Some(device));
        self.end = logical.checked_add(length).ok_or_else(invalid_map)?;
        self.previous_device_end = Some(device.checked_add(length).ok_or_else(invalid_map)?);
        self.mapped = self.mapped.checked_add(length).ok_or_else(invalid_map)?;
        Ok(())
    }

    fn finish(self, len: u64, truncated: bool) -> Fragmentation {
        Fragmentation {
            extents: self.extents,
            mapped_bytes: Some(self.mapped),
            holes: self.holes + u64::from(!truncated && self.end < len),
            truncated,
            ..Fragmentation::default()
        }
    }
}

fn fs_stat(file: &File) -> io::Result<libc::statfs> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: the live descriptor and writable statfs structure match the API.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs successfully initialized the structure.
    Ok(unsafe { stat.assume_init() })
}

#[cfg(target_os = "linux")]
fn read_map(file: &File, len: u64, stop: &AtomicBool) -> io::Result<Fragmentation> {
    const BATCH: usize = 128;
    const LAST: u32 = 1;
    // Unknown, delayed allocation, encoded/encrypted, unaligned/inline/tail,
    // and merged block-based records cannot establish a comparable layout.
    const UNCERTAIN: u32 = 0x0002 | 0x0004 | 0x0008 | 0x0080 | 0x0100 | 0x0200 | 0x0400 | 0x1000;

    #[repr(C)]
    struct Header {
        start: u64,
        length: u64,
        flags: u32,
        mapped: u32,
        count: u32,
        reserved: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved64: [u64; 2],
        flags: u32,
        reserved: [u32; 3],
    }
    #[repr(C)]
    struct Request {
        header: Header,
        extents: [Extent; BATCH],
    }
    // Linux's ioctl encoding includes the fixed header size, even though the
    // caller provides room for the trailing array.
    let ioctl = libc::_IOWR::<Header>(b'f' as u32, 11);
    // EXT4_SUPER_MAGIC has the same value across the Linux statfs ABIs.
    if fs_stat(file)?.f_type != 0xef53 {
        return Err(unsupported("extent checks on Linux currently require ext4"));
    }

    let mut map = Map::default();
    let mut processed = 0;
    while map.end < len {
        interrupted(stop)?;
        let mut request = Request {
            header: Header {
                start: map.end,
                length: len - map.end,
                flags: 0, // Do not force writeback via FIEMAP_FLAG_SYNC.
                mapped: 0,
                count: BATCH as u32,
                reserved: 0,
            },
            extents: [Extent::default(); BATCH],
        };
        // SAFETY: repr(C) matches linux/fiemap.h. count bounds kernel writes.
        if unsafe { libc::ioctl(file.as_raw_fd(), ioctl, &raw mut request) } < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOTTY | libc::EOPNOTSUPP) => {
                    Err(unsupported("filesystem does not expose extent mappings"))
                }
                _ => Err(error),
            };
        }
        let records = request
            .extents
            .get(..request.header.mapped as usize)
            .ok_or_else(invalid_map)?;
        if records.is_empty() {
            break;
        }
        for extent in records {
            if extent.logical >= len {
                return Ok(map.finish(len, false));
            }
            if extent.flags & UNCERTAIN != 0 {
                return Err(unsupported(
                    "file has allocations with unknown or incompatible placement",
                ));
            }
            map.allocation(
                extent.logical,
                extent.physical,
                extent.length.min(len - extent.logical),
            )?;
            processed += 1;
            if extent.flags & LAST != 0 || map.end == len {
                return Ok(map.finish(len, false));
            }
            if processed >= MAX_MAPPINGS {
                return Ok(map.finish(len, true));
            }
        }
    }
    Ok(map.finish(len, false))
}

#[cfg(target_os = "macos")]
fn is_apfs(file: &File) -> io::Result<bool> {
    let stat = fs_stat(file)?;
    // SAFETY: fstatfs returns a null-terminated filesystem type name.
    Ok(unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) } == c"apfs")
}

#[cfg(target_os = "macos")]
fn read_map(file: &File, len: u64, stop: &AtomicBool) -> io::Result<Fragmentation> {
    // Darwin declares log2phys under #pragma pack(4).
    #[repr(C, packed(4))]
    struct Log2Phys {
        flags: u32,
        length: i64,
        offset: i64,
    }
    if !is_apfs(file)? {
        return Err(unsupported("extent checks on macOS currently require APFS"));
    }
    let len = i64::try_from(len).map_err(|_| invalid_map())?;
    let mut logical = 0;
    let mut map = Map::default();
    for _ in 0..MAX_MAPPINGS {
        if logical == len {
            return Ok(map.finish(len as u64, false));
        }
        interrupted(stop)?;
        let mut run = Log2Phys {
            flags: 0,
            length: len - logical,
            offset: logical,
        };
        // SAFETY: the file descriptor is live and the packed structure is the
        // input/output buffer specified by Darwin's fcntl.h.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_LOG2PHYS_EXT, &raw mut run) } < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOTSUP | libc::ENOTTY) => {
                    Err(unsupported("filesystem does not expose extent mappings"))
                }
                _ => Err(error),
            };
        }
        if run.length <= 0 || run.offset < -1 {
            return Err(invalid_map());
        }
        let length = run.length.min(len - logical);
        // -1 is the Darwin sentinel for a sparse interval without placement.
        // Leave the last placed end alone so adjacent hole intervals coalesce.
        if run.offset != -1 {
            map.allocation(logical as u64, run.offset as u64, length as u64)?;
        }
        logical += length;
    }
    Ok(map.finish(len as u64, logical < len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn adjacent_device_and_file_ranges_merge_independently_of_batch_boundaries() {
        let mut map = Map::default();
        map.allocation(0, 4096, 4096).unwrap();
        map.allocation(4096, 8192, 4096).unwrap();
        map.allocation(16384, 12288, 4096).unwrap();
        assert!(map.allocation(16384, 16384, 4096).is_err());
        let result = map.finish(24576, false);
        assert_eq!(
            (result.extents, result.holes, result.mapped_bytes),
            (2, 2, Some(12288))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apfs_clone_keeps_a_measurable_data_fork_after_copy_on_write() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let clone = dir.path().join("clone");
        let mut file = File::create(&original).unwrap();
        file.write_all(&[0x31; 65536]).unwrap();
        file.sync_all().unwrap();
        let stop = AtomicBool::new(false);
        match fragmentation(&original, &stop) {
            Ok(layout) => assert_eq!(layout.mapped_bytes, Some(65536)),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            Err(error) => panic!("APFS source could not be measured: {error}"),
        }
        let source = CString::new(original.as_os_str().as_bytes()).unwrap();
        let destination = CString::new(clone.as_os_str().as_bytes()).unwrap();
        // SAFETY: Both paths are live NUL-terminated strings in the test directory.
        assert_eq!(
            unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) },
            0
        );
        assert_eq!(
            fragmentation(&clone, &stop).unwrap().mapped_bytes,
            Some(65536)
        );
        let mut copy = OpenOptions::new().write(true).open(&clone).unwrap();
        copy.write_all(&[0x7a; 4096]).unwrap();
        copy.sync_all().unwrap();
        assert_eq!(
            fragmentation(&clone, &stop).unwrap().mapped_bytes,
            Some(65536)
        );
        assert_eq!(
            fragmentation(&original, &stop).unwrap().mapped_bytes,
            Some(65536)
        );
    }

    #[test]
    fn sparse_extent_fixture_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.bin");
        let mut file = File::create(&path).unwrap();
        let bytes = [0x5a; 65536];
        #[cfg(target_os = "linux")]
        let runs = 257;
        #[cfg(target_os = "macos")]
        let runs = 2;
        for index in 0..runs {
            file.seek(SeekFrom::Start(index * (1 << 20))).unwrap();
            file.write_all(&bytes).unwrap();
        }
        file.sync_all().unwrap();

        #[cfg(target_os = "macos")]
        if is_apfs(&file).unwrap() {
            // APFS may allocate the gap when the file is extended by a write.
            // Make the fixture sparse explicitly before checking its mapping.
            let mut hole = libc::fpunchhole_t {
                fp_flags: 0,
                reserved: 0,
                fp_offset: bytes.len() as i64,
                fp_length: (1 << 20) - bytes.len() as i64,
            };
            // SAFETY: this is the synthetic file and the documented writable
            // argument buffer for F_PUNCHHOLE.
            assert_eq!(
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &raw mut hole) },
                0
            );
            file.sync_all().unwrap();
            assert_eq!(
                file.metadata().unwrap().blocks() * 512,
                2 * bytes.len() as u64
            );
        }

        let stop = AtomicBool::new(true);
        assert_eq!(
            fragmentation(&path, &stop).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        stop.store(false, Ordering::Relaxed);
        assert_eq!(
            fragmentation(&link, &stop).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );

        match fragmentation(&path, &stop) {
            Ok(layout) => {
                assert_eq!(layout.extents, runs, "{layout:?}");
                assert_eq!(layout.holes, runs - 1, "{layout:?}");
                assert_eq!(layout.mapped_bytes, Some(runs * bytes.len() as u64));
                assert!(!layout.truncated);
            }
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                assert!(error.to_string().contains("currently require"), "{error}");
            }
            Err(error) => panic!("sparse extent reader failed: {error}"),
        }
    }
}
