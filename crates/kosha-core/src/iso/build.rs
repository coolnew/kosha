// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Walking an ISO 9660 filesystem into a [`SizeTree`].
//!
//! Breadth-first, one directory at a time, the same shape as the XFS and FAT
//! walks. There is no table of every file to sweep, and the path tables — two
//! of them, one in each byte order — are an index for finding a directory by
//! path, which a walk that visits every directory anyway has no use for.
//!
//! **What a scan of a disc actually answers.** The file is one number and
//! anybody can see it; the question is what is *inside* the five gigabytes,
//! which is the same question as for any other tree and has never had an
//! answer without mounting the thing first. Mounting needs root on Linux,
//! needs a drive letter and a loopback driver on Windows, and on both it means
//! the disc is a device for as long as you are looking at it.
//!
//! Three things the other readers do are absent, and all three because the
//! medium cannot be written:
//!
//! - *Hardlinks.* Rock Ridge records an inode number in its `PX` entry, and
//!   two names sharing one is legal — but a disc is written once, from a tree
//!   that was already laid out, and the bytes are stored once either way. The
//!   number is read by nothing here.
//! - *Sparse files.* A file is one run of consecutive blocks, so its
//!   allocation is its length rounded up to a block and cannot be anything
//!   else.
//! - *Owners.* `PX` carries a user and a group, and they are the user and
//!   group of whoever built the disc on a machine that no longer matters. A
//!   column of them would be an answer to a question nobody asked.

use std::io;
use std::path::Path;

use rustc_hash::FxHashSet;

use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::scan::{Progress, ScanOptions};
use crate::tree::{ScanStats, SizeTree, Strategy};
use crate::vhdx::ReadAt;

use super::reader::{self, Entry, Filesystem};

/// Where a directory is, which on this medium is two numbers and no chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place {
    pub extent: u32,
    pub bytes: u64,
}

/// One entry of a directory, as the walk collects it.
struct Child {
    name: (u32, u16),
    extent: u32,
    size: u64,
    is_directory: bool,
    is_symlink: bool,
    mtime: u32,
}

impl Child {
    fn of(entry: &Entry<'_>, name: (u32, u16)) -> Child {
        Child {
            name,
            extent: entry.extent,
            size: entry.size,
            is_directory: entry.is_directory,
            is_symlink: entry.is_symlink,
            mtime: entry.mtime,
        }
    }
}

/// Where the disc's tree starts.
pub fn root<R: ReadAt>(fs: &Filesystem<R>) -> Place {
    Place {
        extent: fs.volume().root_extent,
        bytes: u64::from(fs.volume().root_bytes),
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
    let volume = *fs.volume();

    let mut nodes: Vec<Node> = Vec::with_capacity(1 << 16);
    let mut names = NameArena::with_capacity(1 << 20);
    let mut stats = ScanStats {
        strategy: Strategy::DiskImage,
        dedup_hardlinks: options.dedups_hardlinks(true),
        ..ScanStats::default()
    };

    let root_label = root_path.file_name().unwrap_or(root_path.as_os_str());
    let (root_off, root_len) = names.push(root_label);
    nodes.push(Node {
        // A directory's apparent size is its contents', never its own records
        // — the same rule every other reader here follows, and what makes a
        // tree measured two ways agree.
        logical: 0,
        // Unlike FAT, a disc does record how long a directory is, so this is
        // read rather than counted.
        allocated: volume.allocation(start.bytes),
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

    // `.` and `..` are skipped by name, so the only way to arrive somewhere
    // already visited is a record pointing back up the tree — which a disc
    // cannot legally hold and a damaged one can, and following it would be a
    // walk with no end.
    let mut entered: FxHashSet<u32> = FxHashSet::default();
    entered.insert(start.extent);
    // Two names for one run of blocks. ISO 9660 has no hardlinks and this is
    // not one: it is what a mastering tool does with a source tree that had
    // them — `mkisofs` and `xorrisofs` point every name at one extent, with or
    // without Rock Ridge, and write a link count of 1 while doing it. So the
    // extent is the only thing that says the bytes are on the disc once, and
    // counting them per name reports a disc as larger than the medium it came
    // from.
    let mut counted: FxHashSet<u32> = FxHashSet::default();

    let mut frontier: Vec<(u32, Place)> = vec![(0, start)];
    let mut depth: u8 = 0;
    let mut buffer: Vec<u8> = Vec::new();
    let mut spare: Vec<u8> = Vec::new();
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
        for &(parent_index, place) in &frontier {
            children.clear();
            raw_names.clear();
            let read = reader::entries(
                fs,
                place.extent,
                place.bytes,
                &mut buffer,
                &mut spare,
                &mut name,
                |entry| {
                    let at = raw_names.len() as u32;
                    raw_names.extend_from_slice(entry.name);
                    let len = u16::try_from(entry.name.len()).unwrap_or(u16::MAX);
                    children.push(Child::of(&entry, (at, len)));
                },
            );
            match read {
                Ok(true) => {}
                // Some of it was read, or none of it was. Either way the
                // totals below this directory are a floor, and the tree says
                // so rather than quietly reporting a smaller disc.
                Ok(false) | Err(_) => mark_unreadable(&mut nodes, parent_index, &mut stats),
            }

            // Largest first is the interface's order, but the tree is built by
            // name so that two scans of the same disc produce the same arena.
            children.sort_unstable_by(|a, b| {
                name_of(&raw_names, a.name).cmp(name_of(&raw_names, b.name))
            });

            let first_child = nodes.len() as u32;
            let mut kept = 0u32;
            for child in &children {
                let (name_off, name_len) = names.push_bytes(name_of(&raw_names, child.name));
                let mut flags = Flags::default();
                // A link is a link first, exactly as in the walk and in every
                // other reader here: a symbolic link to a directory is measured
                // as itself and has no children. `reader` has already cleared
                // `is_directory` for one, so the walk below cannot descend into
                // whatever it points at.
                if child.is_symlink {
                    flags.insert(Flags::SYMLINK);
                } else if child.is_directory {
                    flags.insert(Flags::DIR);
                }
                // A record naming a block the disc does not have, or a file
                // longer than the disc itself. The name is real and the thing
                // it names is not.
                //
                // **Except where there are no bytes to locate.** `mkisofs`
                // writes 0xFFFFFFF0 as the extent of a zero-length file rather
                // than the zero the standard asks for, and it built a very large
                // share of the discs anybody has — so checking the extent of an
                // empty file made every such disc report an unreadable entry and
                // a partial tree, over a number that means nothing either way.
                let lost = (child.size > 0 && volume.block_offset(child.extent).is_none())
                    || child.size > volume.bytes();
                if lost {
                    stats.unreadable += 1;
                    stats.partial = true;
                    flags.insert(Flags::UNREADABLE);
                }

                // A second name for blocks already counted. Files only, and
                // only where there are blocks: a directory sharing an extent is
                // a cycle, which `entered` deals with, and every empty file on
                // a disc shares the one extent a zero length is recorded with.
                let shared = !child.is_directory
                    && !lost
                    && child.size > 0
                    && !counted.insert(child.extent)
                    && options.dedups_hardlinks(true);
                if shared {
                    flags.insert(Flags::HARDLINK_DUP);
                    stats.hardlinks_deduped += 1;
                }

                let index = nodes.len() as u32;
                nodes.push(Node {
                    logical: if child.is_directory || lost || shared {
                        0
                    } else {
                        child.size
                    },
                    allocated: if lost || shared {
                        0
                    } else {
                        volume.allocation(child.size)
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
                    if !lost && entered.insert(child.extent) {
                        next.push((
                            index,
                            Place {
                                extent: child.extent,
                                bytes: child.size,
                            },
                        ));
                    }
                } else {
                    stats.files += 1;
                }
            }

            let parent = &mut nodes[parent_index as usize];
            parent.first_child = first_child;
            parent.child_count = kept;

            publish(Progress {
                recorded_counts: None,
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

/// Follow a path down from the root, for a scan pointed inside the disc.
///
/// **Case-insensitively, and that is not a convenience either.** A published
/// ISO 9660 name is uppercase because the standard allows nothing else, so a
/// disc whose author called a directory `docs` has `DOCS` on it — and which of
/// the two a scan sees depends on whether the disc carries Rock Ridge, which
/// is not something anybody typing a path should have to know.
pub fn resolve<R: ReadAt>(fs: &mut Filesystem<R>, inside: &Path) -> io::Result<Option<Place>> {
    let mut at = root(fs);
    let mut buffer: Vec<u8> = Vec::new();
    let mut spare: Vec<u8> = Vec::new();
    let mut name: Vec<u8> = Vec::new();

    for component in inside.components() {
        let wanted = component.as_os_str().as_encoded_bytes().to_vec();
        if wanted.is_empty() {
            continue;
        }
        let mut found: Option<Place> = None;
        reader::entries(
            fs,
            at.extent,
            at.bytes,
            &mut buffer,
            &mut spare,
            &mut name,
            |entry| {
                if found.is_none() && entry.is_directory && entry.name.eq_ignore_ascii_case(&wanted)
                {
                    found = Some(Place {
                        extent: entry.extent,
                        bytes: entry.size,
                    });
                }
            },
        )?;
        match found {
            Some(place) => at = place,
            None => return Ok(None),
        }
    }
    Ok(Some(at))
}
