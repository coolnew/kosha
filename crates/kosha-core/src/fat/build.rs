// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Walking a FAT or exFAT filesystem into a [`SizeTree`].
//!
//! Breadth-first, one directory at a time, which is the same shape as the XFS
//! walk and for the same reason: there is no table of every file to sweep. In
//! FAT a file exists because a directory record names it, and nowhere else —
//! the cluster table records which clusters are taken and has no idea what is
//! in them.
//!
//! **Three things the other readers do are absent here, and each of them is
//! absent because the format has nothing to say.**
//!
//! - *Hardlinks.* Neither format has them, so `dedup_hardlinks` has nothing to
//!   deduplicate. Two names for one file is not something a FAT volume can
//!   express — and `chkdsk` calls it a cross-link and offers to break it.
//! - *Sparse files.* Neither format has those either, which is why a file's
//!   allocation is its length rounded up to a cluster rather than a walk of
//!   its chain. See [`Geometry::allocation`](super::format::Geometry::allocation).
//! - *Owners.* There are no permissions on a FAT volume at all. That is the
//!   reason a USB stick can be carried between machines, and the reason a
//!   scan of one has nothing to put in the owner column.

use std::io;
use std::path::Path;

use rustc_hash::FxHashSet;

use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::scan::{Progress, ScanOptions};
use crate::tree::{ScanStats, SizeTree, Strategy};
use crate::vhdx::ReadAt;

use super::format::Place;
use super::reader::{self, Entry, Filesystem};

/// One entry of a directory, as the walk collects it.
struct Child {
    name: (u32, u16),
    cluster: u32,
    length: u64,
    contiguous: bool,
    is_directory: bool,
    mtime: u32,
}

impl Child {
    fn of(entry: &Entry<'_>, name: (u32, u16)) -> Child {
        Child {
            name,
            cluster: entry.cluster,
            length: entry.length,
            contiguous: entry.contiguous,
            is_directory: entry.is_directory,
            mtime: entry.mtime,
        }
    }

    fn place(&self) -> Place {
        Place::Chain {
            first: self.cluster,
            contiguous: self.contiguous.then_some(self.length),
        }
    }
}

/// Build the tree, starting wherever `start` says.
pub fn build<R: ReadAt>(
    fs: &mut Filesystem<R>,
    root_path: &Path,
    start: Place,
    options: &ScanOptions,
    mut publish: impl FnMut(Progress),
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<SizeTree> {
    let geometry = *fs.geometry();

    let mut nodes: Vec<Node> = Vec::with_capacity(1 << 16);
    let mut names = NameArena::with_capacity(1 << 20);
    let mut stats = ScanStats {
        strategy: Strategy::DiskImage,
        ..ScanStats::default()
    };

    let root_label = root_path.file_name().unwrap_or(root_path.as_os_str());
    let (root_off, root_len) = names.push(root_label);
    nodes.push(Node {
        // A directory's apparent size is its contents', never its own records
        // — the same rule every other reader here follows, and what makes a
        // tree measured two ways agree.
        logical: 0,
        // Filled in when the directory is walked, which is the only place its
        // length is known: FAT does not record one anywhere.
        allocated: 0,
        parent: 0,
        first_child: 0,
        child_count: 0,
        subtree_files: 0,
        name_off: root_off,
        mtime: 0,
        name_len: root_len,
        flags: Flags::DIR,
        depth: 0,
    });
    stats.dirs = 1;
    stats.entries = 1;

    // A directory reached twice is a corrupt volume — `..` is skipped by name,
    // so the only way to arrive somewhere already visited is a record pointing
    // back up the tree. Following it would be a walk with no end.
    let mut entered: FxHashSet<u32> = FxHashSet::default();
    if let Place::Chain { first, .. } = start {
        entered.insert(first);
    }

    let mut frontier: Vec<(u32, Place)> = vec![(0, start)];
    let mut depth: u8 = 0;
    let mut buffer: Vec<u8> = Vec::new();
    let mut name: Vec<u8> = Vec::new();
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

        let mut next: Vec<(u32, Place)> = Vec::new();
        for &(parent_index, ref place) in &frontier {
            children.clear();
            raw_names.clear();
            let walked = reader::entries(fs, place, &mut buffer, &mut name, |entry| {
                let at = raw_names.len() as u32;
                raw_names.extend_from_slice(entry.name);
                let len = u16::try_from(entry.name.len()).unwrap_or(u16::MAX);
                children.push(Child::of(&entry, (at, len)));
            });

            match walked {
                Ok(walked) => {
                    if let Some(node) = nodes.get_mut(parent_index as usize) {
                        node.allocated = walked.allocated;
                    }
                    if !walked.whole {
                        mark_unreadable(&mut nodes, parent_index, &mut stats);
                    }
                }
                // A directory whose first read failed. The tree keeps the
                // entry and says the total below it is a floor.
                Err(_) => mark_unreadable(&mut nodes, parent_index, &mut stats),
            }

            // Largest first is the interface's order, but the tree is built by
            // name so that two scans of the same volume produce the same
            // arena — the sort into size order happens once, later, over the
            // finished tree.
            children.sort_unstable_by(|a, b| {
                name_of(&raw_names, a.name).cmp(name_of(&raw_names, b.name))
            });

            let first_child = nodes.len() as u32;
            let mut kept = 0u32;
            for child in &children {
                let (name_off, name_len) = names.push_bytes(name_of(&raw_names, child.name));
                let mut flags = Flags::default();
                if child.is_directory {
                    flags.insert(Flags::DIR);
                }

                // A record naming no cluster and claiming bytes anyway, or a
                // directory with nowhere to be. Both are damage, and both are
                // a name that is real pointing at something that is not.
                let lost = child.cluster == 0 && (child.is_directory || child.length > 0);
                // And a file longer than the volume that holds it, which is
                // the shape a flipped bit in a length takes: exFAT states a
                // file's length in sixty-four bits, so one bad bit asks for
                // exabytes and the total above it becomes a number nobody can
                // read past.
                let impossible = child.length > geometry.volume_bytes;
                if lost || impossible || (child.cluster != 0 && !geometry.holds(child.cluster)) {
                    stats.unreadable += 1;
                    stats.partial = true;
                    flags.insert(Flags::UNREADABLE);
                }

                let index = nodes.len() as u32;
                nodes.push(Node {
                    logical: if child.is_directory || impossible {
                        0
                    } else {
                        child.length
                    },
                    // A directory's own bytes are filled in when it is walked;
                    // a file's are its length rounded up to a cluster, which
                    // is exact on a filesystem with no holes in files.
                    allocated: if child.is_directory || impossible {
                        0
                    } else {
                        geometry.allocation(child.length)
                    },
                    parent: parent_index,
                    first_child: 0,
                    child_count: 0,
                    subtree_files: u32::from(!child.is_directory),
                    name_off,
                    mtime: child.mtime,
                    name_len,
                    flags,
                    depth,
                });
                kept += 1;
                stats.entries += 1;
                if child.is_directory {
                    stats.dirs += 1;
                    if !lost && geometry.holds(child.cluster) && entered.insert(child.cluster) {
                        next.push((index, child.place()));
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

/// Follow a path down from the root, for a scan pointed inside the filesystem.
///
/// **Case-insensitively, which is not a convenience.** Neither format has ever
/// had case-sensitive names: FAT stores a short name in capitals whatever was
/// typed, and exFAT keeps an upcase table on the volume precisely so that two
/// names differing only in case are one name. A path somebody typed has to
/// match the way the filesystem itself would match it, or `\EFI\BOOT` would
/// fail on a volume whose records say `EFI` and `Boot`.
///
/// The comparison is ASCII, which is where this stops short of what exFAT's
/// upcase table would do. A name differing only in the case of a character
/// outside ASCII will not be found — and that is a path somebody typed, not a
/// file that goes missing from a scan.
///
/// **Only directories match.** A path naming a file inside an image is not a
/// place a scan can start, and saying so is better than measuring the
/// directory that holds it and calling the answer by the file's name.
pub fn resolve<R: ReadAt>(fs: &mut Filesystem<R>, inside: &Path) -> io::Result<Option<Place>> {
    let mut at = reader::root(fs.geometry());
    let mut buffer: Vec<u8> = Vec::new();
    let mut name: Vec<u8> = Vec::new();

    for component in inside.components() {
        let wanted = component.as_os_str().as_encoded_bytes().to_vec();
        if wanted.is_empty() {
            continue;
        }
        let mut found: Option<Place> = None;
        reader::entries(fs, &at, &mut buffer, &mut name, |entry| {
            if found.is_none() && entry.is_directory && entry.name.eq_ignore_ascii_case(&wanted) {
                found = Some(reader::place_of(&entry));
            }
        })?;
        match found {
            Some(place) => at = place,
            None => return Ok(None),
        }
    }
    Ok(Some(at))
}
