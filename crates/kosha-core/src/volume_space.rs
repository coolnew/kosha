// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Read-only comparison of filesystem counters and a live scan.

use std::path::{Path, PathBuf};

use crate::volume_history::VolumeId;
use crate::{DiskUsage, SizeTree, Strategy};

pub struct SpaceCapture {
    root: PathBuf,
    volume: VolumeId,
    before: Option<DiskUsage>,
    single_pool: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SpaceSummary {
    pub usage: Option<DiskUsage>,
    pub scanned_bytes: Option<u64>,
    pub scanned_trash_bytes: Option<u64>,
    pub filesystem_snapshot_count: Option<u64>,
    pub estimated: bool,
    pub partial: bool,
    pub unreadable: u64,
    pub skipped_mounts: u64,
    pub depth_limited: bool,
    pub gap: SpaceGap,
}

impl SpaceSummary {
    pub fn reported_used(&self) -> Option<u64> {
        let usage = self.usage?;
        // On Windows capacity and available-to-caller can both be quota scoped.
        // The separate volume-wide free value does not share that scope.
        usage.total.checked_sub(if cfg!(windows) {
            usage.available
        } else {
            usage.free
        })
    }

    pub fn usage_label() -> &'static str {
        if cfg!(windows) {
            "Filesystem used (caller scope)"
        } else {
            "Filesystem-reported used"
        }
    }
}

/// Reasons why the two measurements cannot be subtracted safely.
#[derive(Debug, PartialEq, Eq)]
pub enum SpaceGap {
    Unexplained(u64),
    CountersUnavailable,
    AllocationChanged,
    CoverageLimited,
    AccountingUnavailable,
    ScanExceedsUsage,
}

impl SpaceGap {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unexplained(_) => "unexplained",
            Self::CountersUnavailable => "counters_unavailable",
            Self::AllocationChanged => "allocation_changed",
            Self::CoverageLimited => "coverage_limited",
            Self::AccountingUnavailable => "accounting_unavailable",
            Self::ScanExceedsUsage => "scan_exceeds_usage",
        }
    }

    pub fn explanation(&self) -> &'static str {
        match self {
            Self::Unexplained(_) => "Filesystem usage that this scan did not account for.",
            Self::CountersUnavailable => "Filesystem counters are unavailable.",
            Self::AllocationChanged => "Filesystem usage changed during the scan.",
            Self::CoverageLimited => "Scan coverage or allocation accuracy limits the comparison.",
            Self::AccountingUnavailable if cfg!(target_os = "macos") => {
                "An APFS container can share space across volumes, and files can share blocks."
            }
            Self::AccountingUnavailable if cfg!(windows) => {
                "Windows can report used space in the caller’s quota scope."
            }
            Self::AccountingUnavailable => {
                "A comparable usage total is unavailable for this filesystem."
            }
            Self::ScanExceedsUsage => "Scanned allocation exceeds filesystem-reported usage.",
        }
    }
}

impl SpaceCapture {
    pub fn start(root: &Path) -> Option<Self> {
        let root = crate::path::canonical(root);
        let volume = VolumeId::at_root(&root)?;
        let before = crate::disk_usage(&root);
        let single_pool = single_pool(&root);
        Some(Self {
            root,
            volume,
            before,
            single_pool,
        })
    }

    /// The caller supplies the tree from the scan started after this capture.
    pub fn finish(self, tree: &SizeTree) -> Option<SpaceSummary> {
        if !matches!(
            tree.stats().strategy,
            Strategy::DirectoryWalk | Strategy::MasterFileTable | Strategy::WslNative
        ) || !crate::snapshot::store::same_root(&self.root, tree.root_path())
            || tree.stats().cancelled
        {
            return None;
        }
        let usage = crate::disk_usage(&self.root);
        let filesystem_snapshot_count = snapshot_count(&self.root);
        if VolumeId::at_root(&self.root).as_ref() != Some(&self.volume) {
            return None;
        }
        let stats = tree.stats();
        let scanned_bytes = (!tree.root_unreadable()).then(|| tree.total_allocated());
        let partial = tree.is_partial() || tree.root_unreadable();
        let estimated = !stats.exact_allocated;
        let comparable = !partial
            && stats.unreadable == 0
            && !stats.depth_limited
            && !stats.cross_filesystems
            && stats.dedup_hardlinks
            && !estimated;
        let gap = classify_gap(
            self.before,
            usage,
            scanned_bytes,
            comparable,
            self.single_pool,
        );
        Some(SpaceSummary {
            usage,
            scanned_bytes,
            scanned_trash_bytes: if stats.cross_filesystems {
                None
            } else {
                scanned_trash(tree)
            },
            estimated,
            filesystem_snapshot_count,
            partial,
            unreadable: stats.unreadable,
            skipped_mounts: stats.skipped_other_device,
            depth_limited: stats.depth_limited,
            gap,
        })
    }
}

#[cfg(target_os = "macos")]
fn snapshot_count(root: &Path) -> Option<u64> {
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        fn fs_snapshot_list(
            dirfd: libc::c_int,
            attributes: *mut libc::attrlist,
            buffer: *mut libc::c_void,
            size: usize,
            flags: u32,
        ) -> libc::c_int;
    }

    let directory = std::fs::File::open(root).ok()?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_NAME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut records = [0u64; 512];
    let mut count = 0;
    // The native API advances the open directory's cursor. Bound work even if
    // snapshots change concurrently or a malformed filesystem repeats a page.
    for _ in 0..128 {
        // SAFETY: the directory stays open, the attribute mask is initialized,
        // and the aligned writable buffer has the byte length passed here.
        let page = unsafe {
            fs_snapshot_list(
                directory.as_raw_fd(),
                &raw mut attributes,
                records.as_mut_ptr().cast(),
                size_of_val(&records),
                0,
            )
        };
        match page {
            ..=-1 => return None,
            0 => return Some(count),
            positive => count += positive as u64,
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn snapshot_count(_: &Path) -> Option<u64> {
    None
}

fn scanned_trash(tree: &SizeTree) -> Option<u64> {
    use crate::node::Flags;

    tree.children(tree.root())
        .filter_map(|id| {
            let node = tree.node(id);
            let name = tree.name_lossy(id);
            let recognized = if cfg!(windows) {
                name.eq_ignore_ascii_case("$Recycle.Bin")
            } else if cfg!(target_os = "macos") {
                name == ".Trashes"
            } else {
                name == ".Trash"
                    || name
                        .strip_prefix(".Trash-")
                        .is_some_and(|uid| uid.parse::<u32>().is_ok())
            };
            (recognized
                && node.is_dir()
                && !node.is_symlink()
                && !node.flags.contains(Flags::OTHER_DEVICE)
                && !node.flags.contains(Flags::UNREADABLE))
            .then_some(node.allocated)
        })
        .reduce(u64::saturating_add)
}

fn classify_gap(
    before: Option<DiskUsage>,
    after: Option<DiskUsage>,
    scanned: Option<u64>,
    comparable: bool,
    single_pool: bool,
) -> SpaceGap {
    let (Some(before), Some(after)) = (before, after) else {
        return SpaceGap::CountersUnavailable;
    };
    if after.total == 0 || after.free > after.total {
        return SpaceGap::CountersUnavailable;
    }
    if before.total != after.total || before.free != after.free {
        return SpaceGap::AllocationChanged;
    }
    if !comparable || scanned.is_none() {
        return SpaceGap::CoverageLimited;
    }
    if !single_pool {
        return SpaceGap::AccountingUnavailable;
    }
    match scanned.and_then(|size| after.used().checked_sub(size)) {
        Some(gap) => SpaceGap::Unexplained(gap),
        None => SpaceGap::ScanExceedsUsage,
    }
}

// A whole ext filesystem is currently the only accounting scope we verify.
// APFS shared containers and Windows caller-scoped capacity require their own
// accounting probes before subtraction makes sense.
#[cfg(target_os = "linux")]
fn single_pool(root: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = CString::new(root.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: path is NUL-terminated and stat points to writable storage.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // Linux assigns 0xef53 to ext2/3/4. Compare a literal so its type follows
    // the signed glibc or unsigned musl statfs field.
    (unsafe { libc::statfs(path.as_ptr(), &raw mut stat) }) == 0 && stat.f_type == 0xef53
}

#[cfg(not(target_os = "linux"))]
fn single_pool(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_back_free_space_is_not_part_of_the_gap() {
        let usage = DiskUsage {
            total: 100,
            free: 30,
            available: 20,
        };
        assert_eq!(
            classify_gap(Some(usage), Some(usage), Some(60), true, true),
            SpaceGap::Unexplained(10)
        );
    }

    #[test]
    fn withhold_gap_when_counters_coverage_or_scope_do_not_support_it() {
        let usage = DiskUsage {
            total: 100,
            free: 30,
            available: 20,
        };
        let changed = DiskUsage { free: 29, ..usage };
        for (before, after, scanned, comparable, pool, expected) in [
            (
                None,
                Some(usage),
                Some(60),
                true,
                true,
                SpaceGap::CountersUnavailable,
            ),
            (
                Some(usage),
                Some(changed),
                Some(60),
                true,
                true,
                SpaceGap::AllocationChanged,
            ),
            (
                Some(usage),
                Some(usage),
                Some(60),
                false,
                true,
                SpaceGap::CoverageLimited,
            ),
            (
                Some(usage),
                Some(usage),
                Some(60),
                true,
                false,
                SpaceGap::AccountingUnavailable,
            ),
            (
                Some(usage),
                Some(usage),
                Some(80),
                true,
                true,
                SpaceGap::ScanExceedsUsage,
            ),
        ] {
            assert_eq!(
                classify_gap(before, after, scanned, comparable, pool),
                expected
            );
        }
    }
}
