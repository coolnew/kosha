// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Finding files that are byte-for-byte identical.
//!
//! This is the one part of analysis that reads file *contents*, so its cost is
//! proportional to bytes rather than to entry count. It lives outside the rule
//! engine and runs only when asked.
//!
//! Three passes, each seeing only what survived the last:
//!
//! 1. Group by exact size. No I/O at all, and it discards well over 95% of
//!    files, because almost nothing shares an exact byte count by accident.
//! 2. Hash the first and last 16 KiB. Two small reads. Files that share a size
//!    but differ almost always differ near their head.
//! 3. Hash the whole file, in parallel, and only then report a match.
//!
//! Hardlinks are excluded throughout: they already share their storage, so
//! "deduplicating" them frees nothing and deleting one is pure risk.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crossbeam_channel::Sender;
use kosha_core::{Flags, NodeId, SizeTree};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::rules::util;

const EDGE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct DuplicateOptions {
    /// Files below this size are ignored. Deduplicating thumbnails is not worth
    /// anybody's disk I/O.
    pub min_size: u64,
    /// Stop after this many candidate files, so a pathological tree cannot turn
    /// into an unbounded read of the whole volume.
    pub max_candidates: usize,
}

impl Default for DuplicateOptions {
    fn default() -> Self {
        DuplicateOptions {
            min_size: 1024 * 1024,
            max_candidates: 200_000,
        }
    }
}

/// A set of files with identical contents.
#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    /// Every copy, largest arena index last. All have identical content.
    pub nodes: Vec<NodeId>,
    /// The shared logical size of one copy.
    pub size: u64,
    /// On-disk bytes freed by keeping one copy and removing the rest.
    pub reclaimable: u64,
}

/// Result of a duplicate hunt.
#[derive(Clone, Debug, Default)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    /// Files that reached the hashing passes.
    pub candidates_examined: usize,
    /// Bytes actually read from disk, so the UI can explain what that took.
    pub bytes_hashed: u64,
    /// Files we could not open or read; excluded rather than guessed at.
    pub unreadable: usize,
    /// Files skipped because their data is not on this disk.
    ///
    /// Counted and reported rather than silently dropped: somebody with a large
    /// cloud folder should be told why their duplicate hunt covered less than
    /// they expected, instead of concluding there are no duplicates in it.
    pub not_local: usize,
    /// Files skipped for being smaller than [`DuplicateOptions::min_size`], and
    /// the floor that skipped them.
    ///
    /// By far the commonest reason a duplicate is not reported, and the one the
    /// user is least likely to guess: a folder of identical 200 KB photographs
    /// comes back "no identical files found" under a 1 MiB floor. The cloud and
    /// unreadable counts are already surfaced on exactly this argument; this is
    /// the same argument about a larger number.
    pub below_min: usize,
    /// The floor that was applied, so a report can name it without being handed
    /// the options as well.
    pub min_size: u64,
    /// True when the caller stopped the hunt before it had looked at
    /// everything.
    ///
    /// The groups below are still real — a confirmed match is confirmed
    /// whenever it was found — but the *absence* of a group no longer means
    /// anything, and a report that did not say so would be read as a complete
    /// answer. The same rule the scanner follows for a partial tree.
    pub stopped: bool,
}

impl DuplicateReport {
    /// Total on-disk bytes recoverable across every group.
    pub fn total_reclaimable(&self) -> u64 {
        self.groups
            .iter()
            .map(|g| g.reclaimable)
            .fold(0u64, u64::saturating_add)
    }
}

/// How far a hunt has got.
///
/// Counted in *files*, not bytes, and that is deliberate. Bytes look like the
/// honest denominator because the cost is bytes, but they cannot be one: pass 2
/// reads 32 KiB of a file whatever its size, so a hunt that finishes having
/// read a twentieth of the candidate bytes is the normal case, not a stalled
/// one. Files resolved out of files to resolve is monotonic, reaches its
/// total, and is the thing a bar can honestly be drawn from. Bytes are reported
/// beside it as what it *cost*, which is a different sentence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Candidate files whose group has been resolved.
    pub files_done: u64,
    /// Candidate files in total. Known before any file is opened, because
    /// pass 1 is pure arithmetic over the arena.
    pub files_total: u64,
    /// Bytes read from disk so far.
    pub bytes_read: u64,
}

impl Progress {
    /// How far along, 0.0 to 1.0. `None` before the total is known or when
    /// there is nothing to do, where a bar would be a lie either way.
    pub fn fraction(&self) -> Option<f32> {
        if self.files_total == 0 {
            return None;
        }
        Some((self.files_done as f32 / self.files_total as f32).clamp(0.0, 1.0))
    }
}

/// A duplicate hunt that can be watched and stopped.
///
/// Shaped like [`kosha_core::Scanner`] on purpose: the two are the only
/// long-running jobs in the application, the window drives them the same way,
/// and a second spelling for the same idea is a second thing to learn.
pub struct Hunt {
    options: DuplicateOptions,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Sender<Progress>>,
}

impl Hunt {
    pub fn new(options: DuplicateOptions) -> Self {
        Hunt {
            options,
            cancel: None,
            progress: None,
        }
    }

    /// Share a flag the caller can set to stop the hunt. Checked before each
    /// group, so it takes effect within one group rather than at the end — and
    /// a group is a handful of files, not the whole tree.
    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// Publish progress. Use a **bounded** channel: sends never block and a
    /// full one drops the update, so a slow window cannot throttle the hunt.
    pub fn with_progress(mut self, tx: Sender<Progress>) -> Self {
        self.progress = Some(tx);
        self
    }

    pub fn run(&self, tree: &SizeTree) -> DuplicateReport {
        run(
            tree,
            &self.options,
            self.cancel.as_deref(),
            self.progress.as_ref(),
        )
    }
}

/// Find duplicate file contents under a scanned tree.
///
/// The plain form: runs to completion, tells nobody how it is getting on. See
/// [`Hunt`] for the form the window uses.
pub fn find(tree: &SizeTree, opts: &DuplicateOptions) -> DuplicateReport {
    run(tree, opts, None, None)
}

fn run(
    tree: &SizeTree,
    opts: &DuplicateOptions,
    cancel: Option<&AtomicBool>,
    progress: Option<&Sender<Progress>>,
) -> DuplicateReport {
    let mut report = DuplicateReport {
        min_size: opts.min_size,
        ..DuplicateReport::default()
    };

    // Pass 1 — group by exact logical size. Pure arithmetic over the arena.
    let mut by_size: FxHashMap<u64, Vec<NodeId>> = FxHashMap::default();
    for index in 0..tree.len() {
        let id = NodeId::from_index(index);
        let node = tree.node(id);
        if node.is_dir() || !util::is_countable(tree, id) {
            continue;
        }
        // Counted separately from the other exclusions, because this one is a
        // setting rather than a fact about the file: it is the only reason a
        // hunt came up empty that the user can do something about.
        if node.logical < opts.min_size {
            report.below_min += 1;
            continue;
        }
        // A cloud placeholder reports a size but holds no bytes here, and
        // reading one *fetches* it. Hashing a folder of them would download
        // the lot — filling the disk somebody asked this tool to help empty,
        // over their connection, because they clicked "find duplicates".
        //
        // Skipped before the size pass, so they are never even candidates.
        if node.flags.contains(Flags::OFFLINE) {
            report.not_local += 1;
            continue;
        }
        by_size.entry(node.logical).or_default().push(id);
    }
    by_size.retain(|_, group| group.len() > 1);

    let mut candidates: Vec<(u64, Vec<NodeId>)> = by_size.into_iter().collect();
    // Deterministic order, and the biggest wins are examined first so the cap
    // truncates the least valuable candidates rather than an arbitrary slice.
    candidates.sort_unstable_by_key(|(size, _)| std::cmp::Reverse(*size));

    let mut budget = opts.max_candidates;
    candidates.retain(|(_, group)| {
        if budget == 0 {
            return false;
        }
        budget = budget.saturating_sub(group.len());
        true
    });
    report.candidates_examined = candidates.iter().map(|(_, g)| g.len()).sum();

    // The denominator, known before a single file is opened — pass 1 was pure
    // arithmetic. Published once straight away so the window can draw a bar
    // that starts at zero of a real total rather than growing one.
    let files_total = report.candidates_examined as u64;
    let done = AtomicU64::new(0);
    let read = AtomicU64::new(0);
    let publish = |progress: Option<&Sender<Progress>>, p: Progress| {
        if let Some(tx) = progress {
            let _ = tx.try_send(p);
        }
    };
    publish(
        progress,
        Progress {
            files_done: 0,
            files_total,
            bytes_read: 0,
        },
    );

    let stopped = || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed));

    // Passes 2 and 3 — narrow by edge hash, then confirm by full hash. Both
    // run across all groups at once so one huge group cannot serialise the job.
    let outcome: GroupOutcome = candidates
        .par_iter()
        .map(|(size, group)| {
            // Before the work, not after: the point of a stop is to not do the
            // reads still ahead of it.
            if stopped() {
                return GroupOutcome::default();
            }
            let paths: Vec<(NodeId, PathBuf)> =
                group.iter().map(|&id| (id, tree.path_of(id))).collect();
            let outcome = resolve_group(*size, &paths, tree);

            // One update per group. Groups are small and numerous, so this is
            // frequent enough for a bar to move smoothly and cheap enough not
            // to matter: a bounded channel drops what a slow reader cannot
            // keep up with.
            let files = done.fetch_add(group.len() as u64, Ordering::Relaxed) + group.len() as u64;
            let bytes = read.fetch_add(outcome.bytes_read, Ordering::Relaxed) + outcome.bytes_read;
            publish(
                progress,
                Progress {
                    files_done: files,
                    files_total,
                    bytes_read: bytes,
                },
            );
            outcome
        })
        .reduce(GroupOutcome::default, GroupOutcome::merge);

    report.stopped = stopped();

    report.groups = outcome.groups;
    report.bytes_hashed = outcome.bytes_read;
    report.unreadable = outcome.unreadable;

    // A set that frees nothing is not a finding. Two empty files are identical
    // and deleting one recovers no bytes, so listing them is an offer to act
    // with no effect — the same objection as a control that does nothing.
    //
    // Only reachable once the floor is lowered far enough to admit them, which
    // the default does not. That is the floor hiding the case rather than the
    // code handling it, and a hidden case is one that comes back.
    report.groups.retain(|group| group.reclaimable > 0);

    report.groups.sort_unstable_by(|a, b| {
        b.reclaimable
            .cmp(&a.reclaimable)
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.nodes.first().cmp(&b.nodes.first()))
    });
    report
}

/// What examining one same-size group produced, including what it cost.
///
/// The I/O counters ride alongside the groups rather than being attached to
/// them, because a group that turns out to hold no duplicates still read every
/// file in it. Folding the cost into the results would lose exactly that case —
/// which is the common one — and the report would claim no I/O for a hunt that
/// spent seconds reading.
#[derive(Default)]
struct GroupOutcome {
    groups: Vec<DuplicateGroup>,
    bytes_read: u64,
    unreadable: usize,
}

impl GroupOutcome {
    fn merge(mut self, other: GroupOutcome) -> GroupOutcome {
        self.groups.extend(other.groups);
        self.bytes_read = self.bytes_read.saturating_add(other.bytes_read);
        self.unreadable += other.unreadable;
        self
    }
}

/// Narrow one same-size group down to sets of genuinely identical files.
fn resolve_group(size: u64, paths: &[(NodeId, PathBuf)], tree: &SizeTree) -> GroupOutcome {
    let mut outcome = GroupOutcome::default();

    // Pass 2: cheap edge hash.
    let mut by_edge: FxHashMap<[u8; 32], Vec<NodeId>> = FxHashMap::default();
    for (id, path) in paths {
        // Pass 1 already keeps placeholders out, so this is the second lock on
        // the same door, at the one place that opens a file: nothing that
        // reaches `File::open` is allowed to be a cloud placeholder, whatever
        // a caller or a later edit to pass 1 does.
        if tree.node(*id).flags.contains(Flags::OFFLINE) {
            continue;
        }
        match hash_edges(path, size) {
            Ok((digest, read)) => {
                outcome.bytes_read += read;
                by_edge.entry(digest).or_default().push(*id);
            }
            // Excluded rather than guessed at: a file we cannot read is not a
            // file we should propose deleting a copy of.
            Err(_) => outcome.unreadable += 1,
        }
    }
    by_edge.retain(|_, group| group.len() > 1);
    if by_edge.is_empty() {
        return outcome;
    }

    // Pass 3: full hash, only for what is still plausibly identical.
    let lookup: FxHashMap<NodeId, &PathBuf> = paths.iter().map(|(id, p)| (*id, p)).collect();

    for (_, group) in by_edge {
        // When the edge hash already covered every byte, it *is* the full hash
        // and a second read would tell us nothing.
        if edges_cover_everything(size) {
            outcome.groups.push(finish(tree, group, size));
            continue;
        }

        let mut by_full: FxHashMap<[u8; 32], Vec<NodeId>> = FxHashMap::default();
        for id in group {
            let Some(path) = lookup.get(&id) else {
                continue;
            };
            if tree.node(id).flags.contains(Flags::OFFLINE) {
                continue;
            }
            match hash_whole(path) {
                Ok((digest, read)) => {
                    outcome.bytes_read += read;
                    by_full.entry(digest).or_default().push(id);
                }
                Err(_) => outcome.unreadable += 1,
            }
        }
        for (_, confirmed) in by_full {
            if confirmed.len() > 1 {
                outcome.groups.push(finish(tree, confirmed, size));
            }
        }
    }

    outcome
}

fn finish(tree: &SizeTree, mut nodes: Vec<NodeId>, size: u64) -> DuplicateGroup {
    nodes.sort_unstable();
    // Keeping one copy means every other copy's on-disk bytes are recoverable.
    let reclaimable = nodes
        .iter()
        .skip(1)
        .map(|&id| tree.node(id).allocated)
        .fold(0u64, u64::saturating_add);
    DuplicateGroup {
        nodes,
        size,
        reclaimable,
    }
}

/// True when [`hash_edges`] read every byte of a file this size, so its digest
/// is conclusive and the full-file pass can be skipped.
///
/// The head covers `min(EDGE_BYTES, size)`, and the tail covers another
/// `min(EDGE_BYTES, size)` anchored at the end. Together they span the whole
/// file exactly when it is no larger than two windows — overlapping in the
/// middle for sizes between one and two windows, which costs a little
/// double-hashing and no correctness.
#[inline]
fn edges_cover_everything(size: u64) -> bool {
    size <= (2 * EDGE_BYTES) as u64
}

/// Open a path only if it is a regular file, and ask before opening it.
///
/// **A FIFO never answers.** Two of them in a scanned directory have the same
/// size — zero — so they are one same-size group, and this hunt opened the
/// first one and stopped there: `kosha <dir> --dupe-min 0` ran forever, no
/// output, no progress, because `open` on a FIFO waits for a writer that may
/// never come. Measured on Linux with `mkfifo`; the default 1 MiB floor is all
/// that hides it, and the floor is a setting.
///
/// The question is asked of the path rather than of the handle for the reason
/// the hang exists: by the time there is a handle it is too late. Asked again
/// of the handle afterwards, which is what makes the answer about the file
/// actually read — a path that changed under us between the two is a race this
/// cannot block on, only decline.
///
/// `ErrorKind::InvalidInput` rather than a bespoke error because the caller's
/// rule is already right for this: a file it could not read is a file it will
/// not propose deleting a copy of.
fn open_regular(path: &std::path::Path) -> std::io::Result<File> {
    let not_one = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file, so nothing to hash",
        )
    };
    if !std::fs::metadata(path)?.is_file() {
        return Err(not_one());
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(not_one());
    }
    Ok(file)
}

/// Hash the first and last window of a file.
///
/// The tail is read whenever the file is larger than one window. Reading it
/// only past *two* windows would leave a gap that [`edges_cover_everything`]
/// nonetheless claims was covered — which reported two 32 KiB files sharing
/// only their first 16 KiB as identical. In a tool that offers to delete one of
/// them, that is the worst bug this module can have; the regression test is
/// `differing_only_in_the_unread_span_is_not_a_duplicate`.
fn hash_edges(path: &std::path::Path, size: u64) -> std::io::Result<([u8; 32], u64)> {
    let mut file = open_regular(path)?;
    let mut hasher = blake3::Hasher::new();
    let window = EDGE_BYTES.min(size as usize).max(1);
    let mut buf = vec![0u8; window];

    let head = read_fully(&mut file, &mut buf)?;
    hasher.update(&buf[..head]);
    let mut read = head as u64;

    if size > EDGE_BYTES as u64 {
        file.seek(SeekFrom::End(-(window as i64)))?;
        let tail = read_fully(&mut file, &mut buf)?;
        hasher.update(&buf[..tail]);
        read += tail as u64;
    }

    // Mixing the length in means two files cannot collide across sizes even if
    // their edges match, and it distinguishes a file that shrank since the scan
    // from one that did not.
    hasher.update(&size.to_le_bytes());
    Ok((*hasher.finalize().as_bytes(), read))
}

fn hash_whole(path: &std::path::Path) -> std::io::Result<([u8; 32], u64)> {
    let mut file = open_regular(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut read = 0u64;
    loop {
        let n = read_fully(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        read += n as u64;
    }
    Ok((*hasher.finalize().as_bytes(), read))
}

/// Fill `buf` unless the file ends first.
///
/// `Read::read` may legally return fewer bytes than asked for even when more
/// are available. Treating a short read as end-of-file would hash different
/// spans of two identical files and report them as different — the harmless
/// direction, but still wrong, and it would be maddening to diagnose.
fn read_fully(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}
