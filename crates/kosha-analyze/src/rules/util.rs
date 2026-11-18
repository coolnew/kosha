// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Small shared helpers. Kept together so the rules read as rules.

use std::ffi::OsStr;
use std::path::Path;

use kosha_core::{Flags, NodeId, SizeTree};
use rustc_hash::FxHashSet;

/// Whether this entry's extension is one of `candidates`, compared without
/// regard to case.
///
/// Allocates nothing. Rules scan every file in the tree, so an owned lowercased
/// `String` per entry would be millions of allocations to answer a question
/// that is a byte comparison.
pub fn has_extension(tree: &SizeTree, id: NodeId, candidates: &[&str]) -> bool {
    let Some(ext) = Path::new(tree.name(id)).extension() else {
        return false;
    };
    let bytes = ext.as_encoded_bytes();
    candidates.iter().any(|candidate| {
        candidate.len() == bytes.len() && candidate.as_bytes().eq_ignore_ascii_case(bytes)
    })
}

/// True when this entry's own name matches one of `names`, ignoring case.
pub fn named(tree: &SizeTree, id: NodeId, names: &[&str]) -> bool {
    let name = tree.name(id);
    let bytes = name.as_encoded_bytes();
    names.iter().any(|candidate| {
        candidate.len() == bytes.len() && candidate.as_bytes().eq_ignore_ascii_case(bytes)
    })
}

/// True when `id` or any of its ancestors is already in `claimed`.
///
/// This is what stops a rule reporting `node_modules` inside `node_modules` and
/// counting the same bytes twice.
pub fn has_claimed_ancestor(tree: &SizeTree, id: NodeId, claimed: &FxHashSet<NodeId>) -> bool {
    let mut cur = id;
    while cur != NodeId::ROOT {
        cur = NodeId::from_index(tree.node(cur).parent as usize);
        if claimed.contains(&cur) {
            return true;
        }
    }
    false
}

/// True when the node sits under a directory with one of these names, or when
/// the scan root itself is one — so scanning `~/Downloads` directly behaves the
/// same as scanning `~`.
pub fn under_directory_named(tree: &SizeTree, id: NodeId, names: &[&str]) -> bool {
    let matches = |n: NodeId| {
        let name = tree.name(n);
        names.iter().any(|w| OsStr::new(w) == name)
    };
    if matches(NodeId::ROOT) {
        return true;
    }
    let mut cur = id;
    while cur != NodeId::ROOT {
        cur = NodeId::from_index(tree.node(cur).parent as usize);
        if matches(cur) {
            return true;
        }
    }
    false
}

/// Nodes worth reporting on: real, readable, not a symlink, not a hardlink we
/// already counted. Acting on any of these frees real bytes.
#[inline]
pub fn is_countable(tree: &SizeTree, id: NodeId) -> bool {
    let node = tree.node(id);
    !node.flags.contains(Flags::SYMLINK)
        && !node.flags.contains(Flags::HARDLINK_DUP)
        && !node.flags.contains(Flags::OTHER_DEVICE)
        && !node.flags.contains(Flags::UNREADABLE)
}

/// Sum of on-disk bytes across a set of nodes.
#[inline]
pub fn total_allocated(tree: &SizeTree, ids: &[NodeId]) -> u64 {
    ids.iter()
        .map(|&id| tree.node(id).allocated)
        .fold(0u64, u64::saturating_add)
}

/// Order nodes largest-on-disk first, so a view can take the interesting ones
/// from the front.
///
/// Deliberately does not truncate. An insight's node list is data, not a
/// display list: the engine trims overlaps between findings and recomputes
/// `reclaimable` from what is left, which is only sound while the list is
/// complete. Carrying 40,000 ids costs 160 KB and buys an exact total.
pub fn sort_by_size(tree: &SizeTree, mut ids: Vec<NodeId>) -> Vec<NodeId> {
    ids.sort_unstable_by_key(|&id| (std::cmp::Reverse(tree.node(id).allocated), id));
    ids
}
