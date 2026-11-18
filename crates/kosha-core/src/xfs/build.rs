// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Walking an XFS filesystem into a [`SizeTree`].
//!
//! **A different shape of walk from the ext one, and for a reason in the
//! format.** ext keeps its inodes in a table per block group, so the fast way
//! to read one is to read all of them and join afterwards. XFS scatters them
//! across allocation groups and offers no table to sweep: an inode is reached
//! by the directory entry that names it, and by nothing else. So this is a
//! breadth-first walk, one directory at a time, which is the same shape as the
//! ordinary directory walk and for the same reason.
//!
//! What it does *not* do is read any file's extents. A file's size and its
//! allocation are both in its inode, so the block map is only ever walked for
//! directories.

use std::io;
use std::path::Path;

use rustc_hash::FxHashSet;

use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::scan::{Progress, ScanOptions};
use crate::tree::{ScanStats, SizeTree, Strategy};
use crate::vhdx::ReadAt;

use super::format::Kind;
use super::reader::{self, Filesystem};

/// One entry of a directory, as the walk collects it.
struct Child {
    name: (u32, u16),
    inode: u64,
}

/// Build the tree, starting at the filesystem's root inode.
pub fn build<R: ReadAt>(
    fs: &mut Filesystem<R>,
    root_path: &Path,
    start: u64,
    options: &ScanOptions,
    mut publish: impl FnMut(Progress),
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<SizeTree> {
    let superblock = *fs.superblock();
    let block_size = superblock.block_size;

    let mut nodes: Vec<Node> = Vec::with_capacity(1 << 16);
    let mut names = NameArena::with_capacity(1 << 20);
    let mut stats = ScanStats {
        strategy: Strategy::DiskImage,
        dedup_hardlinks: options.dedups_hardlinks(true),
        ..ScanStats::default()
    };

    let (root_inode, _) = fs
        .inode(start)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the root inode is not one"))?;

    let root_label = root_path.file_name().unwrap_or(root_path.as_os_str());
    let (root_off, root_len) = push_os_name(&mut names, root_label);
    nodes.push(Node {
        // A directory's apparent size is its contents', never its own entry
        // blocks — the same rule the walk and the ext reader follow, and what
        // makes a tree measured two ways agree.
        logical: 0,
        allocated: root_inode.blocks.saturating_mul(block_size),
        parent: 0,
        first_child: 0,
        child_count: 0,
        subtree_files: 0,
        name_off: root_off,
        mtime: seconds(root_inode.mtime),
        name_len: root_len,
        flags: Flags::DIR,
        depth: 0,
    });
    stats.dirs = 1;
    stats.entries = 1;

    // A directory cannot really be hardlinked, so a cycle means a corrupt
    // image — and a cycle here is a walk that never ends.
    let mut entered: FxHashSet<u64> = FxHashSet::default();
    entered.insert(start);
    // The first name a file is reached by owns its bytes.
    let mut counted: FxHashSet<u64> = FxHashSet::default();

    let mut frontier: Vec<(u32, u64)> = vec![(0, start)];
    let mut depth: u8 = 0;
    let mut buffer: Vec<u8> = Vec::new();
    let mut children: Vec<Child> = Vec::new();
    let mut raw_names: Vec<u8> = Vec::new();

    while !frontier.is_empty() {
        if cancelled() {
            stats.cancelled = true;
            stats.partial = true;
            break;
        }
        if options.depth_limit_reached(depth) {
            break;
        }
        depth = depth.saturating_add(1);

        let mut next: Vec<(u32, u64)> = Vec::new();
        for &(parent_index, number) in &frontier {
            let Some((inode, raw)) = fs.inode(number)? else {
                mark_unreadable(&mut nodes, parent_index, &mut stats);
                continue;
            };
            // The inode's own bytes are borrowed from the filesystem, and
            // reading a directory block needs it back. A copy per directory,
            // not per entry.
            let raw = raw.to_vec();

            children.clear();
            raw_names.clear();
            let read = reader::entries(fs, &inode, &raw, &mut buffer, |name, child| {
                // `.` and `..` are entries like any other here, and following
                // either is a cycle rather than a subtree.
                if name == b"." || name == b".." {
                    return;
                }
                let at = raw_names.len() as u32;
                raw_names.extend_from_slice(name);
                let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
                children.push(Child {
                    name: (at, len),
                    inode: child,
                });
            });
            match read {
                Ok(true) => {}
                // Some of it was read, or none of it was. Either way the
                // totals below this directory are a floor, and the tree says
                // so rather than quietly reporting a smaller disk.
                Ok(false) | Err(_) => mark_unreadable(&mut nodes, parent_index, &mut stats),
            }

            // Largest first is the interface's order, but the tree is built by
            // name so that two scans of the same filesystem produce the same
            // arena — the sort into size order happens once, later, over the
            // finished tree.
            children.sort_unstable_by(|a, b| {
                name_of(&raw_names, a.name).cmp(name_of(&raw_names, b.name))
            });

            let first_child = nodes.len() as u32;
            let mut kept = 0u32;
            for child in &children {
                let name = name_of(&raw_names, child.name);
                let Some((child_inode, _)) = fs.inode(child.inode)? else {
                    // An entry naming an inode that is not one. The name is
                    // real and the thing it names is not, so it is counted as
                    // unreadable rather than dropped without trace.
                    stats.unreadable += 1;
                    stats.partial = true;
                    continue;
                };

                let (name_off, name_len) = push_name(&mut names, name);
                let is_dir = child_inode.kind == Kind::Directory;
                let mut flags = Flags::default();
                // A link is a link first, exactly as in the walk: a symlink to
                // a directory is measured as itself and has no children.
                if child_inode.kind == Kind::Symlink {
                    flags.insert(Flags::SYMLINK);
                } else if is_dir {
                    flags.insert(Flags::DIR);
                }

                let mut allocated = child_inode.blocks.saturating_mul(block_size);
                let mut logical = if is_dir { 0 } else { child_inode.size };
                // Saturating, because both sides come off the disk: a block
                // count near the top of a `u64` makes `allocated` saturate,
                // and adding a block to that is a panic rather than a large
                // number. Found by the sweep below.
                if !is_dir && allocated.saturating_add(block_size) < child_inode.size {
                    flags.insert(Flags::SPARSE);
                }
                if options.dedups_hardlinks(true)
                    && !is_dir
                    && child_inode.links > 1
                    && !counted.insert(child.inode)
                {
                    flags.insert(Flags::HARDLINK_DUP);
                    logical = 0;
                    allocated = 0;
                    stats.hardlinks_deduped += 1;
                }

                let index = nodes.len() as u32;
                nodes.push(Node {
                    logical,
                    allocated,
                    parent: parent_index,
                    first_child: 0,
                    child_count: 0,
                    subtree_files: if is_dir { 0 } else { 1 },
                    name_off,
                    mtime: seconds(child_inode.mtime),
                    name_len,
                    flags,
                    depth,
                });
                kept += 1;
                stats.entries += 1;
                if is_dir {
                    stats.dirs += 1;
                    // A second name for a directory is a corrupt image, and
                    // following it would be a walk with no end.
                    if entered.insert(child.inode) {
                        next.push((index, child.inode));
                    }
                } else {
                    stats.files += 1;
                }
            }

            let parent = &mut nodes[parent_index as usize];
            parent.first_child = first_child;
            parent.child_count = kept;

            publish(Progress {
                dirs_scanned: None,
                entries: stats.entries,
                dirs_pending: next.len() as u64,
                depth,
            });
        }

        frontier = next;
    }

    stats.max_depth = depth;
    crate::scan::aggregate(&mut nodes);
    nodes.shrink_to_fit();
    names.shrink_to_fit();
    stats.bytes_logical = nodes[0].logical;
    stats.bytes_allocated = nodes[0].allocated;

    Ok(SizeTree::from_parts(
        nodes,
        names,
        crate::owner::Owners::default(),
        root_path.to_path_buf(),
        stats,
    ))
}

/// An XFS timestamp as the tree keeps one.
///
/// XFS signs its seconds and the tree does not: a date before 1970 is a clock
/// that was wrong when the file was written, not something worth carrying into
/// a report about disk space.
fn seconds(when: i64) -> u32 {
    u32::try_from(when).unwrap_or(0)
}

/// A directory whose entries could not all be read.
///
/// Recorded on the directory itself rather than swallowed, because a total
/// that is a floor and does not say so is worse than one that is missing.
fn mark_unreadable(nodes: &mut [Node], index: u32, stats: &mut ScanStats) {
    if let Some(node) = nodes.get_mut(index as usize) {
        node.flags.insert(Flags::UNREADABLE);
        node.flags.insert(Flags::PARTIAL);
    }
    stats.unreadable += 1;
    stats.partial = true;
}

fn name_of(raw: &[u8], (at, len): (u32, u16)) -> &[u8] {
    raw.get(at as usize..at as usize + len as usize)
        .unwrap_or(&[])
}

fn push_name(names: &mut NameArena, name: &[u8]) -> (u32, u16) {
    names.push_bytes(name)
}

fn push_os_name(names: &mut NameArena, name: &std::ffi::OsStr) -> (u32, u16) {
    names.push(name)
}

/// Follow a path down from the root, for a scan pointed inside the filesystem.
pub fn resolve<R: ReadAt>(fs: &mut Filesystem<R>, inside: &Path) -> io::Result<Option<u64>> {
    let mut at = fs.superblock().root_inode;
    let mut buffer: Vec<u8> = Vec::new();

    for component in inside.components() {
        let wanted = component.as_os_str();
        if wanted.is_empty() {
            continue;
        }
        let Some((inode, raw)) = fs.inode(at)? else {
            return Ok(None);
        };
        if inode.kind != Kind::Directory {
            return Ok(None);
        }
        let raw = raw.to_vec();
        let mut found: Option<u64> = None;
        let _ = reader::entries(fs, &inode, &raw, &mut buffer, |name, child| {
            if found.is_none() && name == wanted.as_encoded_bytes() {
                found = Some(child);
            }
        });
        match found {
            Some(child) => at = child,
            None => return Ok(None),
        }
    }
    Ok(Some(at))
}
