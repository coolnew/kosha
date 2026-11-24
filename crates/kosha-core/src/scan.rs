// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Level-synchronous parallel walk.
//!
//! Expand one whole level of the tree in parallel, merge results in frontier order so that a
//! directory's children land in a contiguous run of arena indices, then roll
//! sizes up with a single reverse linear pass.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::Sender;
use rayon::prelude::*;
use rustc_hash::FxHashSet;

use crate::error::ScanError;
use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::owner::{OwnerId, Owners};
use crate::platform::{self, PlatformCtx};
use crate::tree::{ScanStats, SizeTree, Strategy};

/// Below this many entries, parallelising a single directory's `stat` calls
/// costs more in scheduling than it saves. Above it, one huge directory would
/// otherwise serialise the whole level.
#[cfg(unix)]
const PARALLEL_STAT_THRESHOLD: usize = 512;

/// Limit live enumeration and metadata records per directory worker.
#[cfg(unix)]
const STAT_CHUNK_ENTRIES: usize = 16384;

/// Knobs that change what a scan counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    /// Descend into mounted filesystems. Off by default: a scan of `/` should
    /// not wander onto a network share and take ten minutes.
    ///
    /// A *mounted filesystem*, never a link. On Unix that is a differing
    /// `st_dev`; on Windows it is a volume mount point, which shares its
    /// reparse tag with a junction and is told apart by what it points at.
    /// Junctions and symlinks stay unfollowed on both whatever this says,
    /// because following one is how a walk spins forever.
    pub cross_filesystems: bool,
    /// Count a hardlinked file's bytes only the first time they are seen.
    ///
    /// `None` — the default — means *wherever finding out is free*, which is
    /// not a property of the platform but of the strategy about to run. The
    /// folder walk gets an inode and a link count with the `lstat` it already
    /// does on Unix and needs an open per file on Windows, so there `None` is
    /// off; an MFT scan and every disk-image reader have both numbers in hand,
    /// so there `None` is on, on every platform.
    ///
    /// That distinction is the whole reason this is an `Option`. A plain
    /// `cfg!(unix)` default read the cost of the Windows *walk* onto strategies
    /// that do not pay it, and the same disk image then totalled differently on
    /// the two platforms — 4 KiB per hardlink, measured on an ext4 image whose
    /// export was otherwise byte-identical between them. See
    /// [`hardlinks_cost_extra`](crate::hardlinks_cost_extra).
    ///
    /// `Some(false)` is `du -l`: every name's bytes counted separately, even
    /// where counting them once was free.
    ///
    /// The default belongs to the scan strategy, rather than the host OS.
    pub dedup_hardlinks: Option<bool>,
    /// Query the exact on-disk size even where it costs an extra syscall.
    pub exact_allocated: bool,
    /// Stop descending below this depth.
    pub max_depth: Option<u8>,
    /// Read NTFS' Master File Table instead of walking directories, where that
    /// is possible.
    ///
    /// Off by default, and deliberately a *request* rather than an instruction:
    /// it needs Windows, an NTFS volume with a drive letter and an elevated
    /// process, and where any of those is missing the walk runs instead and
    /// [`ScanStats::declined`] says which one. A setting that failed the scan
    /// because the user was not an administrator would be a setting nobody
    /// could leave switched on.
    ///
    /// [`ScanStats::declined`]: crate::ScanStats::declined
    pub use_mft: bool,
    /// Record who owns each entry, so a scan can say who is using the disk.
    ///
    /// Off by default on both platforms, which is a memory decision rather than
    /// a speed one: the table is four bytes an entry, and on a machine one
    /// person uses it answers a question nobody asked. It *is* also a speed
    /// decision on Windows, where reading an owner costs a call per entry —
    /// [`owner_costs_extra`](crate::owner_costs_extra) says which platform is
    /// which, so the interface can warn only where the warning is true.
    ///
    /// Free in an MFT scan, where every record carries a security id already.
    pub collect_owners: bool,
    /// Read a WSL distribution's `ext4.vhdx` instead of going through the 9P
    /// redirector, where that is possible.
    ///
    /// Off by default, and a *request* like [`use_mft`](Self::use_mft): where
    /// the path is not a WSL distribution, or its image cannot be found or
    /// read, the ordinary walk runs and [`ScanStats::declined`] says which.
    ///
    /// Unlike the MFT this costs no privilege whatever. What it costs is
    /// freshness — a running distribution's filesystem reaches its image when
    /// Linux gets round to writing it back, so a file made moments ago may not
    /// be there yet. Immaterial for measuring a disk, and very material for
    /// finding something you just saved, which is why it is a choice rather
    /// than a default.
    ///
    /// [`ScanStats::declined`]: crate::ScanStats::declined
    pub use_image: bool,
}

// Every default is now the zero value on both platforms — which is the point:
// the one field that used to differ by platform was the one that made the same
// disk image total two ways. Written out rather than derived so the defaults are
// readable where they are documented.
#[allow(clippy::derivable_impls)]
impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            cross_filesystems: false,
            dedup_hardlinks: None,
            exact_allocated: false,
            max_depth: None,
            use_mft: false,
            collect_owners: false,
            use_image: false,
        }
    }
}

impl ScanOptions {
    /// Node depth is stored with saturation at 255; that storage limit must
    /// not become a traversal limit when the caller requested no ceiling.
    pub(crate) fn depth_limit_reached(&self, depth: u8) -> bool {
        self.max_depth.is_some_and(|limit| depth >= limit)
    }

    /// Whether this scan counts a hardlinked file's bytes once.
    ///
    /// `free` is whether the strategy about to run can tell that a file is
    /// hardlinked without paying for the answer — true for an MFT scan and for
    /// every disk-image reader, and [`hardlinks_cost_extra`] for the walk. An
    /// explicit [`dedup_hardlinks`](Self::dedup_hardlinks) overrides it either
    /// way, because `--no-hardlinks` means `du -l` and a flag that is quietly
    /// ignored where it is cheap to obey would be worse than no flag.
    ///
    /// [`hardlinks_cost_extra`]: crate::hardlinks_cost_extra
    pub fn dedups_hardlinks(&self, free: bool) -> bool {
        self.dedup_hardlinks.unwrap_or(free)
    }
}

/// A snapshot of scan progress. Published per level.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    /// Recorded file and directory counts at a committed walk level.
    pub recorded_counts: Option<(u64, u64)>,
    /// Directories read by the walk. None for record/image strategies whose
    /// progress units cannot support a directory-queue time estimate.
    pub dirs_scanned: Option<u64>,
    /// Entries recorded so far.
    pub entries: u64,
    /// Directories about to be read in the next level.
    pub dirs_pending: u64,
    /// Depth just completed.
    pub depth: u8,
}

/// Scans a directory tree into a [`SizeTree`].
pub struct Scanner {
    options: ScanOptions,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Sender<Progress>>,
    omit_wsl_mnt: bool,
}

impl Scanner {
    pub fn new(options: ScanOptions) -> Self {
        Scanner {
            options,
            cancel: None,
            progress: None,
            omit_wsl_mnt: false,
        }
    }

    /// Share a flag checked during enumeration and metadata reads as well as
    /// between levels. An in-flight filesystem call must still return first.
    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// Publish progress. Use a **bounded** channel: sends never block, and a
    /// full channel drops the update. A slow consumer must not be able to
    /// throttle the scan.
    pub fn with_progress(mut self, tx: Sender<Progress>) -> Self {
        self.progress = Some(tx);
        self
    }

    #[cfg(target_os = "linux")]
    /// Leave the WSL root's /mnt branch untraversed, even when it is an ordinary directory.
    pub(crate) fn without_wsl_mnt(mut self) -> Self {
        self.omit_wsl_mnt = true;
        self
    }

    pub fn scan(&self, root: &Path) -> Result<SizeTree, ScanError> {
        // Two faster strategies, each for a different kind of path, and both
        // can be asked for at once. Whichever applies here runs; if neither
        // does the ordinary walk runs and says why — which is the whole point
        // of the fallback. Somebody who has asked for the fast scan and is not
        // an administrator today still gets their scan.
        let mut declined: Option<crate::tree::Unavailable> = None;
        // Kept apart from `declined`, which any strategy may set. Only the
        // image strategy's reason can explain a scan root that is a file, and
        // hanging the MFT's "needs administrator" on a text file somebody
        // mistyped the name of would be worse than the message it replaced.
        let mut image_declined: Option<crate::tree::Unavailable> = None;

        // **Before canonicalising**, which matters more than it looks.
        // `canonicalize` on a `wsl.localhost` path goes through the
        // redirector, and the first touch of that path *starts the
        // distribution* — booting a virtual machine in order to work out
        // whether we can avoid talking to it. Nothing about a redirector path
        // needs resolving anyway: it is already absolute and already the
        // spelling Explorer uses.
        if self.options.use_image {
            match self.scan_by_image(root) {
                Ok(tree) => return Ok(tree),
                Err(ScanError::Cancelled) => return Err(ScanError::Cancelled),
                Err(ScanError::Strategy(reason)) => {
                    declined = Some(reason);
                    image_declined = Some(reason);
                }
                Err(other) => return Err(other),
            }
        }

        // Canonicalised before the ordinary strategies choose, because choosing
        // means asking which volume the path is on and a relative path has no
        // answer to that.
        let root_path = crate::path::canonical(root);

        for attempt in [self.options.use_mft.then_some(Strategy::MasterFileTable)]
            .into_iter()
            .flatten()
        {
            let outcome = match attempt {
                Strategy::MasterFileTable => self.scan_by_mft(&root_path),
                _ => continue,
            };
            match outcome {
                Ok(tree) => return Ok(tree),
                // A cancel is the user's answer, not a reason to start over
                // with the slower strategy.
                Err(ScanError::Cancelled) => return Err(ScanError::Cancelled),
                Err(ScanError::Strategy(reason)) => {
                    // Keep the reason worth reading. "This is not a WSL
                    // distribution" is true of `C:` and tells nobody anything;
                    // "kosha is not running as administrator" is the answer.
                    if declined.is_none_or(|kept| kept.is_wrong_place()) {
                        declined = Some(reason);
                    }
                }
                Err(other) => return Err(other),
            }
        }

        let mut tree = match self.walk(&root_path) {
            Ok(tree) => tree,
            // A disk image is a *file*, so this is the one refusal where the
            // ordinary walk has nothing useful to say and the image strategy
            // has everything. See [`explain_file_root`].
            Err(ScanError::NotADirectory(path)) => {
                return Err(explain_file_root(path, image_declined));
            }
            Err(other) => return Err(other),
        };
        tree.stats.declined = declined;
        Ok(tree)
    }

    /// The disk-image strategy.
    ///
    /// Not Windows-only, though it was written for WSL: what it reads is a file
    /// full of bytes. An ext4 image on a Mac cannot be mounted at all without
    /// buying software, and one on Linux needs root — reading it needs neither.
    fn scan_by_image(&self, root_path: &Path) -> Result<SizeTree, ScanError> {
        let publish = |progress: Progress| self.publish(progress);
        let cancelled = || self.is_cancelled();
        crate::image::scan(root_path, &self.options, &publish, &cancelled)
    }

    /// The MFT strategy, which exists only on Windows.
    #[cfg(windows)]
    fn scan_by_mft(&self, root_path: &Path) -> Result<SizeTree, ScanError> {
        let publish = |progress: Progress| self.publish(progress);
        let cancelled = || self.is_cancelled();
        crate::ntfs::scan(root_path, &self.options, &publish, &cancelled)
    }

    #[cfg(not(windows))]
    fn scan_by_mft(&self, _root_path: &Path) -> Result<SizeTree, ScanError> {
        Err(ScanError::Strategy(crate::tree::Unavailable::NotWindows))
    }

    /// The portable strategy: open every directory and read its entries.
    fn walk(&self, root_path: &Path) -> Result<SizeTree, ScanError> {
        let started = Instant::now();
        let root_path = root_path.to_path_buf();

        // The path arrived canonicalised, so it is absolute, unambiguous and
        // free of Windows' verbatim prefix, which must never reach a UI or a
        // prefix comparison. See `crate::path::simplify`.
        // The walk is the one strategy that pays for this, so it is the one
        // that asks what it costs here. Kept in a binding because the answer
        // goes into the stats as well: a comparison cannot tell two scans apart
        // by their *outcome*, since a tree with no hardlinks folds none either
        // way.
        let dedup_hardlinks = self
            .options
            .dedups_hardlinks(!crate::hardlinks_cost_extra());
        let ctx = PlatformCtx::for_root(
            &root_path,
            self.options.exact_allocated,
            dedup_hardlinks,
            self.options.collect_owners,
        )
        .map_err(|source| ScanError::Root {
            path: root_path.clone(),
            source,
        })?;

        let root_meta =
            platform::root_meta(&root_path, &ctx).map_err(|source| ScanError::Root {
                path: root_path.clone(),
                source,
            })?;

        if !root_meta.is_dir {
            return Err(ScanError::NotADirectory(root_path));
        }

        let mut nodes: Vec<Node> = Vec::with_capacity(1 << 16);
        let mut names = NameArena::with_capacity(1 << 20);
        let mut created = Vec::with_capacity(1 << 16);
        let mut owners = if self.options.collect_owners {
            Owners::with_capacity(1 << 16)
        } else {
            Owners::default()
        };
        let mut stats = ScanStats {
            dedup_hardlinks,
            cross_filesystems: self.options.cross_filesystems,
            // What was actually done, not what was asked for: where an exact
            // size costs nothing it is always taken, so the flag says nothing
            // about the numbers and this does.
            exact_allocated: !crate::platform::exact_size_costs_extra()
                || self.options.exact_allocated,
            ..ScanStats::default()
        };

        // The root's own name is only ever used as a label; `path_of` short
        // circuits for it and never joins it onto anything.
        let root_label = root_path
            .file_name()
            .unwrap_or_else(|| root_path.as_os_str());
        let (root_off, root_len) = names.push(root_label);
        nodes.push(Node {
            // Zero for the same reason every other directory's is: see the note
            // beside `if is_dir { logical = 0 }` in the walk below. Until that
            // rule existed only the root was zeroed, so a directory's own
            // entry-table bytes counted or not depending on whether it happened
            // to be the thing you pointed at.
            logical: 0,
            allocated: root_meta.allocated,
            parent: 0,
            first_child: 0,
            child_count: 0,
            subtree_files: 0,
            name_off: root_off,
            mtime: root_meta.mtime,
            name_len: root_len,
            flags: Flags::DIR,
            depth: 0,
        });
        if self.options.collect_owners {
            let slot = root_meta.owner.clone().map(|id| owners.intern(id));
            owners.push(slot);
        }
        stats.dirs = 1;
        created.push(root_meta.created.unwrap_or(u64::MAX));
        stats.entries = 1;

        // Inode identities seen so far. Consulted only during the serial merge
        // step, which keeps it lock-free.
        let mut seen_links: FxHashSet<(u64, u128)> =
            FxHashSet::with_capacity_and_hasher(0, Default::default());

        // Volumes already entered through a mount point, so one cannot be
        // walked twice. A volume can be mounted inside itself, and two can be
        // mounted inside each other — neither is exotic, both are a `mountvol`
        // away, and either would run until the depth ceiling with a tree that
        // is nonsense long before it gets there. Windows only in practice:
        // nothing else fills in `mounted_volume`.
        let mut seen_volumes: FxHashSet<u64> =
            FxHashSet::with_capacity_and_hasher(0, Default::default());

        let mut frontier: Vec<(u32, PathBuf)> = vec![(0, root_path.clone())];
        let mut depth: u8 = 0;
        let mut dirs_scanned = 0;

        while !frontier.is_empty() {
            if self.is_cancelled() {
                stats.cancelled = true;
                stats.partial = true;
                break;
            }
            if self.options.depth_limit_reached(depth) {
                // The loop's own condition is a non-empty frontier, so arriving
                // here means there are directories queued that will now never be
                // read. Every total below is therefore a floor, which is what
                // `partial` means; `depth_limited` is which of the two ways it
                // happened.
                stats.depth_limited = true;
                stats.partial = true;
                break;
            }

            let opts = self.options;
            let completed = AtomicU64::new(0);
            let entries = AtomicU64::new(stats.entries);
            let pending = AtomicU64::new(frontier.len() as u64);
            let publishing = Mutex::new(());
            // One level, read in parallel. Results come back in frontier order,
            // not completion order, which is what makes the arena deterministic.
            let batches: Vec<DirBatch> = frontier
                .par_iter()
                .map(|(_, path)| {
                    let batch = read_directory(
                        path,
                        &ctx,
                        &opts,
                        self.omit_wsl_mnt && path == Path::new("/"),
                        &|| self.is_cancelled(),
                    );
                    if self.progress.is_some() && !self.is_cancelled() {
                        entries.fetch_add(batch.entries.len() as u64, Ordering::Relaxed);
                        pending.fetch_add(
                            batch.entries.iter().filter(|e| e.descend).count() as u64,
                            Ordering::Relaxed,
                        );
                        pending.fetch_sub(1, Ordering::Relaxed);
                        let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        // Sparse, nonblocking publication, even during a broad
                        // level. Serialize publishers so counts never go back.
                        if done.is_multiple_of(64)
                            && let Ok(_guard) = publishing.try_lock()
                        {
                            self.publish(Progress {
                                recorded_counts: None,
                                dirs_scanned: Some(
                                    dirs_scanned + completed.load(Ordering::Relaxed),
                                ),
                                entries: entries.load(Ordering::Relaxed),
                                dirs_pending: pending.load(Ordering::Relaxed),
                                depth,
                            });
                        }
                    }
                    batch
                })
                .collect();

            // Workers may have stopped midway through a directory. Discard
            // this level together, preserving the preceding valid BFS prefix.
            if self.is_cancelled() {
                stats.cancelled = true;
                stats.partial = true;
                break;
            }

            let child_depth = depth.saturating_add(1);
            let mut next: Vec<(u32, PathBuf)> = Vec::new();

            for ((parent_idx, parent_path), batch) in frontier.iter().zip(batches.iter()) {
                if nodes.len() + batch.entries.len() > u32::MAX as usize {
                    return Err(ScanError::TooManyEntries {
                        reached: nodes.len() + batch.entries.len(),
                        max: u32::MAX as u64,
                    });
                }

                let name_base = names.append_raw(&batch.names);
                let first_child = nodes.len() as u32;

                // Batch-local owner slots, mapped into the tree's own table.
                // Interning happens once per distinct owner per *directory*
                // rather than once per file, and a directory's entries almost
                // always share one — so a million files cost a few thousand
                // linear searches over a list of a dozen.
                let owner_map: Vec<u32> = batch
                    .owners
                    .iter()
                    .map(|id| owners.intern(id.clone()))
                    .collect();

                // Decided here rather than in the parallel pass, because it
                // depends on every other directory read so far and that is a
                // question only the serial merge can answer.
                let mut allowed: Vec<bool> = Vec::with_capacity(batch.entries.len());

                for entry in &batch.entries {
                    let mut flags = entry.flags;
                    let mut logical = entry.logical;
                    let mut allocated = entry.allocated;

                    // Hardlink accounting: the first sighting owns the bytes.
                    if let Some(key) = entry.link_key
                        && !seen_links.insert(key)
                    {
                        flags.insert(Flags::HARDLINK_DUP);
                        logical = 0;
                        allocated = 0;
                        stats.hardlinks_deduped += 1;
                    }

                    // A volume already walked is not walked again. The
                    // entry stays in the tree with its own size, marked as the
                    // boundary it is, so the row does not vanish.
                    let mut descend = entry.descend;
                    if descend
                        && let Some(volume) = entry.mounted_volume
                        && !seen_volumes.insert(volume)
                    {
                        descend = false;
                        flags.insert(Flags::OTHER_DEVICE);
                    }
                    allowed.push(descend);

                    let is_dir = flags.contains(Flags::DIR);

                    // A directory's apparent size is its contents', never its
                    // own entry table. Unix reports a real `st_size` for a
                    // directory — 60 bytes on tmpfs, 4096 on ext4 — and NTFS
                    // reports zero, so counting it would make the same tree
                    // measure differently on the two platforms. Every row of a
                    // Windows-against-Linux export diff would then differ for
                    // reasons that have nothing to do with the content, which
                    // is the one thing that comparison exists to show.
                    //
                    // `allocated` is deliberately *not* zeroed here. A
                    // directory's blocks are real disk usage — 4 KiB each on
                    // ext4 — and a tree with a hundred thousand directories
                    // genuinely spends that. On-disk size differing between
                    // filesystems is honest; apparent size differing is not.
                    if is_dir {
                        logical = 0;
                    }

                    if is_dir {
                        stats.dirs += 1;
                    } else {
                        stats.files += 1;
                    }
                    if flags.contains(Flags::UNREADABLE) {
                        stats.unreadable += 1;
                        stats.partial = true;
                    }
                    if flags.contains(Flags::OTHER_DEVICE) {
                        stats.skipped_other_device += 1;
                    }

                    nodes.push(Node {
                        logical,
                        allocated,
                        parent: *parent_idx,
                        first_child: 0,
                        child_count: 0,
                        subtree_files: u32::from(!is_dir),
                        name_off: name_base + entry.name_off,
                        mtime: entry.mtime,
                        name_len: entry.name_len,
                        flags,
                        depth: child_depth,
                    });
                    created.push(entry.created.unwrap_or(u64::MAX));
                    if self.options.collect_owners {
                        // `None` here is an owner we were refused, which is not
                        // the same as one we did not ask for and not the same
                        // as nobody. See `crate::owner`.
                        owners.push(entry.owner.map(|local| owner_map[local as usize]));
                    }
                }

                nodes[*parent_idx as usize].first_child = first_child;
                nodes[*parent_idx as usize].child_count = batch.entries.len() as u32;
                if batch.unreadable {
                    nodes[*parent_idx as usize].flags.insert(Flags::UNREADABLE);
                    stats.unreadable += 1;
                    stats.partial = true;
                }

                // Queue the directories we are allowed to descend into.
                for (i, entry) in batch.entries.iter().enumerate() {
                    if allowed[i] {
                        let idx = first_child + i as u32;
                        // SAFETY: this span is one whole name from
                        // `as_encoded_bytes`, appended by `append_raw` above
                        // at `name_base`.
                        let name = unsafe {
                            names.get_unchecked(name_base + entry.name_off, entry.name_len)
                        };
                        next.push((idx, parent_path.join(name)));
                    }
                }

                stats.entries += batch.entries.len() as u64;
            }

            stats.max_depth = stats.max_depth.max(child_depth);
            dirs_scanned += frontier.len() as u64;
            self.publish(Progress {
                recorded_counts: Some((stats.files, stats.dirs)),
                dirs_scanned: Some(dirs_scanned),
                entries: stats.entries,
                dirs_pending: next.len() as u64,
                depth,
            });

            frontier = next;
            depth = child_depth;
        }

        aggregate(&mut nodes);

        // Once, per distinct account, after the walk — never per file. A lookup
        // can reach a domain controller over the network, and a cancelled scan
        // is a tree whose owners are still worth naming.
        owners.resolve(platform::resolve_owner);

        // The arena doubles as it grows, so it can be holding up to twice what
        // it needs. A tree is immutable once built and long-lived, so give the
        // slack back rather than carrying it for the life of the scan.
        nodes.shrink_to_fit();
        names.shrink_to_fit();
        owners.shrink_to_fit();
        created.shrink_to_fit();

        stats.bytes_logical = nodes[0].logical;
        stats.bytes_allocated = nodes[0].allocated;
        stats.duration = started.elapsed();

        // Every name span came from `as_encoded_bytes` of a name the OS gave
        // the walk, appended whole, so the tree's name invariant holds with
        // nothing to check.
        Ok(SizeTree {
            nodes,
            names,
            owners,
            created,
            root_path,
            stats,
            replaced_names: Vec::new(),
        })
    }

    #[inline]
    fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    fn publish(&self, progress: Progress) {
        if let Some(tx) = &self.progress {
            // Deliberately ignoring the error: a dropped or full receiver must
            // never slow the scan down.
            let _ = tx.try_send(progress);
        }
    }
}

/// Say something useful about a scan root that turned out to be a file.
///
/// "Not a directory" is true of every disk image ever made, and it is the
/// answer somebody gets for pointing kosha at exactly the file it was taught to
/// read — which is the one case where the reason is both knowable and thrown
/// away. The image strategy has already worked out whether there is a container,
/// whether the filesystem inside it is one kosha reads, and whether the file
/// could be opened at all; all of that is discarded when the walk refuses the
/// path a moment later.
///
/// Two answers, then, and a third that is just the old one:
///
///   - reading images was asked for and did not work — say what stopped it;
///   - it holds something kosha can name and not read — name it, because
///     asking for images would not have helped and the name is the fact that
///     ends the question;
///   - it was not asked for and the file is an image — say so, because the
///     thing to do about it is to ask rather than to find a different path;
///   - anything else is an ordinary file and the old message is right.
///
/// **The second of those is one answer rather than two.** Without it somebody
/// with a Fedora disk is told to ask for images, asks, and is then told the
/// filesystem is btrfs — two steps to a fact that was knowable at the first.
fn explain_file_root(path: PathBuf, image_declined: Option<crate::tree::Unavailable>) -> ScanError {
    // `is_wrong_place` reasons are the strategy saying "this is not my kind of
    // path", which for a file means it was never treated as an image at all.
    if let Some(reason) = image_declined.filter(|reason| !reason.is_wrong_place()) {
        return ScanError::Image { path, reason };
    }

    // Naming costs a magic number and reading costs a reader, so kosha knows
    // btrfs, LVM, LUKS and a dozen others by sight without being able to walk
    // any of them. Telling somebody *which* is the difference between a fact
    // they can act on and a sentence that sounds like their file is broken.
    if let Some(name) = crate::image::recognised_in(&path) {
        return ScanError::Image {
            path,
            reason: crate::tree::Unavailable::NamedFilesystem(name),
        };
    }

    if looks_like_an_image(&path) {
        return ScanError::ImageNotRead(path);
    }
    ScanError::NotADirectory(path)
}

/// Two reads, asked only of a path that has already failed to scan.
///
/// **This is the one thing here that happens without being asked**, so its cost
/// is worth pinning down rather than waving at: one `open` and one sector,
/// once, on a scan that is already returning an error. Never per entry, and never on
/// a scan that succeeded. Whether to read images at all stays an explicit
/// request; only working out *what a file is* happens on its own, and that is
/// the right division — nobody should have to name the filesystem inside an
/// image, and nobody should have an image opened they did not ask about.
///
/// The extension is deliberately not the question. A name is not evidence, and
/// this answer becomes an instruction — telling somebody to ask for images on a
/// file that is not one would send them a step further from the truth.
///
/// Both shapes an image comes in are checked. A VHDX says so in its first eight
/// bytes; a plain image has no container to announce itself, so the filesystem
/// is what there is to recognise, and an ext superblock sits 1024 bytes in with
/// its magic 0x38 further along. Neither costs more than a read of a file that
/// is already open.
///
/// An ISO 9660 disc says so thirty-two kilobytes in, because the space in
/// front of that belongs to whoever wrote the disc — which is exactly where a
/// bootable image puts its partition table. **A UDF volume announces itself in
/// the same place and slightly further along**: a list of six-byte headers, one
/// per block, and a disc carrying ISO 9660 as well pushes the list down past its
/// own descriptors. So this reads the list rather than one fixed offset.
///
/// **FAT is the one with no magic number**, so it is recognised by the reader
/// that will have to read it rather than by a string. That is deliberate and
/// it is the rule the same list follows inside
/// [`image`](crate::image::scan): a second opinion written here would
/// eventually disagree with the first, and the symptom would be a file offered
/// as an image and then refused as not one.
///
/// **This list is the one that has to grow** when another filesystem is read
/// from an image, or a file that kosha could have measured will be turned away
/// with the wrong sentence. It has happened twice. NTFS was read from an image
/// for a whole afternoon before anybody noticed that one handed over *without*
/// asking for images still came back "not a directory"; and a *whole-disk*
/// image — the ordinary shape, a table at the front and the filesystem a
/// megabyte in — was turned away the same way by every check here, because
/// every check here looked at offset zero. A comment saying a list must grow is
/// not the same as growing it.
///
/// **It is deliberately looser than the list in [`image`](crate::image), and
/// that is not the same hazard.** This one answers *is this file a disk image*,
/// from a magic number; that one answers *will a reader open it*, from the
/// parser that will have to. A damaged image is still an image, and should be
/// told what is wrong with it rather than told it is not a directory — so the
/// two cannot be the same check, and the one place they meet is the partition
/// table at the bottom of this function, which is read by the same parser
/// either way.
fn looks_like_an_image(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    // Reading one sector of a cloud placeholder fetches the whole file, which
    // is a poor price for choosing between two error messages.
    if platform::is_placeholder(path) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    // One sector, which is what the largest of these answers needs and is a
    // single read either way.
    //
    // **However much of one there is.** A short read is not a failure here: a
    // file of eight bytes cannot be a FAT volume and can still be the start of
    // a VHDX, and every check below is guarded by its own length rather than
    // by this read having filled the buffer.
    let mut sector = [0u8; 512];
    let mut filled = 0usize;
    while filled < sector.len() {
        match file.read(&mut sector[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return false,
        }
    }
    let start = &sector[..filled];

    if crate::vhdx::has_signature(start) {
        return true;
    }

    // XFS says so in the first four bytes of its superblock.
    if start.starts_with(b"XFSB") {
        return true;
    }

    // NTFS says so in its boot sector, eight bytes in.
    if start.len() >= 0x0B && &start[0x03..0x0B] == b"NTFS    " {
        return true;
    }

    // FAT12, FAT16, FAT32 and exFAT, by the same parser that would open one.
    if crate::fat::format::boot(start).is_ok() {
        return true;
    }

    const EXT_MAGIC_AT: u64 = 1024 + 0x38;
    const EXT_MAGIC: u16 = 0xEF53;
    let mut magic = [0u8; 2];
    if file.seek(SeekFrom::Start(EXT_MAGIC_AT)).is_ok()
        && file.read_exact(&mut magic).is_ok()
        && u16::from_le_bytes(magic) == EXT_MAGIC
    {
        return true;
    }

    // ISO 9660, whose descriptors start sixteen blocks in because the space in
    // front of them belongs to whoever wrote the disc. It is checked last of
    // the lot and it is the farthest into the file, which is the right order:
    // everything above answers from bytes already read.
    const ISO_MAGIC_AT: u64 = 16 * 2048 + 1;
    let mut magic = [0u8; 5];
    if file.seek(SeekFrom::Start(ISO_MAGIC_AT)).is_ok()
        && file.read_exact(&mut magic).is_ok()
        && &magic == b"CD001"
    {
        return true;
    }

    // UDF, in the volume recognition sequence that starts at the same block.
    //
    // **A list rather than an offset**, because a disc that carries ISO 9660
    // too puts its three descriptors first and pushes this along — which is the
    // ordinary bridge disc, and also the shape a Windows installer has. A pure
    // UDF volume puts it in the second block of the sequence. Reading the list
    // costs at most eight reads of six bytes, of a file that is already open, on
    // a scan that is already returning an error.
    for step in 0..crate::udf::format::RECOGNITION_BLOCKS {
        let at = crate::udf::format::RECOGNITION_AT + step * 2048;
        let mut header = [0u8; 6];
        // A file too short to hold the sequence, or the end of it. **`break`
        // rather than `return`**: a check that gives up has to give up on
        // itself and not on the ones after it, and writing that as a return
        // meant a file of four kilobytes could never reach the partition table
        // below — which a test caught immediately, being a file of four
        // kilobytes.
        if file.seek(SeekFrom::Start(at)).is_err() || file.read_exact(&mut header).is_err() {
            break;
        }
        if crate::udf::format::recognises_udf(&header) {
            return true;
        }
        if crate::udf::format::ends_recognition(&header) {
            break;
        }
    }

    // **And a whole disk, where none of the above can be at offset zero.**
    // `dd if=/dev/sda` puts a partition table at the front and the first
    // filesystem a megabyte in, so every check above finds nothing — and such a
    // file was turned away as "not a directory" while `--image` read it
    // completely, which is this comment's own warning happening for the second
    // time. A table is what says *this is a disk* whatever is in it.
    crate::image::is_partitioned(path)
}

/// Roll sizes up to the root in one reverse linear pass.
///
/// Breadth-first construction guarantees `child_index > parent_index` for every
/// edge, so walking backwards means every child is fully summed before its
/// parent is read. The only random access is the parent, which lies *behind*
/// the cursor and is therefore usually still in cache.
pub(crate) fn aggregate(nodes: &mut [Node]) {
    for i in (1..nodes.len()).rev() {
        let child = nodes[i];
        let parent = &mut nodes[child.parent as usize];
        parent.logical = parent.logical.saturating_add(child.logical);
        parent.allocated = parent.allocated.saturating_add(child.allocated);
        parent.subtree_files = parent.subtree_files.saturating_add(child.subtree_files);
        if child.is_partial() {
            parent.flags.insert(Flags::PARTIAL);
        }
    }
}

/// One directory's worth of results, built entirely in worker-local buffers.
struct DirBatch {
    /// Every child name in this directory, concatenated.
    names: Vec<u8>,
    entries: Vec<PendingEntry>,
    /// The distinct owners in this directory, usually one. Interned here so the
    /// merge pays for a directory's accounts rather than for its files, and so
    /// the parallel pass needs no shared table to lock.
    owners: Vec<OwnerId>,
    /// The directory itself could not be opened or fully enumerated.
    unreadable: bool,
}

impl DirBatch {
    /// This owner's index within the batch, adding it if it is new.
    fn owner_slot(&mut self, id: OwnerId) -> u32 {
        match self.owners.iter().position(|known| *known == id) {
            Some(at) => at as u32,
            None => {
                self.owners.push(id);
                (self.owners.len() - 1) as u32
            }
        }
    }
}

struct PendingEntry {
    /// Offset within this batch's `names`, rebased during the merge.
    name_off: u32,
    name_len: u16,
    flags: Flags,
    logical: u64,
    allocated: u64,
    mtime: u32,
    created: Option<u64>,
    link_key: Option<(u64, u128)>,
    /// Set only for a Windows volume mount point; see [`EntryMeta`].
    mounted_volume: Option<u64>,
    /// Index into the batch's own owner list. `None` when the scan did not ask,
    /// and also when it asked and was refused.
    owner: Option<u32>,
    descend: bool,
}

fn read_directory(
    path: &Path,
    ctx: &PlatformCtx,
    opts: &ScanOptions,
    omit_mnt: bool,
    cancelled: &(impl Fn() -> bool + Sync),
) -> DirBatch {
    let mut batch = DirBatch {
        names: Vec::new(),
        entries: Vec::new(),
        owners: Vec::new(),
        unreadable: false,
    };

    if cancelled() {
        return batch;
    }
    let Ok(read_dir) = fs::read_dir(path) else {
        batch.unreadable = true;
        return batch;
    };

    let mut raw: Vec<fs::DirEntry> = Vec::with_capacity(32);
    for entry in read_dir {
        if cancelled() {
            return batch;
        }
        match entry {
            Ok(e) => raw.push(e),
            Err(_) => batch.unreadable = true,
        }
        // Release Unix directory entries and their metadata between chunks.
        #[cfg(unix)]
        if raw.len() == STAT_CHUNK_ENTRIES {
            append_entries(&raw, path, ctx, opts, omit_mnt, cancelled, &mut batch);
            raw.clear();
        }
    }

    if !raw.is_empty() {
        append_entries(&raw, path, ctx, opts, omit_mnt, cancelled, &mut batch);
    }
    if cancelled() {
        return batch;
    }

    // Sort by name so two scans of an unchanged tree produce identical arenas.
    // Costs a sort per directory and buys diffable trees, exact tests, and a UI
    // that does not reshuffle underneath the user on rescan.
    let names = &batch.names;
    batch.entries.sort_unstable_by(|a, b| {
        let sa = &names[a.name_off as usize..][..a.name_len as usize];
        let sb = &names[b.name_off as usize..][..b.name_len as usize];
        sa.cmp(sb)
    });

    batch
}

fn append_entries(
    raw: &[fs::DirEntry],
    path: &Path,
    ctx: &PlatformCtx,
    opts: &ScanOptions,
    omit_mnt: bool,
    cancelled: &(impl Fn() -> bool + Sync),
    batch: &mut DirBatch,
) {
    let stats = stat_all(raw, path, ctx, cancelled);

    batch.names.reserve(raw.len() * 16);
    batch.entries.reserve(raw.len());

    for (entry, stat) in raw.iter().zip(stats) {
        if cancelled() {
            return;
        }
        let name = entry.file_name();
        let bytes = name.as_encoded_bytes();
        if bytes.len() > u16::MAX as usize {
            continue; // Cannot exist; see NameArena::push.
        }
        let name_off = batch.names.len() as u32;
        batch.names.extend_from_slice(bytes);
        let name_len = bytes.len() as u16;

        let mut flags = Flags::default();
        let (mut logical, mut allocated, mut mtime, mut link_key) = (0, 0, 0, None);
        let mut descend = false;
        let mut mounted_volume = None;
        let mut owner = None;
        let mut created = None;

        match stat {
            Some(meta) => {
                if meta.is_symlink {
                    // Measured as the link, never followed: the only rule that
                    // cannot loop or double-count.
                    flags.insert(Flags::SYMLINK);
                } else if meta.is_dir {
                    flags.insert(Flags::DIR);
                    // Three ways to be on another filesystem. Unix says so with
                    // a differing `st_dev`; Windows has no cheap one and says
                    // so by being a volume mount point, which `entry_meta` has
                    // already gone and asked about.
                    //
                    // The third is the one neither of those can see: through
                    // the WSL redirector a whole Linux system, mounts and all,
                    // arrives as one flat filesystem with no device to compare.
                    // `/proc` and `/mnt` are still mounts; see
                    // `platform::opaque_boundary`.
                    let crosses = meta.device != ctx.root_device
                        || meta.mounted_volume.is_some()
                        || platform::opaque_boundary(path, &name)
                        || (omit_mnt && name == "mnt");
                    if !opts.cross_filesystems && crosses {
                        flags.insert(Flags::OTHER_DEVICE);
                    } else {
                        descend = true;
                    }
                }
                mounted_volume = meta.mounted_volume;
                if meta.is_sparse {
                    flags.insert(Flags::SPARSE);
                }
                if meta.is_offline {
                    flags.insert(Flags::OFFLINE);
                }
                logical = meta.logical;
                allocated = meta.allocated;
                mtime = meta.mtime;
                created = meta.created;
                link_key = meta.link_key;
                owner = meta.owner.map(|id| batch.owner_slot(id));
            }
            None => flags.insert(Flags::UNREADABLE),
        }

        batch.entries.push(PendingEntry {
            name_off,
            name_len,
            flags,
            logical,
            allocated,
            mtime,
            created,
            link_key,
            mounted_volume,
            owner,
            descend,
        });
    }
}

/// Fetch metadata for every entry in a directory.
///
/// On Windows this is free — the data is already cached from the enumeration —
/// so it stays serial. On Unix each entry costs an `lstat`, and a directory
/// with hundreds of thousands of files would otherwise pin the whole level to
/// one thread, so past a threshold the stats are taken in parallel.
#[cfg(unix)]
fn stat_all(
    raw: &[fs::DirEntry],
    dir: &Path,
    ctx: &PlatformCtx,
    cancelled: &(impl Fn() -> bool + Sync),
) -> Vec<Option<platform::EntryMeta>> {
    let one = |e: &fs::DirEntry| {
        if cancelled() {
            return None;
        }
        let ft = e.file_type().ok()?;
        platform::entry_meta(e, ft, dir, ctx).ok()
    };
    if raw.len() >= PARALLEL_STAT_THRESHOLD {
        raw.par_iter().with_min_len(128).map(one).collect()
    } else {
        raw.iter().map(one).collect()
    }
}

#[cfg(not(unix))]
fn stat_all(
    raw: &[fs::DirEntry],
    dir: &Path,
    ctx: &PlatformCtx,
    cancelled: &(impl Fn() -> bool + Sync),
) -> Vec<Option<platform::EntryMeta>> {
    raw.iter()
        .map(|e| {
            if cancelled() {
                return None;
            }
            let ft = e.file_type().ok()?;
            platform::entry_meta(e, ft, dir, ctx).ok()
        })
        .collect()
}

/// Scan `root` with default options.
pub fn scan(root: &Path) -> Result<SizeTree, ScanError> {
    Scanner::new(ScanOptions::default()).scan(root)
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn a_large_directory_can_stop_before_enumeration_finishes() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..100 {
            fs::write(dir.path().join(format!("file-{n}")), b"x").unwrap();
        }
        let ctx = PlatformCtx::for_root(dir.path(), false, false, false).unwrap();
        let checks = AtomicUsize::new(0);
        let batch = read_directory(dir.path(), &ctx, &ScanOptions::default(), false, &|| {
            checks.fetch_add(1, Ordering::Relaxed) >= 10
        });
        assert_eq!(checks.load(Ordering::Relaxed), 11);
        assert!(
            batch.entries.is_empty(),
            "an interrupted batch is discarded"
        );
        assert!(!batch.unreadable, "cancellation is not a read error");
    }

    #[test]
    fn wsl_root_scan_omits_mnt_but_walks_home() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("home")).unwrap();
        fs::create_dir(dir.path().join("mnt")).unwrap();
        let ctx = PlatformCtx::for_root(dir.path(), false, false, false).unwrap();
        let batch = read_directory(dir.path(), &ctx, &ScanOptions::default(), true, &|| false);
        assert_eq!(batch.entries.len(), 2);
        assert!(batch.entries[0].descend, "home must be scanned");
        assert!(!batch.entries[1].descend, "mnt must be skipped");
        assert!(batch.entries[1].flags.contains(Flags::OTHER_DEVICE));
    }

    #[cfg(unix)]
    #[test]
    fn wide_directory_preserves_every_name_in_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        for n in (0..=STAT_CHUNK_ENTRIES).rev() {
            fs::File::create(dir.path().join(format!("entry-{n:05}"))).unwrap();
        }
        let tree = scan(dir.path()).unwrap();
        assert_eq!(tree.len(), STAT_CHUNK_ENTRIES + 2);
        for (n, id) in tree.children(tree.root()).enumerate() {
            assert_eq!(tree.name_lossy(id), format!("entry-{n:05}"));
        }
    }

    #[test]
    fn metadata_reads_honour_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file"), b"x").unwrap();
        let ctx = PlatformCtx::for_root(dir.path(), false, false, false).unwrap();
        let raw: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            stat_all(&raw, dir.path(), &ctx, &|| true)
                .iter()
                .all(Option::is_none)
        );
    }
}
