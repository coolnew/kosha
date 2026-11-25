// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Read-only comparison of the metadata in two validated snapshot arenas.

use std::cmp::Ordering;
use std::ops::Range;

use crate::node::{Flags, Node, NodeId};
use crate::snapshot::{Meta, check_child_layout, valid_path_component};
use crate::tree::SizeTree;

#[cfg(any(unix, windows))]
mod content;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    LeftOnly,
    RightOnly,
    Changed,
    MetadataMatch,
    ContentMatch,
    Indeterminate,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::LeftOnly => "left-only",
            Self::RightOnly => "right-only",
            Self::Changed => "changed",
            Self::MetadataMatch => "metadata-match",
            Self::ContentMatch => "content-match",
            Self::Indeterminate => "indeterminate",
        }
    }
}

pub struct Row<'a> {
    pub path: &'a [u8],
    pub status: Status,
    pub reasons: [Option<&'static str>; 2],
}

#[derive(Debug, Default)]
pub struct Summary {
    pub left_only: u64,
    pub right_only: u64,
    pub changed: u64,
    pub matched: u64,
    pub content_matched: u64,
    pub indeterminate: u64,
    pub incomplete: [bool; 2],
}

impl Summary {
    pub fn exit_code(&self) -> u8 {
        if self.left_only + self.right_only + self.changed > 0 {
            1
        } else if self.indeterminate > 0 || self.incomplete.contains(&true) {
            2
        } else {
            0
        }
    }

    fn count(&mut self, status: Status) {
        *match status {
            Status::LeftOnly => &mut self.left_only,
            Status::RightOnly => &mut self.right_only,
            Status::Changed => &mut self.changed,
            Status::MetadataMatch => &mut self.matched,
            Status::ContentMatch => &mut self.content_matched,
            Status::Indeterminate => &mut self.indeterminate,
        } += 1;
    }
}

struct Side<'a> {
    tree: &'a SizeTree,
    incomplete: bool,
    import_has_unknown_sizes: bool,
    mtime_known: bool,
}

impl<'a> Side<'a> {
    fn new(meta: &Meta, tree: &'a SizeTree) -> Result<Self, &'static str> {
        let import = meta.import.as_ref();
        if tree.names_replaced() != 0 || import.is_some_and(|item| item.lossy_names != 0) {
            return Err("snapshot contains replaced names; byte-exact comparison is unavailable");
        }
        let nodes = tree.nodes();
        check_child_layout(nodes)?;
        for index in 1..nodes.len() {
            let name = tree.name(NodeId::from_index(index)).as_encoded_bytes();
            if !valid_path_component(name) {
                return Err("invalid snapshot path component");
            }
        }
        Ok(Self {
            tree,
            incomplete: meta.partial
                || meta.depth_limited
                || meta.cancelled
                || meta.unreadable != 0
                || meta.skipped_other_device != 0
                || nodes[0].is_partial()
                || import.is_some_and(|item| item.excluded_by_source != 0),
            import_has_unknown_sizes: import.is_some_and(|item| item.unknown_size != 0),
            mtime_known: import.is_none_or(|item| item.mtime_known),
        })
    }

    fn size(&self, node: &Node) -> Option<u64> {
        // Imported records keep a count of unknown sizes, with zero in those
        // nodes. A genuine zero in that same record cannot be distinguished.
        if node.flags.contains(Flags::UNREADABLE)
            || node.flags.contains(Flags::HARDLINK_DUP)
            || (node.logical == 0 && self.import_has_unknown_sizes)
        {
            None
        } else {
            Some(node.logical)
        }
    }

    fn timestamp(&self, node: &Node) -> Option<u32> {
        (self.mtime_known
            && node.mtime != 0
            && node.mtime != u32::MAX
            && !node.flags.contains(Flags::UNREADABLE))
        .then_some(node.mtime)
    }
}

/// Validated snapshot pair. Comparison never resolves its recorded paths.
pub struct Comparison<'a> {
    sides: [Side<'a>; 2],
    tolerance: u32,
    #[cfg(any(unix, windows))]
    verifier: Option<content::Verifier<'a>>,
}

impl<'a> Comparison<'a> {
    pub fn new(
        left: (&Meta, &'a SizeTree),
        right: (&Meta, &'a SizeTree),
        tolerance: u32,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            sides: [Side::new(left.0, left.1)?, Side::new(right.0, right.1)?],
            tolerance,
            #[cfg(any(unix, windows))]
            verifier: None,
        })
    }

    /// Enable byte comparison for two freshly scanned local trees. The caller
    /// supplies the live roots explicitly and retains ownership of cancellation.
    #[cfg(any(unix, windows))]
    pub fn with_content_verification(
        mut self,
        roots: [&std::path::Path; 2],
        stop: &'a std::sync::atomic::AtomicBool,
    ) -> std::io::Result<Self> {
        self.verifier = Some(content::Verifier::new(roots, stop)?);
        Ok(self)
    }

    /// Stream rows depth first in byte order within each directory.
    pub fn visit(&self, mut emit: impl FnMut(Row<'_>)) -> Summary {
        self.try_visit(|row| {
            emit(row);
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap()
    }

    /// Stop traversal if the consumer returns an error, including cancellation.
    pub fn try_visit<E>(
        &self,
        mut emit: impl FnMut(Row<'_>) -> Result<(), E>,
    ) -> Result<Summary, E> {
        let mut summary = Summary {
            incomplete: [self.sides[0].incomplete, self.sides[1].incomplete],
            ..Summary::default()
        };
        let trees = [self.sides[0].tree, self.sides[1].tree];
        let mut path = Vec::new();
        let mut stack = vec![(0, Frame::new(trees, [Some(0), Some(0)]))];
        while let Some((prefix, frame)) = stack.last_mut() {
            path.truncate(*prefix);
            let Some(Siblings {
                name,
                ids,
                duplicate,
            }) = frame.next(trees)
            else {
                stack.pop();
                continue;
            };
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(name);
            let classified = if duplicate {
                (Status::Indeterminate, [Some("duplicate-path"), None])
            } else {
                self.classify(ids)
            };
            #[cfg(any(unix, windows))]
            let classified = self.verify_content(classified, ids, &path);
            let (status, reasons) = classified;
            summary.count(status);
            emit(Row {
                path: &path,
                status,
                reasons,
            })?;
            if !duplicate {
                let children = Frame::new(trees, ids);
                if children.has_children() {
                    stack.push((path.len(), children));
                }
            }
        }
        Ok(summary)
    }

    #[cfg(any(unix, windows))]
    fn verify_content(
        &self,
        (status, reasons): (Status, [Option<&'static str>; 2]),
        ids: [Option<usize>; 2],
        path: &[u8],
    ) -> (Status, [Option<&'static str>; 2]) {
        let (Some(verifier), [Some(left), Some(right)]) = (&self.verifier, ids) else {
            return (status, reasons);
        };
        if reasons[0] == Some("duplicate-path") {
            return (status, reasons);
        }
        let nodes = [
            &self.sides[0].tree.nodes()[left],
            &self.sides[1].tree.nodes()[right],
        ];
        if nodes.iter().any(|node| node.is_dir() || node.is_symlink())
            || nodes[0].logical != nodes[1].logical
        {
            return (status, reasons);
        }
        let unavailable = nodes.iter().any(|node| {
            [Flags::OFFLINE, Flags::UNREADABLE, Flags::HARDLINK_DUP]
                .into_iter()
                .any(|flag| node.flags.contains(flag))
        });
        match (!unavailable)
            .then(|| verifier.compare(path, nodes.map(|node| (node.logical, node.mtime))))
        {
            Some(Ok(true)) if status != Status::Changed => (Status::ContentMatch, [None; 2]),
            Some(Ok(true)) => (status, [reasons[0].or(reasons[1]), Some("content-matches")]),
            Some(Ok(false)) => (
                Status::Changed,
                [reasons[0].or(reasons[1]), Some("content-differs")],
            ),
            _ if status == Status::Changed => (
                status,
                [reasons[0].or(reasons[1]), Some("content-unavailable")],
            ),
            _ => (Status::Indeterminate, [None, Some("content-unavailable")]),
        }
    }

    fn classify(&self, ids: [Option<usize>; 2]) -> (Status, [Option<&'static str>; 2]) {
        let [left, right] = ids;
        let (Some(left), Some(right)) = (left, right) else {
            if left.is_some() && !self.sides[1].incomplete {
                return (Status::LeftOnly, [None; 2]);
            }
            if right.is_some() && !self.sides[0].incomplete {
                return (Status::RightOnly, [None; 2]);
            }
            return (
                Status::Indeterminate,
                [Some("other-snapshot-incomplete"), None],
            );
        };
        let a = &self.sides[0].tree.nodes()[left];
        let b = &self.sides[1].tree.nodes()[right];
        if (a.is_dir(), a.is_symlink()) != (b.is_dir(), b.is_symlink()) {
            return (Status::Changed, [Some("type"), None]);
        }
        if a.is_dir() {
            return (Status::MetadataMatch, [None; 2]);
        }
        let size = self.sides[0].size(a).zip(self.sides[1].size(b));
        let time = self.sides[0].timestamp(a).zip(self.sides[1].timestamp(b));
        let size_changed = size.is_some_and(|(a, b)| a != b);
        let time_changed = time.is_some_and(|(a, b)| a.abs_diff(b) > self.tolerance);
        if size_changed || time_changed {
            (
                Status::Changed,
                [
                    size_changed.then_some("apparent-size"),
                    time_changed.then_some("mtime"),
                ],
            )
        } else if size.is_none() || time.is_none() {
            (
                Status::Indeterminate,
                [
                    size.is_none().then_some("size-unavailable"),
                    time.is_none().then_some("mtime-unavailable"),
                ],
            )
        } else {
            (Status::MetadataMatch, [None; 2])
        }
    }
}

fn name(tree: &SizeTree, id: usize) -> &[u8] {
    tree.name(NodeId::from_index(id)).as_encoded_bytes()
}

struct Children {
    range: Range<usize>,
    reordered: Option<Vec<usize>>,
}

impl Children {
    fn new(tree: &SizeTree, parent: Option<usize>) -> Self {
        let mut range = parent.map_or(0..0, |id| tree.nodes()[id].child_range());
        let mut reordered = None;
        if !range
            .clone()
            .is_sorted_by(|&a, &b| name(tree, a) <= name(tree, b))
        {
            let mut ids: Vec<_> = range.collect();
            ids.sort_unstable_by(|&a, &b| name(tree, a).cmp(name(tree, b)).then(a.cmp(&b)));
            range = 0..ids.len();
            reordered = Some(ids);
        }
        Self { range, reordered }
    }

    fn peek(&self) -> Option<usize> {
        let index = self.range.clone().next()?;
        Some(self.reordered.as_ref().map_or(index, |ids| ids[index]))
    }

    fn take_same_name(&mut self, tree: &SizeTree, id: usize) -> bool {
        self.range.next();
        let mut duplicate = false;
        while self
            .peek()
            .is_some_and(|next| name(tree, next) == name(tree, id))
        {
            duplicate = true;
            self.range.next();
        }
        duplicate
    }
}

struct Frame([Children; 2]);

struct Siblings<'a> {
    name: &'a [u8],
    ids: [Option<usize>; 2],
    duplicate: bool,
}

impl Frame {
    fn new(trees: [&SizeTree; 2], parents: [Option<usize>; 2]) -> Self {
        Self([
            Children::new(trees[0], parents[0]),
            Children::new(trees[1], parents[1]),
        ])
    }

    fn has_children(&self) -> bool {
        self.0.iter().any(|side| side.peek().is_some())
    }

    fn next<'a>(&mut self, trees: [&'a SizeTree; 2]) -> Option<Siblings<'a>> {
        let [left, right] = [self.0[0].peek(), self.0[1].peek()];
        let ids = match (left, right) {
            (Some(a), Some(b)) => match name(trees[0], a).cmp(name(trees[1], b)) {
                Ordering::Less => [left, None],
                Ordering::Greater => [None, right],
                Ordering::Equal => [left, right],
            },
            (None, None) => return None,
            _ => [left, right],
        };
        let mut key = &[][..];
        let mut duplicate = false;
        for (side, id) in ids.iter().enumerate() {
            if let Some(id) = *id {
                key = name(trees[side], id);
                duplicate |= self.0[side].take_same_name(trees[side], id);
            }
        }
        Some(Siblings {
            name: key,
            ids,
            duplicate,
        })
    }
}
