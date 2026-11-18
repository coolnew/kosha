// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The immutable result of a scan.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::names::NameArena;
use crate::node::{Flags, Node, NodeId};
use crate::owner::Owners;

/// How a tree was produced.
///
/// Recorded on every scan and shown in the window, because the two strategies
/// can legitimately disagree about what exists: the MFT sees NTFS' own metadata
/// files, which no directory listing shows, and sees inside directories an
/// ordinary user has no permission to open. A tree whose provenance is not
/// stated is a tree nobody can reconcile against another one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Strategy {
    /// The portable walk: open each directory, read its entries.
    #[default]
    DirectoryWalk,
    /// NTFS' Master File Table, read as a raw stream. Windows, elevated, NTFS.
    MasterFileTable,
    /// A WSL distribution's `ext4.vhdx`, read as a filesystem rather than
    /// through the 9P redirector. Windows, and no privilege at all.
    /// Read out of a disk image: a VHDX, a raw image, a partition of one.
    /// Named for the shape of the source rather than for the filesystem in
    /// it, because it is now more than one.
    DiskImage,
}

impl Strategy {
    /// What to call it in the interface.
    pub fn label(self) -> &'static str {
        match self {
            Strategy::DirectoryWalk => "Directory walk",
            Strategy::MasterFileTable => "Master File Table",
            Strategy::DiskImage => "Disk image",
        }
    }

    /// The same thing, spelled to sit inside a sentence.
    ///
    /// Not `label().to_lowercase()`, which is what the command line used to do
    /// to the whole provenance line — and which turned `WSL` into `wsl`, `NTFS`
    /// into `ntfs` and `GPT` into `gpt`. A name that is an initialism does not
    /// stop being one because of where it appears, and lowercasing by rule is
    /// how every one of them got quietly misspelled at once.
    pub fn in_a_sentence(self) -> &'static str {
        match self {
            Strategy::DirectoryWalk => "directory walk",
            // A proper noun: NTFS's own name for the file, and correct
            // capitalised wherever it stands.
            Strategy::MasterFileTable => "Master File Table",
            Strategy::DiskImage => "disk image",
        }
    }

    /// The inverse, for reading a snapshot back.
    ///
    /// Anything unrecognised is the ordinary walk. A snapshot outlives the
    /// build that wrote it, and a strategy added later should read back as the
    /// plainest true thing rather than refusing the file — what the strategy
    /// was is a note on a past scan, not something the tree depends on.
    pub fn from_label(label: &str) -> Strategy {
        match label {
            "Master File Table" => Strategy::MasterFileTable,
            "Disk image" => Strategy::DiskImage,
            _ => Strategy::DirectoryWalk,
        }
    }
}

/// Why a faster strategy could not run.
///
/// Carried rather than collapsed into a bool so the interface can say which
/// wall it hit. "Reading the MFT is unavailable" is not an answer anybody can
/// act on; "kosha is not running as administrator" is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unavailable {
    #[error("reading the Master File Table is a Windows feature")]
    NotWindows,
    #[error("this path has no drive letter, so its volume cannot be opened directly")]
    NoDriveLetter,
    #[error("this volume is not NTFS, and only NTFS has a Master File Table")]
    NotNtfs,
    #[error("reading a volume directly needs kosha to be running as administrator")]
    NotElevated,
    #[error("the volume could not be read")]
    Unreadable,
    #[error("this path is not inside a WSL distribution")]
    NotWsl,
    /// The path handed to the image strategy is not a file, so there is nothing
    /// to open. Separate from [`NotWsl`](Unavailable::NotWsl) because reading
    /// images is not a Windows feature and never was — only finding a
    /// distribution's image by name is — and "not a WSL distribution" is a
    /// baffling thing to be told about a directory on a Mac.
    #[error("this path is not a disk image")]
    NotAnImagePath,
    #[error("this distribution's disk image could not be found")]
    NoImage,
    #[error("the disk image could not be read")]
    ImageUnreadable,
    // Reached only for a path that *was* chosen as an image, so it never
    // means "this strategy is for other paths" — it means this file was
    // handed over as a disk image and is not one.
    #[error("there is no filesystem kosha reads in this file")]
    NotAnImage,
    // The container opened and the filesystem inside it did not. Worth its own
    // reason rather than folding into `ImageUnreadable`, because the two ask
    // for completely different things from whoever reads them: one is a broken
    // or truncated file, the other is a perfectly good image of a filesystem
    // kosha does not read. A WSL data disk formatted `xfs` is the ordinary way
    // to arrive here, and being told "could not be read" would send somebody
    // looking for corruption that is not there.
    //
    // Named for what it is rather than for what it is not: this said "is not
    // ext4" until NTFS was read too, at which point a true sentence became a
    // misleading one — somebody with an `xfs` disk would have gone looking for
    // an ext4 option that was never the point.
    #[error("the disk image opened, but the filesystem inside it is not one kosha reads")]
    UnknownFilesystem,
    // The same refusal as [`UnknownFilesystem`](Unavailable::UnknownFilesystem)
    // with the one fact that makes it actionable: *which* filesystem. Naming
    // costs a magic number and reading costs a reader, so the list of things
    // kosha can name is far longer than the list it can read — see
    // [`recognise`](crate::recognise).
    //
    // The distinction is the whole lesson of the `xfs` refusal: "kosha cannot
    // read this" sends somebody looking for corruption, and "this is btrfs"
    // sends them to a tool that reads btrfs. Fedora and openSUSE both default
    // to btrfs, so this is not a corner.
    #[error("the disk image holds {0}, which kosha does not read")]
    NamedFilesystem(&'static str),
    // A UDF volume that announced itself and then could not be read, with which
    // of the two ways it happened. Reached only after an anchor was found, so
    // "this is UDF" is certain and the sentence can afford to say it.
    //
    // Its own refusal rather than [`NamedFilesystem`](Unavailable::NamedFilesystem)
    // with "UDF" in it, because that sentence ends "which kosha does not read" —
    // and kosha reads UDF. That is exactly the misleading kind of true sentence
    // the comment above is about.
    #[error("the disk image holds UDF, and {0}")]
    Udf(&'static str),
    #[error("the path is not in the distribution's disk image")]
    NotInImage,
}

impl Unavailable {
    /// Whether this means "that strategy is not for this kind of path" rather
    /// than "it is, and something stopped it".
    ///
    /// A scan can be offered both faster strategies at once, and one of them is
    /// always in the wrong place: the MFT has no drive letter to open on a WSL
    /// path, and a disk image is not what `C:` is. Reporting *that* as the
    /// reason a scan was ordinary would bury the reason that is actually
    /// interesting.
    pub fn is_wrong_place(self) -> bool {
        matches!(
            self,
            Unavailable::NotWindows
                | Unavailable::NoDriveLetter
                | Unavailable::NotWsl
                | Unavailable::NotAnImagePath
        )
    }

    /// A short form for a status line, where the sentence above is too long.
    pub fn short(self) -> &'static str {
        match self {
            Unavailable::NotWindows => "Windows only",
            Unavailable::NoDriveLetter => "no drive letter",
            Unavailable::NotNtfs => "not NTFS",
            Unavailable::NotElevated => "needs administrator",
            Unavailable::Unreadable => "volume unreadable",
            Unavailable::NotWsl => "not a WSL distribution",
            Unavailable::NotAnImagePath => "not a disk image",
            Unavailable::NoImage => "no disk image",
            Unavailable::ImageUnreadable => "image unreadable",
            Unavailable::NotAnImage => "no filesystem in it",
            Unavailable::UnknownFilesystem => "unsupported filesystem",
            // The name itself, because that is the whole point of the variant
            // and a status line with room for "unsupported filesystem" has
            // room for "btrfs".
            Unavailable::NamedFilesystem(name) => name,
            // "UDF" and then the reason, because a status line with room for
            // "unsupported filesystem" has room for what it really was.
            Unavailable::Udf(reason) => reason,
            Unavailable::NotInImage => "not in the image",
        }
    }
}

/// What a scan cost and how much it could see.
#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    /// Which of the two strategies produced this tree.
    pub strategy: Strategy,
    /// Set when the MFT was asked for and the walk ran instead, with the
    /// reason. `None` when it was not asked for, or when it ran.
    pub declined: Option<Unavailable>,
    /// What was found inside a disk image, and what was chosen.
    ///
    /// `Some("ext4")` for a plain filesystem image;
    /// `Some("NTFS · partition 2 of 2 (GPT)")` for one filesystem out of
    /// several on a disk. `None` for every scan that did not read an image.
    ///
    /// Both halves answer a question the strategy's own name cannot. "Disk
    /// image" does not say which filesystem was read, and on a dual-boot disk
    /// "partition 2 of 3" does not say whether that is the Windows one or the
    /// Linux one — which is exactly the ambiguity this exists to remove, since
    /// a number nobody can trace back to a decision is a number nobody can
    /// check.
    pub image: Option<String>,
    pub entries: u64,
    pub dirs: u64,
    pub files: u64,
    /// Entries whose metadata or contents we could not read.
    pub unreadable: u64,
    /// Directories skipped because they live on another device.
    pub skipped_other_device: u64,
    /// Hardlinked files whose bytes were already counted elsewhere.
    pub hardlinks_deduped: u64,
    /// Whether on-disk sizes were measured rather than estimated.
    ///
    /// The third rule of the same kind, and the one that hid longest because
    /// it is invisible on the platform where it does not apply. On Unix
    /// `st_blocks` arrives with the `lstat` the scan already does, so the
    /// figure is exact whether or not anybody asks. On Windows the exact
    /// answer costs `GetCompressedFileSizeW` and an open per file, so
    /// ordinary files are estimated from the cluster size unless `--exact`
    /// was given — and for a file small enough to live inside the Master File
    /// Table the estimate is not close, it is a different number:
    ///
    /// ```text
    ///            estimated     exact
    /// f1.bin          4096         8
    /// f64.bin         4096        64
    /// f300.bin        4096       304
    /// f600.bin        4096       600
    /// ```
    ///
    /// So a record taken one way and a scan taken the other disagree about
    /// every small file in the tree. Measured: a snapshot taken with
    /// `--exact`, held against an ordinary scan of the same untouched
    /// directory, reported four files as having *grown* and `Net +15.0 KiB
    /// across files`.
    ///
    /// **The effective rule, not the flag.** On Unix this is always true, so
    /// a Linux record compared against an estimating Windows scan is flagged
    /// too — which is right, because those two really do disagree.
    pub exact_allocated: bool,
    /// Whether this scan descended into other filesystems.
    ///
    /// The same kind of fact as `dedup_hardlinks` below, and the same failure
    /// without it. `skipped_other_device` is how many boundaries were *not*
    /// crossed, which is zero on a tree with none whichever rule was used — so
    /// a record taken with `--cross-fs` and a scan without it disagree by
    /// every byte on every mount inside the tree, and nothing could tell them
    /// apart. Measured on `/run`, which has a handful of mounts in it: the
    /// comparison reported every file on them as *gone*.
    pub cross_filesystems: bool,
    /// Whether this scan counted a hardlinked file's bytes once.
    ///
    /// **The rule, not the outcome.** `hardlinks_deduped` is how many were
    /// folded, which is zero on a tree that has none whichever rule was used —
    /// so it cannot tell two scans apart, and two scans of one unchanged tree
    /// under different rules disagree by every hardlinked byte in it. Measured:
    /// five names for one 8 MiB file, snapshotted with the rule on and compared
    /// against a scan with it off, reported **32 MiB of growth across four files
    /// that nothing had touched**.
    ///
    /// Which rule applies is the strategy's to decide, so it is not
    /// knowable from the options alone: the walk on Windows pays a syscall per
    /// file for the identity and declines by default, and every other strategy
    /// gets it for nothing and dedups. That makes an elevated `--mft` snapshot
    /// and an ordinary walk of the same folder two different rules on one
    /// machine, which is the sequence this is here to let a comparison notice.
    pub dedup_hardlinks: bool,
    pub bytes_logical: u64,
    pub bytes_allocated: u64,
    pub max_depth: u8,
    pub duration: Duration,
    /// Totals are a lower bound: something below them was never looked at.
    ///
    /// Two ways to get here. Something was unreadable — see `unreadable` — or
    /// the walk stopped at its depth ceiling, which is `depth_limited` below.
    /// Both mean the same thing about every number in this struct, which is why
    /// they set the same flag, and the front ends say which one happened.
    pub partial: bool,
    /// The depth ceiling stopped the walk with directories still queued.
    ///
    /// **A depth-limited total is a floor and said nothing about being one.**
    /// `--depth 1` on a tree holding 884 KiB reported 4.00 KiB, with no mark of
    /// any kind — and `du --max-depth=1`, which is the thing people know, counts
    /// everything and only *shows* one level. Worse than the number on screen:
    /// a snapshot of such a scan recorded it as complete, so a later full scan
    /// compared against it reported every directory below the ceiling as **new**
    /// and 880 KiB as growth that never happened. A snapshot carries `partial`
    /// to warn that a comparison may report changes that never happened,
    /// and this was the one way of becoming
    /// partial that never set it.
    pub depth_limited: bool,
    /// The caller cancelled; the tree is a valid prefix of a full scan.
    pub cancelled: bool,
}

impl ScanStats {
    /// How this tree was produced, and — when it is not what was asked for —
    /// why not. One line, to follow "read from the ".
    ///
    /// **Both front ends say this, and they say it from here.** The window used
    /// to build its own and it drifted twice over: it left out what was found
    /// inside an image, and it lowercased the strategy by rule, which turned
    /// `Master File Table` into `master file table`. There was a
    /// `provenance` beside this one that did the same lowercasing and that
    /// nothing called; it is gone, so that the next caller gets the spelling
    /// [`Strategy::in_a_sentence`] was written to protect.
    pub fn provenance_in_a_sentence(&self) -> String {
        self.describe(self.strategy.in_a_sentence())
    }

    fn describe(&self, strategy: &str) -> String {
        let mut line = match self.declined {
            Some(reason) => format!("{strategy} · {}", reason.short()),
            None => strategy.to_string(),
        };
        if let Some(image) = &self.image {
            line.push_str(" · ");
            line.push_str(image);
        }
        line
    }
}

/// A scanned directory tree: two flat vectors and a root path.
///
/// Immutable by construction. Every accessor is a bounds-checked index into an
/// arena, so there is no pointer chasing anywhere in the read path.
pub struct SizeTree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) names: NameArena,
    /// Who owns each node, when the scan was asked to find out. Empty
    /// otherwise, and four bytes an entry when it is not — which is why it is
    /// a side table rather than a field on the 48-byte node.
    pub(crate) owners: Owners,
    /// Unix seconds; u64::MAX means unknown. Empty for older records or readers
    /// without birth times. Kept outside the 48-byte arena node.
    pub(crate) created: Vec<u64>,
    pub(crate) root_path: PathBuf,
    pub(crate) stats: ScanStats,
}

impl SizeTree {
    #[inline]
    pub fn root(&self) -> NodeId {
        NodeId::ROOT
    }

    /// The absolute path the scan started from.
    #[inline]
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    #[inline]
    pub fn stats(&self) -> &ScanStats {
        &self.stats
    }

    /// The scan stopped before exhausting its scope, independently of access errors.
    pub fn is_incomplete(&self) -> bool {
        self.stats.cancelled || self.stats.depth_limited
    }

    /// Record a Stop request received as the worker was finishing. Call before
    /// publishing the tree; a stopped result must never replace a drive record.
    pub fn mark_cancelled(&mut self) {
        self.stats.cancelled = true;
        self.stats.partial = true;
    }

    /// The whole arena, for writing a snapshot.
    #[inline]
    pub fn nodes_slice(&self) -> &[Node] {
        &self.nodes
    }

    /// The name arena's bytes, for the same.
    #[inline]
    pub fn names_bytes(&self) -> &[u8] {
        self.names.as_bytes()
    }

    /// Rebuild a tree from parts that came out of a file rather than a disk.
    ///
    /// Every invariant the accessors assume — a parent above its child,
    /// children inside the arena, a name inside the name arena — is the
    /// caller's to have checked. The scanner cannot produce a bad arena; a
    /// reader can, which is why `snapshot::validate` exists and runs before
    /// this is called.
    pub fn from_parts(
        nodes: Vec<Node>,
        names: NameArena,
        owners: Owners,
        root_path: PathBuf,
        stats: ScanStats,
    ) -> SizeTree {
        SizeTree {
            nodes,
            names,
            owners,
            created: Vec::new(),
            root_path,
            stats,
        }
    }

    /// Number of entries, including the root.
    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    /// The whole arena, for analysis passes that want a linear scan.
    #[inline]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Who owns what, when the scan collected it.
    ///
    /// Always present and empty when it did not, rather than an `Option`: every
    /// caller wants to ask "was this collected" before anything else, and
    /// [`Owners::were_collected`] is that question asked once rather than an
    /// unwrap at each use.
    #[inline]
    pub fn owners(&self) -> &Owners {
        &self.owners
    }

    /// Recorded filesystem creation time, never inferred from modification time.
    pub fn created(&self, id: NodeId) -> Option<u64> {
        self.created
            .get(id.index())
            .copied()
            .filter(|&time| time != u64::MAX)
    }

    pub fn creation_times(&self) -> &[u64] {
        &self.created
    }

    /// Attach an arena-aligned side table. Unknown entries use u64::MAX.
    pub fn with_creation_times(mut self, times: Vec<u64>) -> Self {
        assert!(times.is_empty() || times.len() == self.len());
        self.created = times;
        self
    }

    /// This entry's own name, not its path.
    #[inline]
    pub fn name(&self, id: NodeId) -> &OsStr {
        let n = &self.nodes[id.index()];
        self.names.get(n.name_off, n.name_len)
    }

    /// The name as UTF-8, replacing anything unrepresentable. For display only
    /// — never build a path out of this.
    pub fn name_lossy(&self, id: NodeId) -> std::borrow::Cow<'_, str> {
        self.name(id).to_string_lossy()
    }

    /// Direct children, in the deterministic order the scan produced (sorted by
    /// name, directories and files interleaved).
    #[inline]
    pub fn children(&self, id: NodeId) -> impl ExactSizeIterator<Item = NodeId> + '_ {
        self.nodes[id.index()].child_range().map(NodeId::from_index)
    }

    /// Children ordered largest-on-disk first, which is what every view wants.
    ///
    /// Sorting a contiguous run of indices rather than a linked list is the
    /// whole point of the arena layout. Ties break on name so the order is
    /// stable across rescans.
    pub fn children_by_size(&self, id: NodeId) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.children(id).collect();
        ids.sort_unstable_by(|&a, &b| {
            let (na, nb) = (&self.nodes[a.index()], &self.nodes[b.index()]);
            nb.allocated
                .cmp(&na.allocated)
                .then_with(|| nb.logical.cmp(&na.logical))
                .then_with(|| self.name(a).cmp(self.name(b)))
        });
        ids
    }

    /// Rebuild the full path of a node by walking parent pointers to the root.
    ///
    /// `O(depth)` with one allocation. Paths are not stored — for a deep tree
    /// they would dwarf the arena, and almost none of them are ever needed.
    pub fn path_of(&self, id: NodeId) -> PathBuf {
        if id == NodeId::ROOT {
            return self.root_path.clone();
        }
        // Depth is bounded at 255 and is realistically under 20.
        let mut chain: Vec<u32> = Vec::with_capacity(self.nodes[id.index()].depth as usize + 1);
        let mut cur = id.0;
        while cur != 0 {
            chain.push(cur);
            cur = self.nodes[cur as usize].parent;
        }
        let mut path = self.root_path.clone();
        for &i in chain.iter().rev() {
            let n = &self.nodes[i as usize];
            path.push(self.names.get(n.name_off, n.name_len));
        }
        path
    }

    /// Walk from the root down to `id`, root first, `id` last.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = vec![id];
        let mut cur = id.0;
        while cur != 0 {
            cur = self.nodes[cur as usize].parent;
            chain.push(NodeId(cur));
        }
        chain.reverse();
        chain
    }

    /// Total on-disk bytes below the root.
    #[inline]
    pub fn total_allocated(&self) -> u64 {
        self.nodes.first().map_or(0, |n| n.allocated)
    }

    /// Total apparent bytes below the root.
    #[inline]
    pub fn total_logical(&self) -> u64 {
        self.nodes.first().map_or(0, |n| n.logical)
    }

    /// This node's share of its parent, 0.0 to 1.0. Used for the inline
    /// proportion bars and treemap layout.
    pub fn fraction_of_parent(&self, id: NodeId) -> f32 {
        let node = &self.nodes[id.index()];
        let parent = &self.nodes[node.parent as usize];
        if parent.allocated == 0 || id == NodeId::ROOT {
            return 0.0;
        }
        node.allocated as f32 / parent.allocated as f32
    }

    /// Find a direct child by exact name.
    ///
    /// The scan sorts each directory's children by encoded name bytes, and
    /// `OsStr` orders by those same bytes, so this is a binary search over a
    /// contiguous slice rather than a scan. Rules that ask "is there a
    /// `Cargo.toml` next to this `target/`?" do it once per candidate.
    pub fn child_named(&self, parent: NodeId, name: &OsStr) -> Option<NodeId> {
        let range = self.nodes[parent.index()].child_range();
        let found = self.nodes[range.clone()]
            .binary_search_by(|n| self.names.get(n.name_off, n.name_len).cmp(name))
            .ok()?;
        Some(NodeId::from_index(range.start + found))
    }

    /// Whether a sibling of `id` carries this name — the marker-file test that
    /// keeps cache rules honest.
    pub fn has_sibling_named(&self, id: NodeId, name: &OsStr) -> bool {
        if id == NodeId::ROOT {
            return false;
        }
        let parent = NodeId::from_index(self.nodes[id.index()].parent as usize);
        self.child_named(parent, name).is_some()
    }

    /// Resolve a path back to a node, so a rescan can restore the selection.
    pub fn find(&self, path: &Path) -> Option<NodeId> {
        let rest = path.strip_prefix(&self.root_path).ok()?;
        let mut cur = NodeId::ROOT;
        for component in rest.components() {
            let name = component.as_os_str();
            cur = self.children(cur).find(|&c| self.name(c) == name)?;
        }
        Some(cur)
    }

    /// Approximate resident size of this tree, for the memory readout.
    pub fn memory_bytes(&self) -> usize {
        self.nodes.capacity() * size_of::<Node>()
            + self.names.bytes()
            + self.owners.memory_bytes()
            + self.created.capacity() * size_of::<u64>()
    }

    /// The scan could not look inside the folder it was pointed at.
    ///
    /// **Not the same thing as an empty folder, and it was being shown as
    /// one.** Measured on `C:\System Volume Information`, which an ordinary
    /// account cannot open: the window drew "This folder is empty." in the
    /// middle of the treemap, and the command line reported `~0 B on disk`
    /// and exited 0. One line in each said otherwise — a `1 unreadable` in
    /// the status bar, a "1 entry could not be read" above the table — and
    /// neither is the answer to "what is in here".
    ///
    /// A distinct question from [`is_partial`](Self::is_partial), which is
    /// true when *anything* below was missed and leaves the totals meaningful
    /// as a floor. Here there is no floor and no total: nothing was read at
    /// all, and what a front end should say is that, not a number.
    #[inline]
    pub fn root_unreadable(&self) -> bool {
        self.node(self.root()).flags.contains(Flags::UNREADABLE)
    }

    /// True when any part of the tree could not be read, so totals are a floor.
    #[inline]
    pub fn is_partial(&self) -> bool {
        self.stats.partial
    }
}

impl std::fmt::Debug for SizeTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SizeTree")
            .field("root", &self.root_path)
            .field("nodes", &self.nodes.len())
            .field("allocated", &self.total_allocated())
            .field("partial", &self.stats.partial)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(strategy: Strategy) -> ScanStats {
        ScanStats {
            strategy,
            ..ScanStats::default()
        }
    }

    /// The one line both front ends print, and the two ways it went wrong.
    ///
    /// **A name that is an initialism does not stop being one because of where
    /// it appears.** The command line lowercased this whole line once, which
    /// turned `WSL` into `wsl` and `NTFS` into `ntfs`; the window did the same
    /// to the strategy alone, long after `in_a_sentence` had been written to
    /// stop it, because it composed its own line instead of asking for this
    /// one.
    #[test]
    fn a_strategy_that_is_a_proper_noun_keeps_its_capitals() {
        assert_eq!(
            stats(Strategy::MasterFileTable).provenance_in_a_sentence(),
            "Master File Table"
        );
        assert_eq!(
            stats(Strategy::DirectoryWalk).provenance_in_a_sentence(),
            "directory walk"
        );
        assert_eq!(
            stats(Strategy::DiskImage).provenance_in_a_sentence(),
            "disk image"
        );
    }

    /// What was found inside an image is part of the line, because "disk image"
    /// does not say which filesystem — and on a disk with more than one it does
    /// not say which of them was chosen.
    #[test]
    fn what_was_found_in_an_image_is_part_of_the_line() {
        let mut found = stats(Strategy::DiskImage);
        found.image = Some("UDF · revision 2.50, metadata partition".into());
        assert_eq!(
            found.provenance_in_a_sentence(),
            "disk image · UDF · revision 2.50, metadata partition"
        );

        // And a declined strategy says why, before it says what it found —
        // which for a scan that fell back to the walk is nothing.
        let mut declined = stats(Strategy::DirectoryWalk);
        declined.declined = Some(Unavailable::NotElevated);
        assert_eq!(
            declined.provenance_in_a_sentence(),
            "directory walk · needs administrator"
        );
    }

    /// Every strategy reads back from the label a snapshot keeps, so a tree
    /// written today says the same thing when it is read next year.
    #[test]
    fn every_strategy_survives_a_snapshot() {
        for strategy in [
            Strategy::DirectoryWalk,
            Strategy::MasterFileTable,
            Strategy::DiskImage,
        ] {
            assert_eq!(Strategy::from_label(strategy.label()), strategy);
        }
    }
}
