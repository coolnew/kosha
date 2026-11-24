// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Windows metadata.
//!
//! The cost profile is the mirror image of Unix. `FindFirstFileW` returns the
//! full `WIN32_FIND_DATA` for every entry, and Rust caches it, so
//! `DirEntry::metadata()` is *free* — no syscall, no handle. A Windows scan is
//! therefore bounded almost entirely by directory enumeration itself.
//!
//! The exception is the true on-disk size. Getting it means opening the file,
//! which turns a free scan into one syscall per entry and costs roughly an
//! order of magnitude. So the default derives on-disk size by rounding the
//! logical size up to the volume's allocation unit — right for the ordinary
//! case, and wrong for compressed and sparse files, which we flag from the
//! attribute bits so the UI can say so, and for a file small enough to live
//! inside its MFT record, which occupies no cluster at all and which no
//! rounding can express. `exact_allocated` opts into the slow, precise path.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::Security::GetFileSecurityW;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, GetDiskFreeSpaceExW, GetDiskFreeSpaceW,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetVolumeInformationW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;
use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

use super::sid;
use super::{DiskUsage, EntryMeta, Fragmentation, MediaType, PlatformCtx};
use crate::ntfs::record::filetime_to_unix;
use crate::owner::{OwnerId, OwnerKind};

// Declared locally rather than imported so this file does not churn with
// windows-sys feature reshuffles. These values are stable ABI.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0000_0200;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;
/// Set on tape- and cloud-backed files whose data has been moved elsewhere.
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
/// The modern cloud-placeholder markers. `RECALL_ON_OPEN` means opening the
/// file fetches it; `RECALL_ON_DATA_ACCESS` means reading its bytes does.
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// Every attribute that means "the bytes are not here".
const NOT_RESIDENT: u32 =
    FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
/// The weakest access right there is: metadata only, no data, no directory
/// listing. Enough for both handle queries here and nothing more.
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// Open the link itself rather than whatever it points at.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_OPEN_NO_RECALL: u32 = 0x0010_0000;

/// Round a logical size up to a whole number of allocation units.
///
/// Lives here rather than beside the shared types because only this platform
/// needs it: Unix gets a file's true on-disk size from `st_blocks` as part of
/// the stat it already does, so an estimate would be a worse answer to a
/// question already answered.
#[inline]
fn round_to_cluster(logical: u64, cluster: u64) -> u64 {
    if logical == 0 || cluster == 0 {
        return 0;
    }
    logical.div_ceil(cluster) * cluster
}

/// Windows has no cheap `st_dev`, and does not need one for the scan root:
/// zero here means "the device everything is on unless it says otherwise", and
/// the only entries that say otherwise are mounted volumes, which `entry_meta`
/// gives their real serial. See `mounted_volume`.
#[inline]
pub(super) fn device_of(_md: &fs::Metadata) -> u64 {
    0
}

/// Ask the volume for its allocation unit: sectors-per-cluster ×
/// bytes-per-sector.
pub(super) fn cluster_size(root: &Path) -> Option<u64> {
    let volume = volume_root(root)?;
    let wide = to_wide(volume.as_os_str());

    let mut sectors_per_cluster: u32 = 0;
    let mut bytes_per_sector: u32 = 0;
    let mut free_clusters: u32 = 0;
    let mut total_clusters: u32 = 0;

    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call,
    // and the four out-parameters are live, correctly typed and initialised.
    let ok = unsafe {
        GetDiskFreeSpaceW(
            wide.as_ptr(),
            &mut sectors_per_cluster,
            &mut bytes_per_sector,
            &mut free_clusters,
            &mut total_clusters,
        )
    };
    if ok == 0 {
        return None;
    }

    let size = u64::from(sectors_per_cluster) * u64::from(bytes_per_sector);
    (size > 0).then_some(size)
}

#[inline]
pub(super) fn entry_meta(
    entry: &fs::DirEntry,
    file_type: fs::FileType,
    _dir_path: &Path,
    ctx: &PlatformCtx,
) -> io::Result<EntryMeta> {
    // Free on Windows: served from the cached WIN32_FIND_DATA.
    let md = entry.metadata()?;
    let mut meta = from_metadata(&md, file_type, ctx);

    // Sparse and compressed files are asked about even when `--exact` was not,
    // because for *them* the estimate is not an approximation — it is a number
    // we already know to be wrong, and it is wrong in the direction that hides
    // the saving. The cost is bounded by how many such files exist, which in an
    // ordinary tree is none and in the trees where it matters is exactly the
    // files being asked about. Without this, "already saved by sparse files"
    // could never appear on Windows, while Linux reports it from `st_blocks`
    // for free — a difference in what the product *says*, from a difference in
    // what a syscall costs.
    let wants_allocation = ctx.exact_allocated || meta.is_sparse;

    // Both answers below come from an open handle, so one open serves both:
    // `--exact --hardlinks` is one `CreateFile` per entry rather than two.
    //
    // Never on a directory, which has neither an allocation worth reporting nor
    // links to share. Never on a reparse point for the identity, which is a
    // link counted as itself rather than a file that might be shared. And never
    // on a cloud placeholder at all: its bytes are not on this disk, so there
    // is nothing to measure and nothing to count twice, and opening one marked
    // RECALL_ON_OPEN downloads it — the exact outcome `is_offline` exists to
    // prevent. That exclusion is why `is_sparse` alone is not enough to trigger
    // an open: an offline file carries it too.
    let wants_identity = ctx.dedup_hardlinks && !meta.is_symlink;
    // Never on a symlink or a junction either, for the allocation as well as
    // the identity. `open_for_metadata` does not pass
    // `FILE_FLAG_OPEN_REPARSE_POINT`, so it opens whatever the link points at —
    // and `--exact` was therefore reporting the *target's* size on disk as the
    // link's. A symlink to a 1.9 MB DLL came back as 1.9 MB while the default
    // walk and the MFT both said zero, which is the whole "measured, never
    // followed" rule quietly failing on one code path. Found by diffing the two
    // strategies against each other over `C:\Program Files`, where Office's
    // App-V shims are 141 such links.
    if !meta.is_dir && !meta.is_offline && !meta.is_symlink && (wants_allocation || wants_identity)
    {
        // `DirEntry::path` joins and allocates, so it is built only here.
        if let Some(handle) = open_for_metadata(&entry.path())
            && let Some(info) = standard_info(&handle)
        {
            // One query answers both questions: `FILE_STANDARD_INFO` carries
            // the allocation *and* the link count, so `--exact --hardlinks` is
            // one `CreateFile` and one query per entry rather than two queries.
            if wants_allocation && let Ok(exact) = u64::try_from(info.AllocationSize) {
                meta.allocated = exact;
            }
            // Only a file with more than one name can collide with anything, so
            // the identity set stays as small as it is on Unix — and the second
            // query is paid only by the files that could need it.
            if wants_identity && info.NumberOfLinks > 1 {
                meta.link_key = file_identity(&handle);
            }
        }
    }

    // One more call per entry, and the reason the setting is off by default
    // here and free on Unix. Asked of directories too: on a shared machine a
    // directory somebody else made inside a shared folder is part of the
    // answer, and it is the only entry whose own bytes the tree would
    // otherwise attribute to nobody.
    if ctx.collect_owners {
        meta.owner = owner_of(&entry.path(), meta.is_symlink);
    }

    // A reparse-point directory might be a mounted volume rather than a
    // junction, and the two want opposite treatment. Asking costs an open and
    // an ioctl, which is why it is asked only of reparse points — a handful in
    // a tree, and none at all in most.
    if meta.is_dir
        && meta.is_symlink
        && let Some(serial) = mounted_volume(&entry.path())
    {
        // Reported as a differing device, which is exactly what it is, and
        // which puts Windows on the same code path Unix has always used:
        // `OTHER_DEVICE` and no descent by default, `cross_filesystems` to go
        // in anyway. It stops being a "link", because calling somebody's
        // mounted 2 TB drive a link is a worse answer than calling it another
        // filesystem.
        meta.is_symlink = false;
        meta.mounted_volume = Some(serial);
    }

    Ok(meta)
}

pub(super) fn root_meta(root: &Path, ctx: &PlatformCtx) -> io::Result<EntryMeta> {
    let md = fs::symlink_metadata(root)?;
    let ft = md.file_type();
    let mut meta = from_metadata(&md, ft, ctx);
    if ctx.collect_owners {
        meta.owner = owner_of(root, meta.is_symlink);
    }
    Ok(meta)
}

/// Whether the file at `path` is a cloud placeholder, from its attributes alone.
pub(super) fn is_placeholder(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|md| {
        let attrs = md.file_attributes();
        attrs & FILE_ATTRIBUTE_DIRECTORY == 0 && attrs & NOT_RESIDENT != 0
    })
}

fn from_metadata(md: &fs::Metadata, file_type: fs::FileType, ctx: &PlatformCtx) -> EntryMeta {
    let attrs = md.file_attributes();
    let is_dir = file_type.is_dir() || attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
    let logical = md.file_size();

    // A OneDrive placeholder reports its full length while occupying almost
    // nothing, so it belongs with the other files whose length is an upper
    // bound rather than a measurement.
    let is_offline = !is_dir && attrs & NOT_RESIDENT != 0;

    // Sparse, compressed and cloud-backed files really do occupy less than
    // their length, and rounding up would overstate them badly. Without an
    // exact query the honest answer is "we do not know", so we report the
    // logical size and flag it; the UI shows the caveat rather than a confident
    // wrong total. `--exact` asks the filesystem and gets the truth.
    let unusual =
        attrs & (FILE_ATTRIBUTE_SPARSE_FILE | FILE_ATTRIBUTE_COMPRESSED) != 0 || is_offline;
    let allocated = if is_dir {
        0
    } else if unusual {
        logical
    } else {
        round_to_cluster(logical, ctx.cluster_size)
    };

    EntryMeta {
        logical,
        allocated,
        mtime: filetime_to_unix(md.last_write_time()),
        created: md
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
        device: 0,
        // Filled in afterwards by `entry_meta`, which has the path this needs.
        mounted_volume: None,
        // Filled in afterwards by `entry_meta` when the setting is on: it
        // needs the path, which the cached metadata alone does not carry.
        link_key: None,
        is_dir,
        // Junctions, symlinks and volume mount points are all reparse points,
        // and none of them should be descended into or counted twice.
        // A cloud placeholder is a reparse point, but it is a *file* we should
        // still report rather than a link to skip past — the symlink rule is
        // about not following, and there is nothing here to follow.
        is_symlink: (file_type.is_symlink() || attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            && !is_offline,
        is_sparse: unusual,
        is_offline,
        // Filled in afterwards by `entry_meta`, which has the path it needs.
        // Nothing in the cached `WIN32_FIND_DATA` says who owns a file.
        owner: None,
    }
}

/// Open a file for metadata and nothing else.
///
/// `FILE_READ_ATTRIBUTES` alone, with every share mode, because measuring a
/// disk must not disturb it: this has to succeed on a file somebody else has
/// open, and must not stop them opening, writing or deleting it while we look.
/// `FILE_FLAG_BACKUP_SEMANTICS` is harmless for a file and lets the same call
/// work if a directory ever reaches it.
///
/// `None` rather than an error: a file we cannot open is one we report from the
/// cheap metadata we already have, which is the same answer the default gives.
fn open_for_metadata(path: &Path) -> Option<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()
}

/// File identity, from an open handle.
///
/// Windows has no `st_ino`. Two paths are the same file when the volume serial
/// and the file id match, and the only way to learn either is to ask an open
/// handle. That is one `CreateFile` per entry — precisely the syscall this
/// platform's fast path exists to avoid, and why the setting is off by default
/// here and free on Unix.
///
/// **The id is 128 bits, and no 64 of them will do.**
/// `BY_HANDLE_FILE_INFORMATION`'s `nFileIndexHigh`/`nFileIndexLow` is exact on
/// NTFS, where it is the MFT record number and sequence, and is documented as
/// not guaranteed unique anywhere else. Measured here on NTFS and on a ReFS
/// volume:
///
/// ```text
/// file                  64-bit index        id low half         id high half
/// notepad.exe (NTFS)  0x00010000000da67a  0x00010000000da67a  0x0000000000000000
/// conpty.dll  (ReFS)  0x3000000000004703  0x0000000000000004  0x0000000000000703
/// odin-readme.md      0x3000000000004704  0x0000000000000004  0x0000000000000704
/// ```
///
/// On NTFS the short form *is* the low half and the high half is zero, so this
/// changes nothing there. On ReFS the id is two halves that mean different
/// things, and **neither half identifies a file on its own**: `conpty.dll` and
/// `odin-readme.md` are unrelated files sharing a low half, and five of ten
/// files sampled shared a high half. Keying on either would mark the second of
/// a pair `HARDLINK_DUP`, contributing zero and silently under-reporting the
/// disk.
///
/// ReFS is reachable: it is the default for Storage Spaces and for Dev Drives,
/// and a `--hardlinks` scan of one is an ordinary thing to do.
///
/// Stated honestly: the short form was *not* observed to collide on the ReFS
/// volume here, and a real hardlink pair on it reported one short id as well as
/// one long one, so the previous code would have answered this machine's test
/// correctly. This is correctness by construction rather than a caught failure
/// — the short form is a lossy derivation that the documentation declines to
/// guarantee, the cost of using the whole id is nothing, and the failure it
/// avoids is silent under-reporting that no test on a quiet disk would show.
///
/// `None` when the query fails: a file we cannot identify is counted in full,
/// which overstates rather than silently loses bytes. The caller asks only
/// about files with more than one link, matching Unix's `nlink() > 1`, so the
/// scanner's set only ever holds entries that could actually collide.
///
/// Reported by `marg`, which lifted this layer and measured the divergence.
fn file_identity(file: &fs::File) -> Option<(u64, u128)> {
    use std::os::windows::io::AsRawHandle;

    /// `FileIdInfo` in `FILE_INFO_BY_HANDLE_CLASS`.
    const FILE_ID_INFO_CLASS: i32 = 18;

    let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is valid and kept alive by `file` across the call;
    // `info` is a live, zeroed, correctly aligned `FILE_ID_INFO`, and the length
    // passed is its own size, which is what the class expects.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FILE_ID_INFO_CLASS,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    // The number is only ever compared, never shown, so all that matters is
    // that one file always produces one value and two files rarely collide —
    // which is exactly what the full width buys.
    Some((
        info.VolumeSerialNumber,
        u128::from_le_bytes(info.FileId.Identifier),
    ))
}

/// Whether a reparse point is a mounted volume, and which one.
///
/// A volume mount point and a junction are the *same* reparse tag —
/// `IO_REPARSE_TAG_MOUNT_POINT` — and differ only in where they point. A
/// junction names a directory, `\??\C:\some\path`; a mounted volume names a
/// volume, `\??\Volume{xxxxxxxx-...}\`. There is no flag for it and no cheaper
/// question to ask.
///
/// That distinction is the whole reason this exists. Both must be refused by
/// default, because following either can loop and both cross into storage the
/// scan was not pointed at — but only one of them is a *filesystem boundary*,
/// and telling a user their mounted 2 TB drive is "a link" is a different and
/// worse answer than telling them it is another filesystem.
///
/// Returns the volume's serial number, which is what makes the scanner treat it
/// exactly as Unix treats a differing `st_dev`. `None` for a junction, a
/// symlink, or anything that is not a reparse point at all.
fn mounted_volume(path: &Path) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

    // `FILE_FLAG_OPEN_REPARSE_POINT` is what opens the link rather than its
    // target. Without it this would ask the *mounted volume's root* about
    // itself, which succeeds and answers the wrong question.
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;

    let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut returned: u32 = 0;
    // SAFETY: the handle is valid and kept alive by `file`; the output buffer is
    // a live allocation of exactly the length passed, and `returned` is a live
    // out-parameter. The control code reads nothing from the input buffer, so a
    // null input of length zero is what it expects.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null_mut(),
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    buffer.truncate(returned as usize);

    if !super::reparse::targets_a_volume(&buffer, super::reparse::MOUNT_POINT_TAG) {
        return None;
    }

    // The serial of the volume mounted here, read through the reparse point's
    // *target* — so this open deliberately does not pass
    // `FILE_FLAG_OPEN_REPARSE_POINT`.
    let root = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: as `link_identity`, on a handle this function owns.
    let ok = unsafe { GetFileInformationByHandle(root.as_raw_handle() as HANDLE, &raw mut info) };
    if ok == 0 {
        return None;
    }
    // Zero would read as "no device" to the scanner, and a volume serial of
    // zero is not impossible. One is not a serial anybody has, and any nonzero
    // constant does the job: the number is only ever compared, never shown.
    Some(u64::from(info.dwVolumeSerialNumber).max(1))
}

/// Bytes this file actually occupies, from an open handle.
///
/// `FILE_STANDARD_INFO::AllocationSize`, which is the number Explorer shows as
/// "Size on disk". **Not** `GetCompressedFileSizeW`, which this used to call and
/// which is not an allocation query at all: its documentation says that when
/// the file is neither compressed nor sparse "the value obtained is the actual
/// file size", and that is exactly what it returned — so `--exact` reported the
/// apparent size as the exact on-disk size, which is not merely imprecise but a
/// different quantity, and further from the truth than the cluster estimate it
/// replaced. Measured on NTFS with 4 KiB clusters:
///
/// ```text
/// file             apparent   GetCompressedFileSize   AllocationSize
/// ordinary.bin      127,387                 127,387          131,072
/// resident.txt          490                     490              496
/// compressed.bin  1,800,000                 225,280          225,280
/// ```
///
/// They agree only where the old call happened to be doing its actual job. The
/// 496 is a file small enough to live inside its MFT record and occupy no
/// cluster at all, which no estimate can express and which the rounded default
/// reports as a whole 4 KiB.
fn allocation_of(file: &fs::File) -> Option<u64> {
    u64::try_from(standard_info(file)?.AllocationSize).ok()
}

/// The allocation and the link count, which arrive in the same struct.
///
/// Asked once rather than twice: the exact-size path wants `AllocationSize` and
/// the hardlink path wants `NumberOfLinks`, so a scan with both settings on
/// costs one `CreateFile` and one query per entry rather than two of each.
fn standard_info(file: &fs::File) -> Option<FILE_STANDARD_INFO> {
    use std::os::windows::io::AsRawHandle;

    /// `FileStandardInfo` in `FILE_INFO_BY_HANDLE_CLASS`.
    const FILE_STANDARD_INFO_CLASS: i32 = 1;

    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is valid and kept alive by `file`; `info` is a live,
    // zeroed, correctly aligned `FILE_STANDARD_INFO`, and the length passed is
    // its own size, which is what the class expects.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FILE_STANDARD_INFO_CLASS,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    (ok != 0).then_some(info)
}

/// Volume capacity and free space, via `GetDiskFreeSpaceExW`.
///
/// The API distinguishes what is free on the volume from what this user is
/// allowed to use, and we keep both: under a disk quota they differ, and the
/// honest bar is drawn from the volume figure while the honest "you can still
/// write this much" is the caller's.
pub fn disk_usage(path: &Path) -> Option<DiskUsage> {
    // The scan root, which is a path somebody typed and can be any length.
    let wide = to_wide_path(path);
    let mut available: u64 = 0;
    let mut total: u64 = 0;
    let mut free: u64 = 0;

    // SAFETY: `wide` is NUL-terminated and outlives the call; the three outputs
    // are valid, initialised, correctly aligned u64s.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut available,
            &raw mut total,
            &raw mut free,
        )
    };
    if ok == 0 || total == 0 {
        return None;
    }
    Some(DiskUsage {
        total,
        free,
        available,
    })
}

/// Bytes this file occupies on disk, from the filesystem itself.
pub fn allocated_size(path: &Path) -> io::Result<u64> {
    let file = open_for_metadata(path).ok_or_else(io::Error::last_os_error)?;
    allocation_of(&file).ok_or_else(io::Error::last_os_error)
}

/// `D:\Projects\kosha` -> `D:\`. Returns `None` for UNC and verbatim paths,
/// where the caller's 4 KiB default is the right answer anyway.
fn volume_root(path: &Path) -> Option<std::path::PathBuf> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };
    let mut root = std::path::PathBuf::from(prefix.as_os_str());
    root.push("\\");
    Some(root)
}

/// Whether the volume under `path` can compress a file, asked rather than tried.
///
/// **A read, which is the whole point of it.** The question "can this be
/// compressed" used to be answered by compressing something and seeing whether
/// it worked, which means a capability probe that mutates — fine in a test
/// against a temporary file, and exactly the sort of thing that then wants a
/// mutating API where there should not be one. `GetVolumeInformationW` hands
/// back the filesystem's own flags, and `FILE_FILE_COMPRESSION` is one of them.
///
/// False where the volume cannot be asked at all: a path with no drive prefix,
/// a UNC share, a handle-only path. A caller that goes on to try anyway gets
/// the system's error, which is the honest answer and is what it would have got
/// from the probe.
pub fn compression_available_at(path: &Path) -> bool {
    /// `FILE_FILE_COMPRESSION`: the filesystem supports per-file compression.
    const FILE_FILE_COMPRESSION: u32 = 0x0000_0010;

    let Some(root) = volume_root(path) else {
        return false;
    };
    let wide = to_wide(root.as_os_str());
    let mut flags: u32 = 0;

    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call.
    // Every output buffer this does not want is null with a zero length, which
    // the call accepts; `flags` is a live `u32`.
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    ok != 0 && flags & FILE_FILE_COMPRESSION != 0
}

fn to_wide(s: &OsStr) -> Vec<u16> {
    let mut wide: Vec<u16> = s.encode_wide().collect();
    wide.push(0);
    wide
}

/// A path for a Win32 call, in the form that has no length limit.
///
/// **`MAX_PATH` is 260 and a real directory tree is not.** `node_modules`,
/// nested build outputs and anything unpacked from a deep archive go past it
/// routinely, and a Win32 call given a longer path fails with
/// `ERROR_PATH_NOT_FOUND` however much it exists. `std` knows this and does it
/// for every path it opens, which is why the scan's sizes are right on such a
/// tree; the calls in this file that take a *path* rather than a handle did
/// not, and the one in the walk is `GetFileSecurityW`. Measured on a tree
/// 306 characters deep: the file and the two directories past the limit came
/// back as `(owner unreadable)` while the five above them resolved.
///
/// An account we cannot name must be distinguished from nobody at all,
/// and this was neither: a lookup that failed for the length of the path,
/// reported as though the descriptor could not be read.
///
/// **`\\?\` turns off every other kind of path parsing too** — no `.`, no
/// `..`, no forward slashes, no trailing-dot trimming — so it may only be put
/// in front of a fully-qualified path. Anything else is handed over unchanged,
/// which is the same answer as before for the cases that cannot be long. The
/// counterpart is [`crate::path::simplify`], which takes the prefix off again
/// for anything a person will read.
fn to_wide_path(path: &Path) -> Vec<u16> {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let prefixed = match components.next() {
        // `C:\…`, and only with the root: `C:relative` names a different file
        // depending on the process' current directory on that drive, and the
        // verbatim form has no way to say that.
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)) => {
            matches!(components.next(), Some(Component::RootDir))
        }
        // `\\server\share\…` becomes `\\?\UNC\server\share\…`.
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(..)) => true,
        // Already verbatim, a device path, or relative. Untouched.
        _ => false,
    };
    if !prefixed {
        return to_wide(path.as_os_str());
    }

    let unc = matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(..))
    );
    let mut wide: Vec<u16> = Vec::with_capacity(path.as_os_str().len() + 8);
    if unc {
        // One of the two leading backslashes is the `\\` of `\\?\UNC\`.
        wide.extend(r"\\?\UNC".encode_utf16());
        wide.extend(path.as_os_str().encode_wide().skip(1));
    } else {
        wide.extend(r"\\?\".encode_utf16());
        wide.extend(path.as_os_str().encode_wide());
    }
    wide.push(0);
    wide
}

/// Whether this machine lets a Win32 path exceed `MAX_PATH` without the
/// `\\?\` prefix.
///
/// **Not a question about this program.** `to_wide_path` puts the prefix on
/// every path *kosha* hands to Win32, so kosha's own reading does not depend on
/// this. What depends on it is what happens when kosha asks *Windows* to do
/// something by path — and the one place that matters is the Recycle Bin, which
/// the shell operates through `IFileOperation` rather than through this process.
///
/// Measured, because the difference is not academic and not visible from inside:
/// on a machine with this on, a file 325 characters deep goes to the Recycle Bin
/// and can be restored from it. On a clean Windows, where the default is off, the
/// same call reports success and the file is **gone** — nothing of it under
/// `$Recycle.Bin`, nothing in the shell's listing. `kosha-fsops` refuses to offer
/// that as a reversible delete; see its `guard`.
///
/// `false` where the value cannot be read, which is the safe direction: it means
/// a deep path is treated as un-trashable rather than trashed into nowhere.
pub fn long_paths_enabled() -> bool {
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
    };

    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
    let subkey = wide(r"SYSTEM\CurrentControlSet\Control\FileSystem");
    let name = wide("LongPathsEnabled");
    let mut value = 0u32;
    let mut len = size_of::<u32>() as u32;

    // SAFETY: both strings are NUL-terminated and outlive the call; `value` and
    // `len` are a matched pair in bytes, and the key is a constant handle.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut value).cast(),
            &raw mut len,
        )
    };
    status == 0 && value != 0
}

/// Whether this process is running elevated.
///
/// Not "is the user an administrator": on a machine with UAC on, the
/// interactive administrator runs with a *filtered* token that has the group
/// but not the rights, and every raw volume open fails exactly as it would for
/// a standard user. `TokenElevation` asks the question that predicts the
/// outcome — is the token in this process the full one — rather than the one
/// that predicts what a second, elevated process could do.
pub fn elevated() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
    // closing, and `token` is a live out-parameter of the right type.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return false;
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned: u32 = 0;
    // SAFETY: `token` is a handle this function owns and closes below;
    // `elevation` is a live, correctly typed struct of exactly the length
    // passed, and `returned` is a live out-parameter.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            (&raw mut elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut returned,
        )
    };
    // SAFETY: `token` came from `OpenProcessToken` above and is not used again.
    unsafe { CloseHandle(token) };

    ok != 0 && elevation.TokenIsElevated != 0
}

/// `TokenElevation` in `TOKEN_INFORMATION_CLASS`.
///
/// Spelled as its number for the same reason the attribute bits above are: so
/// this file does not churn with windows-sys feature reshuffles. The value is
/// stable ABI.
const TOKEN_ELEVATION_CLASS: i32 = 20;

/// Ask the token for `SeBackupPrivilege`, and say whether it was granted.
///
/// This is the part of an elevated scan that is easy to miss. Administrator
/// membership gets a handle on `\.\C:`; the privilege is what makes the reads
/// through it bypass the ACL checks that would otherwise apply. It is present
/// but *disabled* in an elevated token — every privilege is — so it has to be
/// asked for explicitly, once, before the volume is opened.
///
/// Returns false for an unelevated process, where the privilege is not in the
/// token at all and `AdjustTokenPrivileges` succeeds while enabling nothing:
/// the call reports success and sets `ERROR_NOT_ALL_ASSIGNED`, which is the
/// one case a naive check reads as "it worked".
pub fn enable_backup_privilege() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let name = to_wide(OsStr::new("SeBackupPrivilege"));

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: as `elevated`, with the access this call actually needs.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut token,
        )
    } == 0
    {
        return false;
    }

    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: unsafe { std::mem::zeroed() },
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };

    // SAFETY: `name` is NUL-terminated and outlives the call; the LUID is a
    // live out-parameter inside a struct this function owns. A null system name
    // means the local machine, which is the only one we ask about.
    let found = unsafe {
        LookupPrivilegeValueW(
            std::ptr::null(),
            name.as_ptr(),
            &raw mut privileges.Privileges[0].Luid,
        )
    };

    let enabled = if found == 0 {
        false
    } else {
        // SAFETY: `token` is owned here, `privileges` is a live, fully
        // initialised `TOKEN_PRIVILEGES` with the count its array holds, and
        // the three output parameters are deliberately null — we do not need
        // the previous state back.
        let ok = unsafe {
            AdjustTokenPrivileges(
                token,
                0,
                &raw const privileges,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // SAFETY: reads a thread-local error code set by the call above.
        ok != 0 && unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED
    };

    // SAFETY: `token` came from `OpenProcessToken` above and is not used again.
    unsafe { CloseHandle(token) };
    enabled
}

/// How many pieces a file's bytes are in on the disk.
///
/// `FSCTL_GET_RETRIEVAL_POINTERS` returns the file's extent map: a run list of
/// virtual-to-logical cluster mappings saying where each part of the file
/// physically sits. The count of *placed* runs is how fragmented it is.
///
/// Needs `GENERIC_READ`, which for a file the user owns needs nothing else —
/// this is not an elevated operation. A system-protected file refuses the open,
/// and that is reported as the error it is rather than as "not fragmented".
pub fn fragmentation(
    path: &Path,
    stop: &std::sync::atomic::AtomicBool,
) -> io::Result<Fragmentation> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::sync::atomic::Ordering::Relaxed;
    use windows_sys::Win32::Foundation::{ERROR_HANDLE_EOF, ERROR_MORE_DATA, GetLastError};
    use windows_sys::Win32::System::Ioctl::FSCTL_GET_RETRIEVAL_POINTERS;

    const GENERIC_READ: u32 = 0x8000_0000;
    /// 64 KiB holds 4,094 runs per round trip, which covers all but the
    /// pathological cases in one call.
    const BUFFER: usize = 64 * 1024;

    use super::extents::UNPLACED;

    if stop.load(Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "extent check cancelled",
        ));
    }
    let file = fs::OpenOptions::new()
        .access_mode(GENERIC_READ)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        // Lets the same call work on a directory, whose index is also a stream
        // with an extent map.
        .custom_flags(
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
        )
        .open(path)?;
    if file.metadata()?.file_attributes() & (FILE_ATTRIBUTE_REPARSE_POINT | NOT_RESIDENT) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "layout is unavailable for links and placeholders",
        ));
    }

    let mut found = Fragmentation::default();
    let mut buffer = vec![0u8; BUFFER];
    let mut start_vcn: i64 = 0;

    loop {
        if stop.load(Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "extent check cancelled",
            ));
        }
        let mut returned: u32 = 0;
        // SAFETY: the handle is valid and kept alive by `file`; the input is a
        // live `i64` of exactly the length passed — a `STARTING_VCN_INPUT_BUFFER`
        // is one — and the output buffer is a live allocation of its own length.
        // `returned` is a live out-parameter.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle() as HANDLE,
                FSCTL_GET_RETRIEVAL_POINTERS,
                (&raw const start_vcn).cast(),
                std::mem::size_of::<i64>() as u32,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: reads a thread-local error code set by the call above.
        let error = unsafe { GetLastError() };

        if ok == 0 && error != ERROR_MORE_DATA {
            // A file small enough to live inside its own MFT record has no
            // extents at all. That is not a failure and not fragmentation: it
            // is the least fragmented a file can be, and it is why this returns
            // what it has rather than an error.
            if error == ERROR_HANDLE_EOF {
                return Ok(found);
            }
            // **A filesystem that does not implement the query is not a
            // failure to read a file.** `FSCTL_GET_RETRIEVAL_POINTERS` is NTFS'
            // and not every volume Windows mounts is NTFS: the Windows Sandbox's
            // own C: is a layered virtual filesystem and answers
            // `ERROR_NOT_SUPPORTED`, which is where this was found — the tests
            // for this function had only ever run on real NTFS.
            //
            // Said as `Unsupported`, because that is the kind the callers
            // already look for and the one `from_raw_os_error` does not produce:
            // error 50 arrives as `Uncategorized`, indistinguishable from a file
            // that vanished.
            const ERROR_INVALID_FUNCTION: u32 = 1;
            const ERROR_NOT_SUPPORTED: u32 = 50;
            if error == ERROR_NOT_SUPPORTED || error == ERROR_INVALID_FUNCTION {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "this filesystem does not report where a file's bytes are",
                ));
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }

        let Some(run) = super::extents::parse(&buffer[..returned as usize]) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the extent map did not have the shape it declares",
            ));
        };

        let mut vcn = run.starting_vcn;
        for &(next_vcn, lcn) in &run.extents {
            let clusters = next_vcn.saturating_sub(vcn).max(0) as u64;
            if lcn == UNPLACED {
                found.holes += 1;
            } else {
                found.extents += 1;
                found.clusters += clusters;
            }
            vcn = next_vcn;
        }

        // Past this many runs the answer is "very", and the remaining round
        // trips buy nothing anybody would act on differently.
        if found.extents + found.holes >= MAX_EXTENTS {
            found.truncated = true;
            break;
        }
        // A success means the list is complete. `ERROR_MORE_DATA` means what
        // arrived is real and there is more, and the next query starts where
        // this one stopped. An empty run with more claimed would not advance,
        // so it ends the loop rather than spinning.
        if ok != 0 || run.extents.is_empty() {
            break;
        }
        start_vcn = vcn;
    }

    Ok(found)
}

/// The ceiling on runs read for one file.
const MAX_EXTENTS: u64 = 100_000;

/// Whether the volume holding `path` has a head that has to move.
///
/// Asked of the volume with **zero desired access**, which is the whole reason
/// this needs no elevation: a handle opened for nothing at all still answers
/// query control codes, while opening `\\.\C:` for `GENERIC_READ` — what the
/// MFT reader does — requires administrator.
///
/// `IncursSeekPenalty` is the right question rather than "is this an SSD",
/// because what makes fragmentation matter is the seek: an NVMe drive, a RAM
/// disk and a well-behaved array all answer the same way, and so should the
/// advice.
pub fn media_type(path: &Path) -> Option<MediaType> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        STORAGE_PROPERTY_QUERY, StorageDeviceSeekPenaltyProperty,
    };

    // `C:\` -> `\\.\C:`. The device namespace wants the form without the
    // trailing separator, which is the only thing telling the two spellings
    // apart.
    let volume = volume_root(path)?;
    let letter = volume
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    let device = format!(r"\\.\{letter}");

    let file = fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(0)
        .open(&device)
        .ok()?;

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut descriptor: DEVICE_SEEK_PENALTY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let mut returned: u32 = 0;

    // SAFETY: the handle is valid and kept alive by `file`; both structures are
    // live, correctly typed and of exactly the lengths passed, and `returned`
    // is a live out-parameter.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&raw const query).cast(),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            (&raw mut descriptor).cast(),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        // A device that will not answer is one we say nothing about rather than
        // one we guess at: the advice this gates is only worth giving when the
        // answer is known.
        return None;
    }
    Some(if descriptor.IncursSeekPenalty {
        MediaType::Spinning
    } else {
        MediaType::SolidState
    })
}

// ------------------------------------------------------------------- owners

/// Who a file belongs to, as a security identifier.
///
/// **Two calls, because one of them follows links.** `GetFileSecurityW` takes a
/// path and opens the file itself, which means a symlink or a junction is
/// resolved and the *target's* owner comes back as the link's. That is exactly
/// the bug `--exact` carried until T-18 — measured, never followed is a rule
/// about every property of a link, not only about its size — so a reparse point
/// is opened explicitly with `FILE_FLAG_OPEN_REPARSE_POINT` and asked through
/// its own handle instead. There are a handful of those in a tree and millions
/// of ordinary files, so the cheap call stays the common one.
///
/// `None` when the owner could not be read: a file whose security descriptor
/// this process is refused, or a link whose own handle will not open. The
/// scanner records that as unreadable rather than as unowned; see
/// [`crate::owner`].
pub(super) fn owner_of(path: &Path, is_reparse: bool) -> Option<OwnerId> {
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;

    // An owner-only descriptor is a 20-byte header and a SID, so this is never
    // short in practice. The `needed` path below covers it anyway, because a
    // buffer sized from a guess is how a truncated read becomes a wrong answer.
    let mut buffer = [0u8; 32 + sid::MAX_BYTES];
    let mut needed: u32 = 0;

    let read_into = |buffer: &mut [u8], needed: &mut u32| -> Option<bool> {
        if is_reparse {
            return owner_by_handle(path, buffer, needed);
        }
        let wide = to_wide_path(path);
        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
        // call, `buffer` is a live allocation of exactly the length passed, and
        // `needed` is a live out-parameter.
        let ok = unsafe {
            GetFileSecurityW(
                wide.as_ptr(),
                OWNER_SECURITY_INFORMATION,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &raw mut *needed,
            )
        };
        Some(ok != 0)
    };

    if read_into(&mut buffer, &mut needed)? {
        return owner_from(&buffer);
    }

    // `ERROR_INSUFFICIENT_BUFFER` leaves the size wanted in `needed`.
    // Unreachable for an owner-only descriptor, which cannot exceed the buffer
    // above — and handled rather than assumed away, because the alternative to
    // handling it is parsing whatever half a descriptor happens to say.
    let wanted = needed as usize;
    if wanted <= buffer.len() || wanted > 64 * 1024 {
        return None;
    }
    let mut heap = vec![0u8; wanted];
    let mut again: u32 = 0;
    if read_into(&mut heap, &mut again)? {
        return owner_from(&heap);
    }
    None
}

fn owner_from(descriptor: &[u8]) -> Option<OwnerId> {
    let owner = sid::owner_of_descriptor(descriptor)?;
    sid::to_string(owner).map(OwnerId::Sid)
}

/// The owner of the link itself, through a handle opened on it.
///
/// `Some(false)` means the query ran and would not fit; `None` means the file
/// could not be opened at all, which is not a size problem and must not be
/// retried with a bigger buffer.
fn owner_by_handle(path: &Path, buffer: &mut [u8], needed: &mut u32) -> Option<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::{GetKernelObjectSecurity, OWNER_SECURITY_INFORMATION};

    /// The right that reading a security descriptor needs, and the only one
    /// asked for. Not `GENERIC_READ`: nothing here opens a file's contents.
    const READ_CONTROL: u32 = 0x0002_0000;

    let file = fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;

    // SAFETY: the handle is valid and kept alive by `file` across the call,
    // `buffer` is a live allocation of exactly the length passed, and `needed`
    // is a live out-parameter.
    let ok = unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle() as HANDLE,
            OWNER_SECURITY_INFORMATION,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &raw mut *needed,
        )
    };
    Some(ok != 0)
}

/// Put a name to a security identifier.
///
/// The classification is the system's own answer rather than kosha's guess:
/// `LookupAccountSidW` reports what kind of principal a SID names, and the
/// mapping below only decides which of those count as somebody using the
/// machine. That distinction earns its keep immediately — `TrustedInstaller`
/// and `SYSTEM` own most of a Windows installation, and a report that cannot
/// set them aside buries the people under the operating system.
///
/// Spelled `DOMAIN\name` where there is a domain, which is how every other tool
/// on the machine writes it and what a user would type back into one.
// `SID_NAME_USE` is an `i32` whose values are `SidTypeUser` and friends —
// constants rather than variants. Matching on them by name is the only way to
// read the mapping below against the documentation; the alternative is a column
// of numbers nobody can check against anything.
#[allow(non_upper_case_globals)]
pub(super) fn resolve_owner(id: &OwnerId) -> Option<(String, OwnerKind)> {
    use windows_sys::Win32::Security::{
        LookupAccountSidW, SID_NAME_USE, SidTypeAlias, SidTypeComputer, SidTypeDeletedAccount,
        SidTypeDomain, SidTypeGroup, SidTypeLabel, SidTypeLogonSession, SidTypeUser,
        SidTypeWellKnownGroup,
    };

    let OwnerId::Sid(text) = id else {
        // A uid on Windows. Not reachable through a scan.
        return None;
    };
    let bytes = sid::from_string(text)?;

    // Longer than any real account name; the call reports what it wanted when
    // it is short, which the retry below honours rather than truncating a name.
    let mut name = vec![0u16; 256];
    let mut domain = vec![0u16; 256];
    let mut name_len = name.len() as u32;
    let mut domain_len = domain.len() as u32;
    let mut use_of: SID_NAME_USE = 0;

    let mut lookup = |name: &mut [u16], domain: &mut [u16], nl: &mut u32, dl: &mut u32| {
        // SAFETY: `bytes` holds a valid SID that outlives the call; the two
        // buffers are live allocations of exactly the lengths passed, and the
        // three out-parameters are live and correctly typed.
        unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                bytes.as_ptr() as *mut _,
                name.as_mut_ptr(),
                &raw mut *nl,
                domain.as_mut_ptr(),
                &raw mut *dl,
                &raw mut use_of,
            )
        }
    };

    if lookup(&mut name, &mut domain, &mut name_len, &mut domain_len) == 0 {
        // Either it does not fit — in which case the lengths now say what it
        // wants — or the machine does not know this account, which is what
        // `None` carries back.
        if name_len as usize <= name.len() && domain_len as usize <= domain.len() {
            return None;
        }
        name = vec![0u16; name_len.max(1) as usize];
        domain = vec![0u16; domain_len.max(1) as usize];
        if lookup(&mut name, &mut domain, &mut name_len, &mut domain_len) == 0 {
            return None;
        }
    }

    let account = String::from_utf16_lossy(&name[..name_len as usize]);
    let scope = String::from_utf16_lossy(&domain[..domain_len as usize]);
    if account.is_empty() {
        return None;
    }
    let full = if scope.is_empty() {
        account
    } else {
        format!("{scope}\\{account}")
    };

    let kind = match use_of {
        // Somebody logs in as this.
        SidTypeUser => OwnerKind::Person,
        // An account that was a person and has been removed. Still a person's
        // files, and still worth a row of its own — on a shared machine this is
        // the case that most often turns out to be the answer.
        SidTypeDeletedAccount => OwnerKind::Person,
        // `BUILTIN\Administrators` owning a file means an elevated process
        // created it, which is somebody's doing rather than the system's.
        SidTypeGroup | SidTypeAlias => OwnerKind::Group,
        // `SYSTEM`, `NT SERVICE\TrustedInstaller`, the machine account and the
        // integrity labels. None of these is anybody.
        SidTypeWellKnownGroup
        | SidTypeDomain
        | SidTypeComputer
        | SidTypeLabel
        | SidTypeLogonSession => OwnerKind::Machine,
        // Named, and the system would not say what it is.
        _ => OwnerKind::Unknown,
    };
    Some((full, kind))
}

/// The account this process is running as.
///
/// Read from the process token rather than from `GetUserNameW`, because the two
/// can disagree in exactly the case that matters: an elevated process and its
/// unelevated parent are the same *person* and can hold different tokens, and
/// it is the token's user SID that the filesystem compares a file's owner
/// against.
/// The account that will own what this process creates.
///
/// **Not always the same account as the one above, and that is a Windows policy
/// rather than an edge case.** "System objects: Default owner for objects created
/// by members of the Administrators group" has shipped for twenty years; where it
/// names the group, every file an elevated administrator creates is owned by
/// `BUILTIN\Administrators` and not by them. `TokenUser` answers who the process
/// *is*; `TokenOwner` answers what it *stamps*, and a disk-usage tool comparing a
/// file's owner wants the second.
///
/// Measured on a clean Windows, where the tests for `owner = me` failed and this
/// developer's machine could not reproduce it: the tree said
/// `BUILTIN\Administrators (6 files)` and the process said
/// `…\WDAGUtilityAccount`. The filter matched nothing and the owners panel filed
/// somebody's own files under a group.
pub(super) fn token_owner() -> Option<OwnerId> {
    use windows_sys::Win32::Security::{TOKEN_OWNER, TokenOwner};

    // The same shape as `current_owner`: a pointer and the SID it points at, in
    // one buffer.
    let buffer = token_information(TokenOwner, size_of::<TOKEN_OWNER>())?;
    // SAFETY: the call succeeded, so `buffer` starts with a `TOKEN_OWNER` whose
    // `Owner` points into `buffer`, which is still alive.
    let owner = unsafe { &*(buffer.as_ptr() as *const TOKEN_OWNER) };
    if owner.Owner.is_null() {
        return None;
    }
    let offset = owner.Owner as usize - buffer.as_ptr() as usize;
    sid::to_string(buffer.get(offset..)?).map(OwnerId::Sid)
}

/// One field of this process' token, as the bytes the API wrote.
///
/// Shared by the two callers above because the dance is the same either way: a
/// struct with a pointer into its own tail, so the buffer has to hold both and
/// the pointer means nothing once it is dropped.
fn token_information(class: i32, header: usize) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no closing,
    // and `token` is a live out-parameter.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return None;
    }
    let mut buffer = vec![0u8; header + sid::MAX_BYTES];
    let mut needed: u32 = 0;
    // SAFETY: `token` is a live handle, `buffer` is a live allocation of exactly
    // the length passed, and `needed` is a live out-parameter.
    let ok = unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &raw mut needed,
        )
    };
    // SAFETY: `token` came from `OpenProcessToken` and is closed once.
    unsafe { CloseHandle(token) };
    (ok != 0).then_some(buffer)
}

pub(super) fn current_owner() -> Option<OwnerId> {
    use windows_sys::Win32::Security::{TOKEN_USER, TokenUser};

    // A `TOKEN_USER` is a pointer and a u32, and the SID it points at follows it
    // in the same buffer — so the buffer has to hold both, and the pointer is
    // only meaningful while that buffer is alive.
    let buffer = token_information(TokenUser, size_of::<TOKEN_USER>())?;
    // SAFETY: the call succeeded, so `buffer` starts with a `TOKEN_USER` whose
    // `Sid` points into `buffer`, which is still alive.
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    if user.User.Sid.is_null() {
        return None;
    }
    let offset = user.User.Sid as usize - buffer.as_ptr() as usize;
    sid::to_string(buffer.get(offset..)?).map(OwnerId::Sid)
}

/// A Unix second as local wall-clock time.
///
/// `SystemTimeToTzSpecificLocalTime` rather than the bias out of
/// `GetTimeZoneInformation`: the bias describes *now*, and applying today's
/// daylight saving to a snapshot taken in July is how a timestamp ends up an
/// hour out for half the year. This asks Windows what the clock said on that
/// date, which is the question being asked.
pub(super) fn local_clock(unix_secs: i64) -> Option<super::Clock> {
    let (days, rest) = (unix_secs.div_euclid(86_400), unix_secs.rem_euclid(86_400));
    let (year, month, day) = crate::format::civil_from_days(days);
    // SYSTEMTIME years are a u16 and Windows refuses anything before 1601.
    // A stamp outside that is a broken clock, and `None` sends the caller to
    // its UTC fallback rather than to a wrapped year.
    if !(1601..=9999).contains(&year) {
        return None;
    }

    let utc = SYSTEMTIME {
        wYear: year as u16,
        wMonth: month as u16,
        // Ignored by the conversion, which is why it is not worked out here.
        wDayOfWeek: 0,
        wDay: day as u16,
        wHour: (rest / 3600) as u16,
        wMinute: ((rest % 3600) / 60) as u16,
        wSecond: (rest % 60) as u16,
        wMilliseconds: 0,
    };
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: both structures are valid and writable for the call, and a null
    // time zone means "the one this machine is set to".
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
    if ok == 0 {
        return None;
    }
    Some(super::Clock {
        year: i64::from(local.wYear),
        month: u32::from(local.wMonth),
        day: u32::from(local.wDay),
        hour: u32::from(local.wHour),
        minute: u32::from(local.wMinute),
        second: u32::from(local.wSecond),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The string forms, without touching a filesystem.
    ///
    /// Each of these is a path Win32 accepts under a different rule about what
    /// may go in front of it: a drive path takes the prefix, a UNC path takes it
    /// with `UNC` in place of one backslash, and the ones that must be left alone
    /// are left alone — prefixing an already-verbatim path, a device path or a
    /// relative one names nothing at all.
    #[test]
    fn a_long_path_is_handed_to_win32_in_the_form_that_has_no_limit() {
        let wide = |path: &str| -> String {
            let out = super::to_wide_path(Path::new(path));
            assert_eq!(out.last(), Some(&0), "not NUL-terminated");
            String::from_utf16(&out[..out.len() - 1]).unwrap()
        };

        assert_eq!(wide(r"C:\Users\someone\deep"), r"\\?\C:\Users\someone\deep");
        assert_eq!(
            wide(r"\\server\share\folder\file.txt"),
            r"\\?\UNC\server\share\folder\file.txt"
        );

        for already in [r"\\?\C:\Windows", r"\\?\UNC\server\share", r"\\.\C:"] {
            assert_eq!(wide(already), already);
        }

        // A relative path, and a drive-relative one, which names a different
        // file depending on the process' current directory on that drive.
        assert_eq!(wide(r"docs\design"), r"docs\design");
        assert_eq!(wide(r"C:relative\thing"), r"C:relative\thing");
    }
}
