// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Building a [`SizeTree`] from entries that arrive in depth-first order.
//!
//! Every reader inside this crate builds its arena breadth-first as it walks.
//! A listing written by another tool does not walk anything: an ncdu export is
//! nested, so a streaming parse sees each entry once, in depth-first order,
//! with its parent already known. This turns that sequence into the arena the
//! rest of kosha assumes — breadth-first, each directory's children contiguous
//! and sorted by name, every child after its parent — and then runs the same
//! aggregation pass and the same structural checks a snapshot is held to.
//!
//! **Names are `&str` only.** UTF-8 is a valid name on every platform, so a
//! tree built here never needs a name replaced, and a caller outside this
//! crate never touches the arena's unchecked constructors.

use std::path::PathBuf;

use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::owner::Owners;
use crate::tree::{ScanStats, SizeTree};

/// What one entry measures, before aggregation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Entry {
    /// Apparent size. Ignored for a directory, whose apparent size is what
    /// its contents add up to, as in every other reader.
    pub logical: u64,
    /// On-disk size. For a directory this is its own allocation, and its
    /// contents are added to it.
    pub allocated: u64,
    /// Modification time as Unix seconds; zero means unknown.
    pub mtime: u32,
    /// `Flags::DIR` makes it a directory that can have children.
    pub flags: Flags,
}

/// Why an entry could not be added, or a tree could not be finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    #[error("a name that is empty, `.` or `..`, or holds `/`, a NUL or a `\\..` step")]
    InvalidName,
    #[error("a name longer than {} bytes", u16::MAX)]
    NameTooLong,
    #[error("an entry whose parent is not a directory already in the tree")]
    BadParent,
    #[error("more entries than a tree can hold")]
    TooManyEntries,
    #[error("more name bytes than a tree can address")]
    NamesTooLarge,
    #[error("the finished tree failed its own checks: {0}")]
    Invalid(&'static str),
}

/// Whether `name` may be one component of an imported path.
///
/// Refused on every platform alike, so one listing imports the same way
/// everywhere: empty, `.` and `..`, anything holding `/` or NUL, and anything
/// that becomes a `.` or `..` step when `\` is read as a separator — which
/// is what a Windows path does with it.
pub fn is_valid_component(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.contains('/') || name.contains('\0') {
        return false;
    }
    !name.split('\\').any(|step| step == "." || step == "..")
}

#[derive(Clone, Copy)]
struct Record {
    logical: u64,
    allocated: u64,
    parent: u32,
    name_off: u32,
    mtime: u32,
    name_len: u16,
    flags: Flags,
}

/// Depth-first entries in, a [`SizeTree`] out.
pub struct TreeBuilder {
    records: Vec<Record>,
    names: Vec<u8>,
}

/// What [`TreeBuilder::finish`] noticed while laying out the arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// Entries whose name an earlier sibling already had. Kept as separate
    /// entries, never merged.
    pub duplicate_names: u64,
}

impl TreeBuilder {
    /// Start a tree whose root has this name (shown in the breadcrumb) and
    /// these measurements. The root is always a directory.
    pub fn new(root_name: &str, mut root: Entry) -> Result<Self, BuildError> {
        if root_name.contains('\0') {
            return Err(BuildError::InvalidName);
        }
        if root_name.len() > usize::from(u16::MAX) {
            return Err(BuildError::NameTooLong);
        }
        root.flags.insert(Flags::DIR);
        let mut builder = TreeBuilder {
            records: Vec::with_capacity(1024),
            names: Vec::with_capacity(16 * 1024),
        };
        builder.names.extend_from_slice(root_name.as_bytes());
        builder.records.push(Record {
            logical: 0,
            allocated: root.allocated,
            parent: 0,
            name_off: 0,
            mtime: root.mtime,
            name_len: root_name.len() as u16,
            flags: root.flags,
        });
        Ok(builder)
    }

    /// Entries so far, the root included.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Never true: a builder always holds its root.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Add an entry below `parent`, which must be a directory already added.
    /// Returns the handle to pass as `parent` for its own children.
    pub fn push(&mut self, parent: u32, name: &str, entry: Entry) -> Result<u32, BuildError> {
        if !is_valid_component(name) {
            return Err(BuildError::InvalidName);
        }
        if name.len() > usize::from(u16::MAX) {
            return Err(BuildError::NameTooLong);
        }
        match self.records.get(parent as usize) {
            Some(record) if record.flags.contains(Flags::DIR) => {}
            _ => return Err(BuildError::BadParent),
        }
        // A `NodeId` is a u32, and the last value is kept free so a count of
        // entries also fits.
        if self.records.len() >= (u32::MAX - 1) as usize {
            return Err(BuildError::TooManyEntries);
        }
        let name_off = self.names.len();
        if name_off + name.len() > u32::MAX as usize {
            return Err(BuildError::NamesTooLarge);
        }
        self.names.extend_from_slice(name.as_bytes());
        let is_dir = entry.flags.contains(Flags::DIR);
        self.records.push(Record {
            logical: if is_dir { 0 } else { entry.logical },
            allocated: entry.allocated,
            parent,
            name_off: name_off as u32,
            mtime: entry.mtime,
            name_len: name.len() as u16,
            flags: entry.flags,
        });
        Ok((self.records.len() - 1) as u32)
    }

    /// Store every entry's on-disk size as its apparent size, for a source
    /// that recorded no on-disk sizes at all. Totals, sorting and the treemap
    /// then work on apparent sizes, and the caller records that basis rather
    /// than letting zero stand for "not recorded".
    pub fn apparent_only(&mut self) {
        for record in &mut self.records {
            record.allocated = record.logical;
        }
    }

    /// Lay the entries out breadth-first, aggregate, check and build.
    ///
    /// `stats` carries what the caller knows (strategy, counters, partial);
    /// the counts and totals in it are replaced by the tree's own.
    pub fn finish(
        self,
        root_path: PathBuf,
        mut stats: ScanStats,
    ) -> Result<(SizeTree, Layout), BuildError> {
        let TreeBuilder { records, names } = self;
        let count = records.len();

        // Children of each record, as one flat list with an offset per record.
        let mut starts = vec![0u32; count + 1];
        for record in records.iter().skip(1) {
            starts[record.parent as usize + 1] += 1;
        }
        for index in 0..count {
            starts[index + 1] += starts[index];
        }
        let mut fill = starts.clone();
        let mut kids = vec![0u32; count.saturating_sub(1)];
        for (index, record) in records.iter().enumerate().skip(1) {
            let slot = &mut fill[record.parent as usize];
            kids[*slot as usize] = index as u32;
            *slot += 1;
        }

        // Each directory's children by name bytes, then by arrival. `OsStr`
        // orders by its encoded bytes, which for UTF-8 are these bytes on
        // every platform, so `child_named`'s binary search holds.
        let name = |index: u32| {
            let record = &records[index as usize];
            &names[record.name_off as usize..record.name_off as usize + record.name_len as usize]
        };
        let mut layout = Layout::default();
        for dir in 0..count {
            let list = &mut kids[starts[dir] as usize..starts[dir + 1] as usize];
            list.sort_unstable_by(|&a, &b| name(a).cmp(name(b)).then(a.cmp(&b)));
            layout.duplicate_names +=
                list.windows(2).filter(|w| name(w[0]) == name(w[1])).count() as u64;
        }

        // Breadth-first order: a record's children are appended as it is
        // reached, which is what makes them contiguous.
        let mut order: Vec<u32> = Vec::with_capacity(count);
        let mut first_child = vec![0u32; count];
        order.push(0);
        let mut at = 0;
        while at < order.len() {
            let record = order[at] as usize;
            let list = &kids[starts[record] as usize..starts[record + 1] as usize];
            if !list.is_empty() {
                first_child[at] = order.len() as u32;
            }
            order.extend_from_slice(list);
            at += 1;
        }
        drop(kids);
        let mut position = vec![0u32; count];
        for (at, &record) in order.iter().enumerate() {
            position[record as usize] = at as u32;
        }

        let mut nodes: Vec<Node> = Vec::with_capacity(count);
        let mut dirs = 0u64;
        let mut max_depth = 0u8;
        for (at, &index) in order.iter().enumerate() {
            let record = &records[index as usize];
            let parent = if at == 0 {
                0
            } else {
                position[record.parent as usize]
            };
            let depth = if at == 0 {
                0
            } else {
                nodes[parent as usize].depth.saturating_add(1)
            };
            max_depth = max_depth.max(depth);
            let is_dir = record.flags.contains(Flags::DIR);
            if is_dir {
                dirs += 1;
            }
            let children = starts[index as usize + 1] - starts[index as usize];
            nodes.push(Node {
                logical: record.logical,
                allocated: record.allocated,
                parent,
                first_child: first_child[at],
                child_count: children,
                subtree_files: u32::from(!is_dir),
                name_off: record.name_off,
                mtime: record.mtime,
                name_len: record.name_len,
                flags: record.flags,
                depth,
            });
        }
        drop(records);

        crate::snapshot::validate(&nodes, names.len()).map_err(BuildError::Invalid)?;
        crate::scan::aggregate(&mut nodes);

        stats.entries = count as u64;
        stats.dirs = dirs;
        stats.files = count as u64 - dirs;
        stats.max_depth = max_depth;
        stats.bytes_logical = nodes[0].logical;
        stats.bytes_allocated = nodes[0].allocated;
        if nodes[0].is_partial() {
            stats.partial = true;
        }

        let tree = SizeTree::from_parts(
            nodes,
            NameArena::from_bytes(names),
            Owners::default(),
            root_path,
            stats,
        );
        debug_assert_eq!(tree.names_replaced(), 0, "UTF-8 names are valid everywhere");
        Ok((tree, layout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;
    use std::ffi::OsStr;

    fn file(size: u64) -> Entry {
        Entry {
            logical: size,
            allocated: size.div_ceil(4096) * 4096,
            ..Entry::default()
        }
    }

    fn dir() -> Entry {
        Entry {
            flags: Flags::DIR,
            ..Entry::default()
        }
    }

    #[test]
    fn depth_first_entries_become_a_breadth_first_sorted_arena() {
        let mut b = TreeBuilder::new("srv", dir()).unwrap();
        let z = b.push(0, "zeta", dir()).unwrap();
        b.push(z, "inner.bin", file(10)).unwrap();
        b.push(0, "alpha.txt", file(5)).unwrap();
        let m = b.push(0, "mid", dir()).unwrap();
        b.push(m, "b", file(1)).unwrap();
        b.push(m, "a", file(2)).unwrap();
        let (tree, layout) = b
            .finish(PathBuf::from("/srv"), ScanStats::default())
            .unwrap();

        assert_eq!(layout.duplicate_names, 0);
        assert_eq!(tree.len(), 7);
        assert_eq!(tree.total_logical(), 18);
        let top: Vec<_> = tree
            .children(tree.root())
            .map(|id| tree.name_lossy(id).into_owned())
            .collect();
        assert_eq!(top, ["alpha.txt", "mid", "zeta"]);
        let mid = tree.child_named(tree.root(), OsStr::new("mid")).unwrap();
        let under: Vec<_> = tree
            .children(mid)
            .map(|id| tree.name_lossy(id).into_owned())
            .collect();
        assert_eq!(under, ["a", "b"]);
        assert_eq!(tree.node(mid).logical, 3);
        assert_eq!(tree.node(mid).subtree_files, 2);
        assert_eq!(tree.stats().dirs, 3);
        assert_eq!(tree.stats().files, 4);
        assert_eq!(tree.stats().max_depth, 2);
        // Every child sits after its parent, which is what aggregation needs.
        for index in 1..tree.len() {
            assert!((tree.node(NodeId::from_index(index)).parent as usize) < index);
        }
    }

    #[test]
    fn duplicate_names_are_kept_and_counted() {
        let mut b = TreeBuilder::new("r", dir()).unwrap();
        b.push(0, "same", file(1)).unwrap();
        b.push(0, "same", file(2)).unwrap();
        let (tree, layout) = b.finish(PathBuf::from("/r"), ScanStats::default()).unwrap();
        assert_eq!(layout.duplicate_names, 1);
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.total_logical(), 3);
    }

    #[test]
    fn names_that_would_step_out_of_their_directory_are_refused() {
        for bad in [
            "", ".", "..", "a/b", "/abs", "nul\0x", "..\\x", "a\\..\\b", "a\\.",
        ] {
            assert!(!is_valid_component(bad), "{bad:?} accepted");
            let mut b = TreeBuilder::new("r", dir()).unwrap();
            assert_eq!(b.push(0, bad, file(1)), Err(BuildError::InvalidName));
        }
        for good in ["a\\b", "...", "a..b", "名前", "C:x", " "] {
            assert!(is_valid_component(good), "{good:?} refused");
        }
    }

    #[test]
    fn a_parent_must_be_a_directory_already_present() {
        let mut b = TreeBuilder::new("r", dir()).unwrap();
        let f = b.push(0, "f", file(1)).unwrap();
        assert_eq!(b.push(f, "x", file(1)), Err(BuildError::BadParent));
        assert_eq!(b.push(99, "x", file(1)), Err(BuildError::BadParent));
    }

    #[test]
    fn an_empty_root_is_a_tree_of_one() {
        let b = TreeBuilder::new("/", dir()).unwrap();
        let (tree, _) = b.finish(PathBuf::from("/"), ScanStats::default()).unwrap();
        assert_eq!(tree.len(), 1);
        assert!(tree.node(tree.root()).is_dir());
        assert_eq!(tree.stats().dirs, 1);
    }
}
