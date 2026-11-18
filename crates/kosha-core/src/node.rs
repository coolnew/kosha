// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The arena node: 48 bytes, no pointers, no allocation.
//!
//! Fixed-width fields keep per-entry memory bounded for large scans.

/// Index of a node in a [`SizeTree`](crate::SizeTree)'s arena.
///
/// A `NodeId` is meaningful only for the tree that produced it. Trees are
/// immutable, so an id can never dangle into mutated data — a rescan builds a
/// new tree and callers remap by path.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    /// The root is always the first node appended.
    pub const ROOT: NodeId = NodeId(0);

    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Build an id from a raw arena index.
    ///
    /// Valid only against the tree the index came from; an out-of-range id
    /// panics on first use rather than reading someone else's node.
    #[inline]
    pub const fn from_index(index: usize) -> Self {
        NodeId(index as u32)
    }
}

/// Bit flags describing what a node is and how much we trust its numbers.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Flags(pub(crate) u8);

impl Flags {
    pub const DIR: Flags = Flags(1 << 0);
    /// A symbolic link, junction or reparse point. Never followed.
    pub const SYMLINK: Flags = Flags(1 << 1);
    /// A mount point on a different device that we did not descend into.
    pub const OTHER_DEVICE: Flags = Flags(1 << 2);
    /// Enumeration or metadata retrieval failed. Sizes below are a floor.
    pub const UNREADABLE: Flags = Flags(1 << 3);
    /// An inode already counted elsewhere in this scan; contributes zero.
    pub const HARDLINK_DUP: Flags = Flags(1 << 4);
    /// Allocated is meaningfully smaller than logical — a sparse file.
    pub const SPARSE: Flags = Flags(1 << 5);
    /// Something below this node was unreadable, so its total is a floor.
    pub const PARTIAL: Flags = Flags(1 << 6);
    /// The file's data is not on this disk: a OneDrive placeholder, or anything
    /// else behind Windows' `OFFLINE` and `RECALL_ON_*` attributes.
    ///
    /// Three consequences, and each one is a decision somewhere else:
    ///
    /// - It occupies little or nothing locally while reporting its full length,
    ///   so cluster rounding would claim space that is not being used.
    /// - **Reading it fetches it.** Anything that opens file contents has to
    ///   skip these or it will quietly download the file — which for a disk
    ///   tool means filling the disk somebody asked it to help empty.
    /// - Deleting it usually deletes the only copy, because the local entry is
    ///   the handle to the cloud one.
    ///
    /// Windows only. Unix has no portable equivalent — an HSM-backed
    /// filesystem is the nearest thing and exposes nothing standard.
    pub const OFFLINE: Flags = Flags(1 << 7);

    #[inline]
    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    /// The raw byte, for a file format that has to say what it stores.
    ///
    /// Deliberately not `From`/`Into`: these two exist so that
    /// [`crate::snapshot`] writes a documented byte rather than a memcpy of
    /// whatever the compiler laid out, and naming them says so at the call
    /// site.
    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Rebuild from a raw byte.
    ///
    /// Every bit is defined, so there is no invalid value to reject — an
    /// unknown bit from a newer kosha reads back as a flag this build does not
    /// test for, which is the right outcome.
    #[inline]
    pub const fn from_bits(bits: u8) -> Flags {
        Flags(bits)
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }

    /// Take a flag back off.
    ///
    /// One caller: a mounted volume found in the MFT is a reparse point, and
    /// therefore a link, right up until the reparse target says it is a
    /// filesystem boundary instead. The walk reaches the same conclusion by
    /// building its flags in the other order.
    #[inline]
    pub fn remove(&mut self, other: Flags) {
        self.0 &= !other.0;
    }
}

impl core::ops::BitOr for Flags {
    type Output = Flags;

    #[inline]
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

/// What [`Debug`](core::fmt::Debug) calls each flag.
///
/// Module-level rather than a local, so the static assertion below can check it
/// covers the whole byte. `OFFLINE` was missing from this list for its entire
/// life, which meant a cloud placeholder printed as `Flags(-)` — and almost
/// everything written about that flag is about *not reading these files*, so a
/// debug session into "why was this skipped" was reading output that said the
/// flag was not set. Reported by `marg`, which lifted this type and hit the
/// same shape.
const NAMED: [(Flags, &str); 8] = [
    (Flags::DIR, "DIR"),
    (Flags::SYMLINK, "SYMLINK"),
    (Flags::OTHER_DEVICE, "OTHER_DEVICE"),
    (Flags::UNREADABLE, "UNREADABLE"),
    (Flags::HARDLINK_DUP, "HARDLINK_DUP"),
    (Flags::SPARSE, "SPARSE"),
    (Flags::PARTIAL, "PARTIAL"),
    (Flags::OFFLINE, "OFFLINE"),
];

/// Every bit of the byte, named.
const fn named_bits() -> u8 {
    let (mut bits, mut i) = (0u8, 0);
    while i < NAMED.len() {
        bits |= NAMED[i].0.0;
        i += 1;
    }
    bits
}

// A test could not have caught the omission: iterating `NAMED` can only see
// what is in it, and there is no way to ask Rust for the set of constants an
// `impl` block declares. What *is* checkable is that every bit of the
// representation has a name — the byte is fully allocated, so an unnamed bit is
// either a flag somebody forgot to name or a flag that does not exist.
//
// A ninth flag means widening the repr, at which point this stops compiling
// until the new bit is named too. That is the point: the failure lands on
// whoever adds the flag rather than on whoever later reads the output.
const _: () = assert!(
    named_bits() == u8::MAX,
    "a Flags bit has no name in NAMED, so Debug would print it as nothing"
);

impl core::fmt::Debug for Flags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        f.write_str("Flags(")?;
        for (flag, name) in NAMED {
            if self.contains(flag) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("-")?;
        }
        f.write_str(")")
    }
}

/// One filesystem entry.
///
/// `#[repr(C)]` with fields ordered widest-first so the layout is exactly what
/// the documentation claims: 44 bytes of payload, 48 with tail padding. A
/// static assertion below keeps that honest.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Node {
    /// Apparent size: the length the file reports.
    pub logical: u64,
    /// Bytes actually consumed on disk, including cluster slack.
    pub allocated: u64,
    /// Index of the parent. The root is its own parent.
    pub parent: u32,
    /// Index of the first child; children occupy a contiguous run.
    pub first_child: u32,
    /// Number of direct children.
    pub child_count: u32,
    /// Files anywhere below this node, for "N items" labels.
    pub subtree_files: u32,
    /// Offset of this entry's name in the tree's name arena.
    pub name_off: u32,
    /// Modification time as Unix seconds, saturating at the epoch and 2106.
    pub mtime: u32,
    /// Length of the name in bytes.
    pub name_len: u16,
    pub flags: Flags,
    /// Depth below the root, saturating at 255.
    pub depth: u8,
}

const _: () = assert!(size_of::<Node>() == 48);

impl Node {
    #[inline]
    pub const fn is_dir(&self) -> bool {
        self.flags.contains(Flags::DIR)
    }

    #[inline]
    pub const fn is_symlink(&self) -> bool {
        self.flags.contains(Flags::SYMLINK)
    }

    /// True when this node's totals are known to be incomplete.
    #[inline]
    pub const fn is_partial(&self) -> bool {
        self.flags.contains(Flags::UNREADABLE) || self.flags.contains(Flags::PARTIAL)
    }

    /// Cluster slack and other on-disk overhead, saturating so a compressed or
    /// sparse file reports zero waste rather than an enormous wrapped number.
    #[inline]
    pub const fn wasted(&self) -> u64 {
        self.allocated.saturating_sub(self.logical)
    }

    /// Range of arena indices covering this node's direct children.
    #[inline]
    pub const fn child_range(&self) -> core::ops::Range<usize> {
        let start = self.first_child as usize;
        start..start + self.child_count as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The static assertion above guarantees every bit is named. This checks
    /// the other half — that the names actually come out — and pins the one
    /// that was missing, because the symptom was silent: `Flags(-)` is exactly
    /// what an empty set of flags prints, so nothing about the output looked
    /// wrong.
    #[test]
    fn every_flag_prints_its_own_name() {
        for (flag, name) in NAMED {
            assert_eq!(format!("{flag:?}"), format!("Flags({name})"));
        }
        assert_eq!(format!("{:?}", Flags::OFFLINE), "Flags(OFFLINE)");
    }

    #[test]
    fn no_flags_and_several_flags_both_read_clearly() {
        assert_eq!(format!("{:?}", Flags::default()), "Flags(-)");
        assert_eq!(
            format!("{:?}", Flags::DIR | Flags::UNREADABLE | Flags::OFFLINE),
            "Flags(DIR|UNREADABLE|OFFLINE)"
        );
    }

    /// Two flags sharing a bit would make `contains` answer for the wrong one,
    /// and the assertion on `named_bits` would still pass.
    #[test]
    fn no_two_flags_share_a_bit() {
        let mut seen = 0u8;
        for (flag, name) in NAMED {
            assert_eq!(flag.0.count_ones(), 1, "{name} is not a single bit");
            assert_eq!(seen & flag.0, 0, "{name} reuses a bit already taken");
            seen |= flag.0;
        }
    }
}
