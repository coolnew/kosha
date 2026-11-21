// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Finding large files the filesystem could not lay down in one piece.
//!
//! Like [`duplicates`](crate::duplicates), and for the same reason, this is
//! **not a rule**. Rules are pure functions of the finished tree — no I/O, no
//! clock, no randomness — and that is what makes the engine reproducible and
//! testable against a synthetic tree. How many pieces a file is in has to be
//! asked of the filesystem, one open per file, so it lives out here and is run
//! separately.
//!
//! Three things keep the cost honest, because this runs on its own after a scan
//! rather than being asked for:
//!
//! 1. **Only on a disk with a head that has to move.** On solid state a
//!    fragmented file costs a pointer dereference rather than a seek, and
//!    telling somebody to go and defragment an SSD wears it out for nothing.
//!    One query settles it for the whole volume before a single file is opened.
//! 2. **Only files worth seeking across.** A fragmented 4 KiB file is a fact
//!    with no consequence. The floor is in [`Options::min_size`].
//! 3. **Only the biggest of them.** Candidates are taken largest-first up to
//!    [`Options::max_candidates`], so the work is bounded by a constant rather
//!    than by how many files somebody has.
//!
//! What comes out is a `ReviewOnly` finding that reclaims nothing, which the
//! rail already understands — "deep nesting" and "a dominant child" are the
//! same shape. It is an observation, and kosha offers no button for it: the
//! defragmenter is Windows', and on a spinning disk it is already scheduled.

use kosha_core::{Flags, MediaType, Node, NodeId, SizeTree};
use rayon::prelude::*;

use crate::insight::{Confidence, Insight, RuleId, Severity, Suggested};

pub const RULE: RuleId = RuleId::new("fragmented-files");

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Files below this are not worth measuring. A fragmented thumbnail is a
    /// fact with no consequence attached.
    pub min_size: u64,
    /// How many pieces before a file is worth naming.
    ///
    /// One is perfect and a handful is ordinary — a filesystem that never
    /// split a file would be one that never filled up. This is the point where
    /// reading the file stops being a read and starts being a search.
    pub min_pieces: u64,
    /// The most files to open, taken largest-first. A hard ceiling rather than
    /// a proportion, so the cost of this pass does not grow with the volume.
    pub max_candidates: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            min_size: 64 * 1024 * 1024,
            min_pieces: 16,
            max_candidates: 256,
        }
    }
}

/// Whether this entry is one worth opening.
///
/// A free function over a [`Node`] rather than a closure inside [`candidates`],
/// so every exclusion can be tested against a node built to have exactly the
/// property in question — a real symlink needs developer mode to create and a
/// real cloud placeholder needs a cloud provider.
pub fn is_candidate(node: &Node, options: &Options) -> bool {
    // A directory index has extents of its own, but a fragmented directory
    // index is not something anybody can act on.
    !node.is_dir()
        && node.allocated >= options.min_size
        // A link is measured as itself and whatever it points at is somebody
        // else's file. A second name reports bytes already counted under the
        // first. A cloud placeholder has no pieces on this disk at all — and
        // opening one fetches it, which is the outcome that flag exists to
        // prevent.
        && !node.flags.contains(Flags::SYMLINK)
        && !node.flags.contains(Flags::HARDLINK_DUP)
        && !node.flags.contains(Flags::OFFLINE)
}

/// One file and how many pieces it is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fragmented {
    pub node: NodeId,
    pub pieces: u64,
    /// On-disk bytes. Not reclaimable — carried so the worst offenders can be
    /// ordered by how much of the disk is being sought across.
    pub allocated: u64,
}

/// Which files in `tree` are worth measuring at all.
///
/// Separated from the measuring so the choice is testable without a disk, which
/// is where every decision that could be wrong actually lives: the measuring is
/// one ioctl and has nothing to get wrong.
pub fn candidates(tree: &SizeTree, options: &Options) -> Vec<NodeId> {
    let mut found: Vec<NodeId> = tree
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| is_candidate(node, options))
        .map(|(index, _)| NodeId::from_index(index))
        .collect();

    // Largest first, so a ceiling keeps the files that matter rather than
    // whichever happened to be scanned first. Ties break on index to stay
    // deterministic.
    found.sort_unstable_by(|&a, &b| {
        tree.node(b)
            .allocated
            .cmp(&tree.node(a).allocated)
            .then(a.cmp(&b))
    });
    found.truncate(options.max_candidates);
    found
}

/// Measure the candidates and report the ones in too many pieces.
///
/// Returns `None` when there is nothing to say, which is the common case and
/// includes every solid-state volume. A rail card that appears on every scan to
/// report that things are fine is a card nobody reads.
pub fn find(tree: &SizeTree, options: &Options) -> Option<Insight> {
    // An imported listing's root names a place on another machine. Asking this
    // machine about that path — its media type, then its files' extents —
    // would read a local path that only shares the name.
    if tree.stats().strategy == kosha_core::Strategy::Imported {
        return None;
    }
    // Asked once for the whole scan, before anything is opened. On solid state
    // this is where the pass ends.
    if kosha_core::media_type(tree.root_path()) != Some(MediaType::Spinning) {
        return None;
    }

    let mut worst: Vec<Fragmented> = candidates(tree, options)
        .into_par_iter()
        .filter_map(|node| {
            let found = kosha_core::fragmentation(&tree.path_of(node)).ok()?;
            (found.extents >= options.min_pieces).then_some(Fragmented {
                node,
                pieces: found.extents,
                allocated: tree.node(node).allocated,
            })
        })
        .collect();

    if worst.is_empty() {
        return None;
    }
    // Most pieces first: that is the ordering the finding is about, and the
    // panel takes from the front.
    worst.sort_unstable_by(|a, b| b.pieces.cmp(&a.pieces).then(a.node.cmp(&b.node)));

    Some(report(&worst))
}

/// Turn measurements into the finding.
///
/// Split out so the wording and the shape are testable without a disk.
pub fn report(worst: &[Fragmented]) -> Insight {
    Insight {
        rule: RULE,
        title: "Large files scattered across the disk".into(),
        detail: "Each of these is stored in many separate pieces, so reading one \
                 means seeking between them. It costs no extra space, and \
                 Windows' own defragmenter is what fixes it — on a drive like \
                 this it is normally already scheduled."
            .into(),
        subject: "file".into(),
        nodes: worst.iter().map(|f| f.node).collect(),
        superseded: 0,
        // Nothing is recovered by acting, and saying otherwise would put this
        // above real findings in a panel ranked by what it frees. The rail
        // already renders a finding worth showing that frees nothing.
        reclaimable: 0,
        confidence: Confidence::Certain,
        action: Suggested::ReviewOnly,
        severity: Severity::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kosha_core::{ScanOptions, Scanner};

    fn tree_of(dir: &std::path::Path) -> SizeTree {
        Scanner::new(ScanOptions::default()).scan(dir).unwrap()
    }

    /// The floor exists so a fragmented thumbnail is never offered as a
    /// problem: the seek matters in proportion to how much is being read.
    #[test]
    fn small_files_are_not_candidates_however_broken_they_are() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tiny.bin"), vec![0u8; 4096]).unwrap();
        std::fs::write(dir.path().join("big.bin"), vec![0u8; 512 * 1024]).unwrap();

        let options = Options {
            min_size: 256 * 1024,
            ..Options::default()
        };
        let tree = tree_of(dir.path());
        let picked = candidates(&tree, &options);

        assert_eq!(picked.len(), 1);
        assert_eq!(tree.name_lossy(picked[0]), "big.bin");
    }

    /// The ceiling is what keeps this from growing with the volume, and it has
    /// to keep the *largest* files rather than the first ones it met.
    #[test]
    fn the_ceiling_keeps_the_biggest_rather_than_the_first() {
        let dir = tempfile::tempdir().unwrap();
        for (name, size) in [("a", 1), ("b", 9), ("c", 3), ("d", 7)] {
            std::fs::write(dir.path().join(name), vec![0u8; size * 64 * 1024]).unwrap();
        }

        let options = Options {
            min_size: 0,
            max_candidates: 2,
            ..Options::default()
        };
        let tree = tree_of(dir.path());
        let names: Vec<String> = candidates(&tree, &options)
            .iter()
            .map(|&id| tree.name_lossy(id).into_owned())
            .collect();

        assert_eq!(names, vec!["b", "d"]);
    }

    /// A node with exactly one property set, so each exclusion is tested for
    /// its own reason rather than incidentally.
    fn node(allocated: u64, flags: Flags) -> Node {
        Node {
            logical: allocated,
            allocated,
            parent: 0,
            first_child: 0,
            child_count: 0,
            subtree_files: 1,
            name_off: 0,
            mtime: 0,
            name_len: 0,
            flags,
            depth: 1,
        }
    }

    /// Three kinds of entry whose piece count would be a lie or a download: a
    /// link is measured as itself, a second name is bytes already counted, and
    /// a cloud placeholder has no pieces here and is *fetched* by being opened.
    #[test]
    fn links_second_names_and_placeholders_are_never_opened() {
        let options = Options {
            min_size: 1024,
            ..Options::default()
        };
        let big = 1 << 20;

        assert!(is_candidate(&node(big, Flags::default()), &options));

        for (flags, why) in [
            (Flags::SYMLINK, "a link is measured as the link"),
            (Flags::HARDLINK_DUP, "already counted under another name"),
            (Flags::OFFLINE, "opening a placeholder downloads it"),
        ] {
            assert!(
                !is_candidate(&node(big, flags), &options),
                "{why}: {flags:?} was offered as a candidate"
            );
        }

        assert!(
            !is_candidate(&node(big, Flags::DIR), &options),
            "a directory index is not something anybody can act on"
        );
        assert!(
            !is_candidate(&node(1023, Flags::default()), &options),
            "below the floor"
        );
        // Exactly at the floor is in: the boundary belongs to the side that
        // says "big enough to be worth a seek".
        assert!(is_candidate(&node(1024, Flags::default()), &options));
    }

    /// A directory index has extents too, and a fragmented one is not something
    /// anybody can act on.
    #[test]
    fn directories_are_not_candidates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("inner")).unwrap();
        std::fs::write(dir.path().join("inner/file.bin"), vec![0u8; 128 * 1024]).unwrap();

        let tree = tree_of(dir.path());
        let options = Options {
            min_size: 0,
            ..Options::default()
        };
        for id in candidates(&tree, &options) {
            assert!(!tree.node(id).is_dir(), "{}", tree.name_lossy(id));
        }
    }

    /// The finding has to be honest about reclaiming nothing, or it outranks
    /// real ones in a panel ordered by what it frees.
    #[test]
    fn the_finding_frees_nothing_and_offers_no_button() {
        let insight = report(&[
            Fragmented {
                node: NodeId::from_index(3),
                pieces: 940,
                allocated: 8 << 30,
            },
            Fragmented {
                node: NodeId::from_index(7),
                pieces: 22,
                allocated: 1 << 30,
            },
        ]);

        assert_eq!(insight.reclaimable, 0);
        assert_eq!(insight.score(), 0.0);
        assert_eq!(insight.action, Suggested::ReviewOnly);
        assert!(!insight.action.is_destructive());
        assert_eq!(insight.len(), 2);
        // The same rule every other finding follows: no counts or byte figures
        // baked into prose the engine may later recompute around.
        assert!(!insight.title.contains(char::is_numeric));
        assert!(!insight.detail.contains(char::is_numeric));
    }
}
