// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Where "how big is this file" stops being portable.
//!
//! The two supported families answer the question at very different prices, and
//! pretending otherwise would cost us most of our speed.

use std::fs;
use std::io;
use std::path::Path;

use crate::owner::{OwnerId, OwnerKind};

// Compiled everywhere, consulted only on Windows: the rules are decided by
// reading rather than by running, so they stay under test on whichever machine
// they are being edited on.
// Off Windows the only callers are its own tests, which is the point rather
// than an oversight — the same arrangement as the `ntfs` modules.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod extents;
pub(crate) mod reparse;
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod sid;
pub mod wsl;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(target_os = "macos")]
pub use unix::macos_mounts;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix_extents;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;
#[cfg(windows)]
pub(crate) use windows::{NOT_RESIDENT, file_identity};

/// How full a mounted volume is.
///
/// Separate from anything the scanner produces, and deliberately so: a scan
/// measures what is under one path, while this measures the volume that path
/// sits on. Showing a 40 GiB scan against a 2 TB disk is the difference between
/// a number and a proportion, and the proportion is what tells somebody whether
/// they have a problem.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiskUsage {
    /// Capacity of the volume in bytes.
    pub total: u64,
    /// Free bytes on the volume, ignoring quotas.
    pub free: u64,
    /// Free bytes this user is actually allowed to consume. Equal to `free`
    /// without quotas, and smaller with them.
    pub available: u64,
}

impl DiskUsage {
    pub fn used(self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// Used share of the volume, 0.0 to 1.0.
    ///
    /// Zero for a zero-capacity volume rather than a division by zero: an empty
    /// optical drive or an unmounted card reader reports one, and a bar that
    /// panics is worse than a bar that reads empty.
    pub fn fraction_used(self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.used() as f64 / self.total as f64) as f32
    }
}

/// A moment as a clock on the wall showed it.
///
/// Local time, not UTC. Everywhere this appears it is next to something a
/// person did — when they took a snapshot — and a stamp five and a half hours
/// away from when they remember doing it does not read as a different time
/// zone. It reads as a broken clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clock {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// A Unix second as local wall-clock time, or `None` if the platform will not
/// say.
///
/// Asked of the operating system rather than worked out from an offset, so the
/// answer is right for *that* date's daylight saving rather than for today's.
/// A snapshot taken in July and read in December is exactly the case an offset
/// held once and reused gets wrong, and by a whole hour.
///
/// `None` rather than a guess: every caller has UTC to fall back to, and a
/// time stamped as local when it is not is a worse thing to show than one
/// that says `Z`.
pub fn local_clock(unix_secs: i64) -> Option<Clock> {
    imp::local_clock(unix_secs)
}

/// Whether a Win32 path may exceed `MAX_PATH` on this machine without the
/// `\\?\` prefix — which decides whether the *shell* can put a deep file in
/// the Recycle Bin. See the Windows implementation for what was measured.
///
/// `true` everywhere else: no other platform has the limit, so no caller has to
/// ask twice.
pub fn long_paths_enabled() -> bool {
    #[cfg(windows)]
    {
        imp::long_paths_enabled()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// How full the volume holding `path` is, or `None` if it cannot be asked.
///
/// `None` rather than a guess: a drive that is not ready, a path that has gone
/// away, or a filesystem that does not answer are all cases where showing a
/// wrong bar would be worse than showing none.
pub fn disk_usage(path: &Path) -> Option<DiskUsage> {
    imp::disk_usage(path)
}

/// Whether this process is running with administrative rights.
///
/// The elevated scan and read-write mode are separate axes and must stay that
/// way: this widens what a *scan can see* and changes nothing whatever about
/// what an action may do, which still needs a `ReadWriteGrant` that no amount
/// of privilege can conjure. Running as administrator with read-write mode on
/// is the one combination where a mistake is unrecoverable, and it is reached
/// only by making both choices deliberately.
///
/// On Windows this asks whether *this token* is elevated, not whether the user
/// is in the administrators group — with UAC on, the interactive administrator
/// runs filtered, and a raw volume open fails for them exactly as it does for a
/// standard user.
pub fn elevated() -> bool {
    imp::elevated()
}

/// Ask the token for the privilege a raw volume read needs.
///
/// Windows only, and *absent* rather than a no-op elsewhere: the one caller is
/// the MFT strategy, which does not exist on another platform, and a function
/// that always returns false would be a claim nobody could act on.
///
/// Returns whether the privilege is now held, which is false for an unelevated
/// process — where `AdjustTokenPrivileges` reports success while enabling
/// nothing.
#[cfg(windows)]
pub fn enable_backup_privilege() -> bool {
    imp::enable_backup_privilege()
}

/// How many pieces a file's bytes are in, on the disk.
///
/// A file the filesystem could not lay down in one run has to be found in
/// several, and on a disk with a head that has to move, each extra run is a
/// seek. On solid state it is very nearly free, which is why nothing reports
/// this without first asking what kind of disk it is — see [`MediaType`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Fragmentation {
    /// Runs of contiguous clusters actually placed on the volume.
    ///
    /// One is a perfectly laid-out file. Zero is a file with no extents at all
    /// — empty, or small enough to live inside its own MFT record — which is
    /// the least fragmented a file can be rather than a missing answer.
    pub extents: u64,
    /// Clusters those runs cover.
    pub clusters: u64,
    /// Mapped bytes inside the logical file on Linux and macOS.
    pub mapped_bytes: Option<u64>,
    /// Runs that are holes in a sparse file. Counted separately because they
    /// occupy nothing and cost no seek, so calling them fragmentation would
    /// report every sparse file as badly fragmented when it is not.
    pub holes: u64,
    /// The extent map was longer than we were willing to read, so `extents` is
    /// a floor. Only reachable on a file in tens of thousands of pieces, where
    /// the exact number is not the point.
    pub truncated: bool,
}

impl Fragmentation {
    /// Whether the bytes are in more than one place.
    pub fn is_fragmented(&self) -> bool {
        self.extents > 1
    }

    /// Average clusters per run — how long a stretch the disk reads before it
    /// has to look somewhere else. Higher is better; `None` when there is
    /// nothing placed to average over.
    pub fn clusters_per_extent(&self) -> Option<f64> {
        (self.extents > 0 && self.mapped_bytes.is_none())
            .then(|| self.clusters as f64 / self.extents as f64)
    }
}

/// What kind of disk a volume is on.
///
/// Deliberately two values and an absence rather than three: "we asked and it
/// would not say" and "it has no moving parts" are different, and the advice
/// that hangs off this should only be given when the answer is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaType {
    /// A head that has to move, so a seek costs milliseconds and fragmentation
    /// is worth a user's attention.
    Spinning,
    /// No moving parts. Fragmentation is a number rather than a problem, and
    /// saying anything about it would be noise.
    SolidState,
}

/// Whether this build has an extent reader. The filesystem may still decline.
pub const fn supports_fragmentation() -> bool {
    cfg!(any(windows, target_os = "linux", target_os = "macos"))
}

/// Read the allocated runs of a live file without reading file contents.
pub fn fragmentation(path: &Path) -> io::Result<Fragmentation> {
    fragmentation_with_cancel(path, &std::sync::atomic::AtomicBool::new(false))
}

/// Read the file layout, checking cancellation between mapping requests.
pub fn fragmentation_with_cancel(
    path: &Path,
    stop: &std::sync::atomic::AtomicBool,
) -> io::Result<Fragmentation> {
    #[cfg(windows)]
    return imp::fragmentation(path, stop);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return unix_extents::fragmentation(path, stop);
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (path, stop);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this build has no extent reader",
        ))
    }
}

/// What kind of disk the volume holding `path` is, when it will say.
///
/// Costs one query on a handle opened with *no access rights at all*, which is
/// what keeps it out of the elevated path: `\\.\C:` opened for nothing still
/// answers property queries, while opening it for `GENERIC_READ` — what reading
/// the MFT needs — requires administrator.
pub fn media_type(path: &Path) -> Option<MediaType> {
    #[cfg(windows)]
    {
        imp::media_type(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// Whether this platform can turn filesystem compression on and off per file.
///
/// Windows only, and deliberately not faked elsewhere. NTFS exposes a
/// per-file toggle that is transparent to every reader of the file. The Unix
/// equivalents are not the same thing: btrfs's `chattr +c` applies to *future*
/// writes rather than existing data, and APFS compression has no supported
/// interface for setting it. Offering a button that quietly does nothing, or
/// something different from what it says, is worse than not offering it.
pub const fn supports_compression() -> bool {
    cfg!(windows)
}

/// Whether the volume under `path` can compress a file.
///
/// The narrower question, and the one worth asking before offering the action:
/// [`supports_compression`] answers for the platform, and a Windows machine
/// with a FAT32 stick in it can do neither on that stick. Read from the
/// filesystem's own flags rather than found out by compressing something, which
/// is how a capability probe ends up wanting a mutating API.
pub fn compression_available_at(path: &Path) -> bool {
    imp::compression_available_at(path)
}

/// Whether this platform can be asked to scan across a filesystem boundary.
///
/// True on both, now. On Unix a differing `st_dev` marks the boundary. On
/// Windows there is no cheap `st_dev`: a mounted volume is a reparse point
/// that shares its tag with a junction and differs only in pointing at
/// `\??\Volume{...}` rather than a directory, so the boundary costs a query
/// to recognise — see `windows::mounted_volume`.
///
/// This returned false for a while, and the setting was withheld on Windows
/// rather than offered and quietly ignored. That was honest about a real gap
/// and is no longer true: the gap is closed, and a control that now works is a
/// control worth showing.
///
/// [`ScanOptions::cross_filesystems`]: crate::ScanOptions::cross_filesystems
pub const fn can_cross_filesystems() -> bool {
    true
}

/// Whether asking for an exact on-disk size costs anything on this platform.
///
/// False on Unix, where `st_blocks` arrives with the `lstat` the scan already
/// does: the figure is exact whether or not anybody asks for it, and
/// [`ScanOptions::exact_allocated`] changes nothing. True on Windows, where it
/// means `GetCompressedFileSizeW` and an open per file.
///
/// Exists so the setting can be *hidden* where it buys nothing, the same way
/// the compression action is. A control that does nothing is worse than no
/// control: somebody who finds it assumes their numbers were wrong until they
/// ticked it.
///
/// [`ScanOptions::exact_allocated`]: crate::ScanOptions::exact_allocated
pub const fn exact_size_costs_extra() -> bool {
    cfg!(windows)
}

/// Whether reading who owns a file costs anything on this platform.
///
/// False on Unix, where `st_uid` arrives with the `lstat` the walk already
/// does. True on Windows, where it means asking for the file's security
/// descriptor — one more call per entry during the ordinary walk, and nothing
/// at all during an MFT scan, where every record carries a security id already.
///
/// Exists so the interface can warn where the setting is expensive and stay
/// quiet where it is free, the same way [`exact_size_costs_extra`] does.
pub const fn owner_costs_extra() -> bool {
    cfg!(windows)
}

/// Whether learning that a file is hardlinked costs anything on this platform.
///
/// False on Unix, where `st_nlink` and `st_ino` arrive with the `lstat` the
/// walk already does. True on Windows, where it means an open per file to ask
/// for the file's identity.
///
/// **This is about the folder walk and nothing else.** An MFT scan and every
/// disk-image reader have the link count and an identity in hand already, so
/// they count a hardlinked file once whatever this says — which is why
/// [`ScanOptions::dedup_hardlinks`] is a question rather than an answer, and
/// why the default is spelt `None`.
///
/// Exists so the setting can be hidden where it buys nothing, the same way
/// [`exact_size_costs_extra`] is.
///
/// [`ScanOptions::dedup_hardlinks`]: crate::ScanOptions::dedup_hardlinks
pub const fn hardlinks_cost_extra() -> bool {
    cfg!(windows)
}

/// Put a name to an account, when the machine knows one.
///
/// Called once per *distinct* owner after a scan, never per file: on a
/// domain-joined Windows machine this can go to a domain controller over the
/// network, and paying that per entry is the difference between a scan and a
/// hang.
///
/// `None` means the machine could not name it — a deleted account, a SID from
/// another computer, a uid with no passwd entry. That is a real answer and the
/// caller shows the identifier itself rather than inventing a placeholder.
pub fn resolve_owner(id: &OwnerId) -> Option<(String, OwnerKind)> {
    imp::resolve_owner(id)
}

/// Who is running kosha, named if the machine can name them.
///
/// So that a query can say `owner = me` — which is the query somebody on a
/// shared machine actually wants, since the alternative is looking up the
/// spelling of their own account before they can ask about it.
///
/// `None` when the account cannot be read at all, which is not a case anybody
/// has seen: the process token always names its own user.
pub fn current_owner() -> Option<crate::owner::Owner> {
    let id = imp::current_owner()?;
    let mut owner = crate::owner::Owner::unnamed(id);
    if let Some((name, kind)) = imp::resolve_owner(&owner.id) {
        owner.name = Some(name);
        owner.kind = kind;
    }
    Some(owner)
}

/// Every account that means "this process", for a filter that says `me`.
///
/// One on Unix. **Two on Windows, and the second is the one that was missing:**
/// the token's user is who the process is, and the token's *owner* is what will
/// be stamped on the files it creates. A Windows policy — "System objects:
/// Default owner for objects created by members of the Administrators group" —
/// makes those different, and where it is set every file an elevated
/// administrator writes is owned by `BUILTIN\Administrators`.
///
/// Which matters here more than anywhere, because the strategy this program is
/// proudest of on Windows *requires* elevation. Measured on a clean Windows: the
/// tree said `BUILTIN\Administrators (6 files)`, the process said
/// `…\WDAGUtilityAccount`, and `owner = me` matched nothing at all — a filter
/// that answers "none of your files" about a directory full of them.
///
/// Ordered with the user first, so anything showing one of these shows the
/// person rather than the group.
pub fn current_owners() -> Vec<crate::owner::Owner> {
    let mut ids: Vec<crate::owner::OwnerId> = Vec::new();
    if let Some(id) = imp::current_owner() {
        ids.push(id);
    }
    #[cfg(windows)]
    if let Some(id) = imp::token_owner()
        && !ids.contains(&id)
    {
        ids.push(id);
    }

    ids.into_iter()
        .map(|id| {
            let mut owner = crate::owner::Owner::unnamed(id);
            if let Some((name, kind)) = imp::resolve_owner(&owner.id) {
                owner.name = Some(name);
                owner.kind = kind;
            }
            owner
        })
        .collect()
}

/// Bytes this file actually occupies on disk, asked of the filesystem.
///
/// Distinct from the scanner's `allocated`, which on Windows is an estimate
/// derived from the cluster size unless `exact_allocated` was set. Compression
/// makes that estimate wrong by design, so anything reporting what compression
/// saved has to ask rather than derive.
pub fn allocated_size(path: &Path) -> io::Result<u64> {
    imp::allocated_size(path)
}

/// Whether the file at `path` is a cloud placeholder, asked of its metadata and
/// never by opening it.
///
/// For the few readers that open a file the user named directly (a disk image
/// handed to the scanner), which must refuse a placeholder rather than fetch it.
pub(crate) fn is_placeholder(path: &Path) -> bool {
    imp::is_placeholder(path)
}

/// Everything the scanner learns about one directory entry.
///
/// `Clone` but no longer `Copy`: [`EntryMeta::owner`] carries a SID, which is
/// text of a length nobody can bound at compile time without wasting 68 bytes
/// on every entry of every scan that never asks for it.
#[derive(Clone, Debug, Default)]
pub struct EntryMeta {
    /// Apparent size in bytes.
    pub logical: u64,
    /// Bytes consumed on disk.
    pub allocated: u64,
    /// Unix seconds, saturating.
    pub mtime: u32,
    pub created: Option<u64>,
    /// Device the entry lives on, for mount-crossing detection. Always 0 on
    /// Windows, where reparse points serve the same purpose.
    pub device: u64,
    /// Whatever identifies this file uniquely on its volume, when it has more
    /// than one link, so the scanner can count its bytes exactly once.
    /// `(st_dev, st_ino)` on Unix and `(volume serial, file id)` on Windows —
    /// different numbers with the same meaning, and never compared across
    /// platforms. `None` when the file is unshared or the setting is off.
    ///
    /// The id is 128 bits because Windows' is: ReFS file ids do not fit in 64,
    /// and the short form Windows also offers is documented as not unique
    /// there. Unix widens its `st_ino` into the same slot at no cost — the tuple
    /// is a hash key that lives only for the length of a scan, so the eight
    /// extra bytes are never stored per entry.
    pub link_key: Option<(u64, u128)>,
    /// The serial of a volume mounted *at* this entry, when it is one.
    ///
    /// Windows only, and `None` for everything else including every Unix
    /// entry. A mounted volume there is a reparse point sharing its tag with a
    /// junction, so telling them apart costs a query and the answer is worth
    /// carrying: it is the one entry kind that is a filesystem boundary rather
    /// than a link, and the scanner needs the serial to notice a volume
    /// mounted inside a volume it is already walking.
    pub mounted_volume: Option<u64>,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_sparse: bool,
    /// The data is not resident on this disk — a cloud placeholder, or anything
    /// else behind Windows' `OFFLINE` and `RECALL_ON_*` attributes.
    ///
    /// On macOS this is the kernel's own `SF_DATALESS` marker from `st_flags`.
    /// False on every other Unix: there is no standard equivalent, and
    /// inventing one from a heuristic would put a scary label on ordinary files.
    pub is_offline: bool,
    /// Who the filesystem says this entry belongs to.
    ///
    /// `None` when the scan did not ask — [`PlatformCtx::collect_owners`] — and
    /// also when it did and was refused, which the scanner keeps apart from
    /// "unowned" because they are different answers. See [`crate::owner`].
    pub owner: Option<OwnerId>,
}

/// Per-scan platform state, computed once for the root and shared read-only
/// with every worker thread.
#[derive(Clone, Copy, Debug)]
pub struct PlatformCtx {
    /// Device id of the scan root; entries on other devices are not descended.
    pub root_device: u64,
    /// Allocation unit of the root's filesystem, used to estimate on-disk size
    /// where the OS does not hand it to us for free.
    pub cluster_size: u64,
    /// Pay for an exact on-disk size even when it costs an extra syscall.
    pub exact_allocated: bool,
    /// Track inodes so hardlinked files are counted once.
    pub dedup_hardlinks: bool,
    /// Read who owns each entry. Free on Unix, an extra call per entry on
    /// Windows — see [`owner_costs_extra`].
    pub collect_owners: bool,
}

impl PlatformCtx {
    /// Probe the filesystem hosting `root`.
    ///
    /// Never fails the scan: if a probe is unavailable we fall back to a 4 KiB
    /// cluster, which is correct for the overwhelming majority of volumes and
    /// wrong only in the third decimal place.
    pub fn for_root(
        root: &Path,
        exact_allocated: bool,
        dedup_hardlinks: bool,
        collect_owners: bool,
    ) -> io::Result<Self> {
        let md = fs::symlink_metadata(root)?;
        Ok(PlatformCtx {
            root_device: imp::device_of(&md),
            cluster_size: imp::cluster_size(root).unwrap_or(4096),
            exact_allocated,
            dedup_hardlinks,
            collect_owners,
        })
    }
}

/// Read the metadata for one directory entry.
///
/// `dir_path` is the directory being enumerated; it is only touched on
/// platforms and settings that need to rebuild a full path.
#[inline]
pub fn entry_meta(
    entry: &fs::DirEntry,
    file_type: fs::FileType,
    dir_path: &Path,
    ctx: &PlatformCtx,
) -> io::Result<EntryMeta> {
    imp::entry_meta(entry, file_type, dir_path, ctx)
}

/// A filesystem boundary this platform cannot see, but knows about.
///
/// The scanner declines to cross a mount by comparing `st_dev`, or on Windows
/// by recognising a volume mount point. Neither works through the WSL
/// redirector: `\\wsl.localhost\<distro>` presents a Linux filesystem and every
/// mount inside it as one flat thing, so `/proc`, `/sys` and — most expensively
/// — `/mnt`, which holds the Windows drives, are walked as if they were part of
/// the distribution. See [`wsl`] for the measurements and the list.
///
/// Everywhere else this is `false` and the device comparison does the work.
pub fn opaque_boundary(dir_path: &Path, name: &std::ffi::OsStr) -> bool {
    if !cfg!(windows) {
        return false;
    }
    // The first test is a cheap prefix check that fails on every ordinary
    // Windows path, so nothing is paid for this on a normal scan.
    match (dir_path.to_str(), name.to_str()) {
        (Some(dir), Some(name)) => wsl::is_boundary(dir, name),
        // A path or a name that is not UTF-8 cannot be either of the ASCII
        // spellings this matches, so there is nothing to recognise.
        _ => false,
    }
}

/// Metadata for the scan root itself, which is not reached through a
/// `DirEntry`.
pub fn root_meta(root: &Path, ctx: &PlatformCtx) -> io::Result<EntryMeta> {
    imp::root_meta(root, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_is_supported_exactly_where_it_is_implemented() {
        assert_eq!(supports_compression(), cfg!(windows));
    }

    /// The claim behind hiding the setting: where it costs nothing, it also
    /// changes nothing, because the number was already exact.
    ///
    /// Asserted against a real scan rather than against `cfg!`, so this fails
    /// if the Unix path ever starts deriving `allocated` instead of reading
    /// `st_blocks` — at which point the setting would need to come back.
    #[test]
    fn where_the_exact_size_is_free_it_is_already_what_you_get() {
        use crate::{ScanOptions, Scanner};

        if exact_size_costs_extra() {
            return; // Windows: it does something, and is offered.
        }

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.bin"), vec![0u8; 100_000]).unwrap();
        fs::write(dir.path().join("b.txt"), b"small").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/c.bin"), vec![7u8; 4097]).unwrap();

        let scan = |exact| {
            Scanner::new(ScanOptions {
                exact_allocated: exact,
                ..Default::default()
            })
            .scan(dir.path())
            .unwrap()
            .total_allocated()
        };

        assert_eq!(
            scan(true),
            scan(false),
            "the setting changed the answer on a platform that offers no way to turn it on"
        );
    }

    #[test]
    fn the_filesystem_is_asked_for_the_on_disk_size() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        std::fs::write(&file, vec![0u8; 64 * 1024]).unwrap();

        let size = allocated_size(&file).expect("a file that exists has a size");
        assert!(size > 0, "a 64 KiB file reported {size} bytes on disk");

        assert!(allocated_size(&dir.path().join("absent")).is_err());
    }

    /// The claim behind hiding the control: where it is not supported it says
    /// so, rather than reporting a file in zero pieces and calling that a
    /// measurement.
    #[test]
    fn fragmentation_is_reported_exactly_where_it_is_implemented() {
        assert_eq!(
            supports_fragmentation(),
            cfg!(any(windows, target_os = "linux", target_os = "macos"))
        );

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        {
            use std::io::Write as _;
            let mut output = fs::File::create(&file).unwrap();
            output.write_all(&vec![0x5a; 256 * 1024]).unwrap();
            output.sync_all().unwrap();
        }

        match fragmentation(&file) {
            Ok(found) => {
                assert!(supports_fragmentation());
                // A quarter-megabyte file just written to a temp directory is
                // almost certainly in one piece, but "almost" is not something
                // to assert on somebody else's disk. What must hold is that the
                // clusters add up to something and the file is not reported as
                // both placed and nowhere.
                if cfg!(windows) {
                    assert!(found.clusters > 0, "{found:?}");
                    assert!(found.clusters_per_extent().is_some());
                } else {
                    assert_eq!(found.mapped_bytes, Some(256 * 1024));
                    assert!(found.clusters_per_extent().is_none());
                }
                assert!(found.extents > 0, "{found:?}");
                assert!(!found.truncated, "{found:?}");
            }
            Err(error) => {
                // Either the platform has no answer, or this volume has none:
                // `supports_fragmentation` is a fact about the build and the
                // query is a fact about the filesystem under `dir`. A Windows
                // Sandbox's C: is the case that separated the two — a layered
                // virtual filesystem on a Windows that certainly supports the
                // call in general.
                assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{error}");
            }
        }
    }

    /// A file with no extents at all is the least fragmented a file can be, not
    /// a failed query. On NTFS anything under about 700 bytes lives inside its
    /// own MFT record and occupies no cluster.
    #[test]
    fn a_file_with_nothing_in_it_is_not_an_error() {
        if !supports_fragmentation() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.txt");
        fs::write(&empty, b"").unwrap();

        let found = match fragmentation(&empty) {
            Ok(found) => found,
            // The volume under a temp directory need not answer at all; see the
            // test above. What is being asserted here is the shape of the answer
            // where there is one.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            Err(error) => panic!("an empty file has an answer: {error}"),
        };
        assert_eq!(found.extents, 0);
        assert_eq!(found.clusters, 0);
        assert!(!found.is_fragmented());
        assert_eq!(found.clusters_per_extent(), None);
    }

    #[test]
    fn a_path_that_is_not_there_reports_an_error_rather_than_zero_pieces() {
        if !supports_fragmentation() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(fragmentation(&dir.path().join("absent.bin")).is_err());
    }

    /// Whatever the answer is, it has to be one of the two — and the same one
    /// twice, since nothing about a disk changes between two calls.
    #[test]
    fn the_media_under_a_real_path_answers_consistently_or_not_at_all() {
        let here = std::env::current_dir().unwrap();
        let first = media_type(&here);
        assert_eq!(first, media_type(&here));
        if !cfg!(windows) {
            assert_eq!(first, None, "nothing off Windows should claim to know");
        }
        assert!(media_type(Path::new("/definitely-not-a-volume-8f3a2b1c")).is_none());
    }

    #[test]
    fn one_piece_is_not_fragmented_and_several_are() {
        let whole = Fragmentation {
            extents: 1,
            clusters: 40,
            ..Fragmentation::default()
        };
        assert!(!whole.is_fragmented());
        assert_eq!(whole.clusters_per_extent(), Some(40.0));

        let broken = Fragmentation {
            extents: 8,
            clusters: 40,
            ..Fragmentation::default()
        };
        assert!(broken.is_fragmented());
        assert_eq!(broken.clusters_per_extent(), Some(5.0));

        // A hole is not a piece: a sparse file in one placed run is not
        // fragmented however many holes it has.
        let sparse = Fragmentation {
            extents: 1,
            clusters: 40,
            holes: 6,
            ..Fragmentation::default()
        };
        assert!(!sparse.is_fragmented());
    }

    #[test]
    fn the_volume_under_a_real_path_reports_a_sane_size() {
        let here = std::env::current_dir().unwrap();
        let usage = disk_usage(&here).expect("the volume the tests run on has a size");

        assert!(usage.total > 0);
        assert!(
            usage.free <= usage.total,
            "more free than exists: {usage:?}"
        );
        assert!(
            usage.available <= usage.free,
            "more usable than free: {usage:?}"
        );
        assert!((0.0..=1.0).contains(&usage.fraction_used()), "{usage:?}");
        assert_eq!(usage.used(), usage.total - usage.free);
    }

    #[test]
    fn a_path_that_is_not_there_reports_nothing() {
        let missing = std::path::Path::new("/definitely-not-a-volume-8f3a2b1c/x");
        assert!(disk_usage(missing).is_none());
    }

    #[test]
    fn an_empty_volume_does_not_divide_by_zero() {
        let nothing = DiskUsage {
            total: 0,
            free: 0,
            available: 0,
        };
        assert_eq!(nothing.fraction_used(), 0.0);
        assert_eq!(nothing.used(), 0);
    }

    #[test]
    fn the_fraction_is_of_used_not_free() {
        let three_quarters = DiskUsage {
            total: 400,
            free: 100,
            available: 100,
        };
        assert_eq!(three_quarters.used(), 300);
        assert!((three_quarters.fraction_used() - 0.75).abs() < f32::EPSILON);
    }
}
