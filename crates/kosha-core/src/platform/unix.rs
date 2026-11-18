// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Unix metadata.
//!
//! `read_dir` hands back `d_type`, so directory-vs-file is free, but the *size*
//! costs one `lstat` per entry. That syscall dominates a Unix scan, which is
//! why the scanner parallelises within a directory once it is large enough.
//!
//! `st_blocks` gives the real on-disk size for free as part of the same stat,
//! so sparse files, compressed files and cluster slack are all exact here with
//! no extra work.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::{DiskUsage, EntryMeta, PlatformCtx};
use crate::owner::{OwnerId, OwnerKind};

#[inline]
pub(super) fn device_of(md: &fs::Metadata) -> u64 {
    md.dev()
}

/// `statvfs` would give the exact figure, but `st_blocks` already reports true
/// on-disk usage on every Unix filesystem, so the cluster size is never used
/// for real files here. It only backstops the rare metadata-less path.
pub(super) fn cluster_size(_root: &Path) -> Option<u64> {
    Some(4096)
}

#[inline]
pub(super) fn entry_meta(
    entry: &fs::DirEntry,
    file_type: fs::FileType,
    _dir_path: &Path,
    ctx: &PlatformCtx,
) -> io::Result<EntryMeta> {
    // `DirEntry::metadata` does not traverse symlinks on Unix, so this is an
    // `lstat` and a symlink is measured as the link itself.
    let md = entry.metadata()?;
    Ok(from_metadata(&md, file_type, ctx))
}

pub(super) fn root_meta(root: &Path, ctx: &PlatformCtx) -> io::Result<EntryMeta> {
    let md = fs::symlink_metadata(root)?;
    let ft = md.file_type();
    Ok(from_metadata(&md, ft, ctx))
}

#[inline]
fn from_metadata(md: &fs::Metadata, file_type: fs::FileType, ctx: &PlatformCtx) -> EntryMeta {
    let is_dir = file_type.is_dir();
    let logical = md.len();
    // `st_blocks` is always in 512-byte units regardless of the filesystem's
    // own block size — that is what POSIX specifies, not `st_blksize`.
    let allocated = md.blocks().saturating_mul(512);

    let link_key = if ctx.dedup_hardlinks && !is_dir && md.nlink() > 1 {
        Some((md.dev(), u128::from(md.ino())))
    } else {
        None
    };

    EntryMeta {
        logical,
        allocated,
        mtime: md.mtime().clamp(0, u32::MAX as i64) as u32,
        created: md
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
        device: md.dev(),
        // Windows only: a Unix mount point is a differing `st_dev` and needs
        // nothing else to recognise it.
        mounted_volume: None,
        link_key,
        is_dir,
        is_symlink: file_type.is_symlink(),
        // A file whose blocks do not cover its length by at least one cluster
        // is sparse (or transparently compressed, which we report the same way
        // because the user-visible consequence is identical).
        is_sparse: !is_dir && allocated + ctx.cluster_size < logical,
        // No portable equivalent. HSM-backed filesystems exist but expose
        // nothing standard, and guessing from a size-versus-blocks heuristic
        // would put a scary label on every sparse file.
        is_offline: false,
        // Free: `st_uid` came with the stat the scan had to do anyway. There is
        // no path here where the owner is unreadable — if the stat succeeded
        // the uid is in it — so `None` here means only that nobody asked.
        owner: ctx.collect_owners.then(|| OwnerId::Uid(md.uid())),
    }
}

/// Put a name to a uid, out of the passwd database.
///
/// `getpwuid_r` rather than reading `/etc/passwd`: the database can be LDAP,
/// NIS or systemd-userdbd, and a uid from a network directory is exactly the
/// one a shared machine wants named.
///
/// **The account type is a judgement rather than a fact**, which is the honest
/// difference from Windows: there, `LookupAccountSid` returns what kind of
/// principal a SID is and kosha repeats it. Unix records no such thing, so the
/// login shell stands in for it — an account that cannot log in is a daemon's,
/// and `nologin` is precisely the field's purpose. Root is named outright
/// because uid 0 is the one uid that means something everywhere. No threshold
/// on the number itself: the boundary is 500 on macOS and 1000 on Linux, both
/// are configurable, and a wrong guess files a person under the operating
/// system where nobody looks.
pub(super) fn resolve_owner(id: &OwnerId) -> Option<(String, OwnerKind)> {
    let OwnerId::Uid(uid) = *id else {
        // A SID on Unix. Not reachable through a scan, and not something to
        // invent an answer for.
        return None;
    };

    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // `sysconf(_SC_GETPW_R_SIZE_MAX)` is the documented way to size this and
    // returns -1 on plenty of systems, so a fixed buffer with one retry is what
    // is left. 4 KiB holds every real entry; the retry covers the rest.
    // `u8` rather than `c_char`: that type is signed on x86 and unsigned on
    // ARM, and the pointer is cast on the way in either way.
    let mut buffer = vec![0u8; 4096];
    loop {
        // SAFETY: `passwd` and `found` are live and correctly typed, and
        // `buffer` is a live allocation of exactly the length passed.
        let code = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &raw mut found,
            )
        };
        if code == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 4, 0);
            continue;
        }
        if code != 0 || found.is_null() {
            // No entry: a uid from a filesystem written on another machine, or
            // an account that has been deleted. The caller shows the number.
            return None;
        }
        break;
    }

    // SAFETY: `getpwuid_r` succeeded and filled `passwd`, whose string members
    // point into `buffer` and are NUL-terminated.
    let name = unsafe { std::ffi::CStr::from_ptr(passwd.pw_name) }
        .to_string_lossy()
        .into_owned();
    let shell = if passwd.pw_shell.is_null() {
        String::new()
    } else {
        // SAFETY: as above.
        unsafe { std::ffi::CStr::from_ptr(passwd.pw_shell) }
            .to_string_lossy()
            .into_owned()
    };

    let kind = if uid == 0 || shell.ends_with("/nologin") || shell.ends_with("/false") {
        OwnerKind::Machine
    } else {
        OwnerKind::Person
    };
    Some((name, kind))
}

/// The account this process is running as.
///
/// The *effective* uid, because that is what the kernel compares a file's
/// `st_uid` against, and the two differ for a setuid binary — which kosha is
/// not, and which is exactly the sort of thing that stops being true quietly.
pub(super) fn current_owner() -> Option<OwnerId> {
    // SAFETY: `geteuid` takes no arguments, touches no memory and cannot fail.
    Some(OwnerId::Uid(unsafe { libc::geteuid() }))
}

/// Volume capacity and free space, via `statvfs`.
///
/// `f_bfree` counts every free block; `f_bavail` excludes the reservation most
/// filesystems keep for root, which is the number an ordinary user can actually
/// write into. Both are kept, because they answer different questions and a
/// bar drawn from the wrong one is wrong by several percent on every ext4
/// volume in existence.
pub fn disk_usage(path: &Path) -> Option<DiskUsage> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };

    // SAFETY: `c_path` is NUL-terminated and outlives the call, and `stat` is a
    // valid, correctly aligned, zeroed `statvfs` that the kernel fills in.
    if unsafe { libc::statvfs(c_path.as_ptr(), &raw mut stat) } != 0 {
        return None;
    }

    // The block counts are in units of `f_frsize`, the fragment size.
    // `f_bsize` is the preferred I/O size and is a different number on plenty
    // of filesystems; using it would silently misreport capacity.
    //
    // Widened straight to `u64`. This went through `as u32` first, which
    // truncates: the field is `c_ulong`, 64 bits on 64-bit glibc. It happened
    // to work because fragment sizes are small, which is how a narrowing cast
    // survives review — it is not wrong on any filesystem anybody tested.
    // `as u64` is a no-op where the type is already 64-bit and a widening
    // where it is 32-bit, and cannot lose bits either way.
    let unit = if stat.f_frsize == 0 {
        stat.f_bsize as u64
    } else {
        stat.f_frsize as u64
    };

    let total = (stat.f_blocks as u64).saturating_mul(unit);
    if total == 0 {
        // Pseudo-filesystems — procfs, sysfs — report zero blocks. There is
        // nothing to draw and nothing true to say about how full they are.
        return None;
    }

    Some(DiskUsage {
        total,
        free: (stat.f_bfree as u64).saturating_mul(unit),
        available: (stat.f_bavail as u64).saturating_mul(unit),
    })
}

/// Bytes this file occupies on disk, from `st_blocks`.
///
/// Exact here, unlike Windows: the same `lstat` the scanner already does
/// carries it.
pub fn allocated_size(path: &Path) -> io::Result<u64> {
    let md = fs::symlink_metadata(path)?;
    Ok(md.blocks().saturating_mul(512))
}

/// Whether this process can read things an ordinary one cannot.
///
/// Root, and nothing subtler. Linux capabilities can grant `CAP_DAC_READ_SEARCH`
/// without the uid, and a process holding it really could read more — but there
/// is nothing on this platform for the extra reach to *do*: the elevated scan is
/// an NTFS strategy, and `st_blocks` already gives Unix the exact numbers
/// Windows needs administrator for. So this reports the fact and claims nothing
/// about what it unlocks.
pub fn elevated() -> bool {
    // SAFETY: `geteuid` takes no arguments, touches no memory and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// A Unix second as local wall-clock time.
///
/// `localtime_r` rather than `localtime`: the latter returns a pointer into a
/// static buffer that the next call from any thread overwrites, and this is
/// called from whichever thread happens to be drawing.
///
/// It reads the zone from the environment on first use, which is what every
/// other program on the machine does and therefore what the user's clock
/// agrees with.
pub(super) fn local_clock(unix_secs: i64) -> Option<super::Clock> {
    // `try_into` rather than `as`, and the deprecation allowed rather than
    // dodged.
    //
    // `libc::time_t` is deprecated on musl alone, and not because it is the
    // wrong type: musl 1.2 widened it to 64 bits on 32-bit targets and the
    // `libc` crate has not followed yet, so the alias carries a notice
    // pending that change. It is still exactly the type `localtime_r` takes,
    // and naming anything else here would be guessing at an ABI. The allow is
    // on this statement rather than the function, so a different deprecation
    // later is not silenced along with it.
    //
    // The conversion is fallible for the same reason the notice exists: where
    // `time_t` is 32 bits, a stamp past 2038 does not fit, and `as` would
    // quietly wrap it to a date in 1901. `None` sends the caller to its UTC
    // fallback, which is the right answer for a moment this platform cannot
    // represent — and it is why CI checks musl at all. There was a cast like
    // this one in the free-space code, and it truncated.
    //
    // `useless_conversion` is allowed because it is only useless *here*, on a
    // target where `time_t` is already `i64`. Clippy sees one width at a time
    // and cannot know that the same line is load-bearing on another; taking
    // its advice on a glibc x86_64 build would reintroduce the truncation on
    // 32-bit musl, which is the target this whole statement exists for.
    #[allow(deprecated, clippy::useless_conversion)]
    let when: libc::time_t = unix_secs.try_into().ok()?;
    // Zeroed rather than uninitialised: `localtime_r` fills every field it
    // promises, but a failure path that returns null must not leave us reading
    // whatever was on the stack.
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `when` is a valid `time_t` and `parts` is a valid, writable `tm`
    // that outlives the call.
    let out = unsafe { libc::localtime_r(&when, &mut parts) };
    if out.is_null() {
        return None;
    }
    Some(super::Clock {
        year: i64::from(parts.tm_year) + 1900,
        month: (parts.tm_mon as u32) + 1,
        day: parts.tm_mday as u32,
        hour: parts.tm_hour as u32,
        minute: parts.tm_min as u32,
        second: parts.tm_sec as u32,
    })
}

/// No filesystem here offers per-file compression through an interface like
/// Windows'. btrfs and ZFS compress per subvolume or dataset, which is an
/// administrator's decision about a mount rather than a thing a disk usage tool
/// should reach into.
pub fn compression_available_at(_path: &Path) -> bool {
    false
}
