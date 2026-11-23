// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Walking a UDF volume into a [`SizeTree`].
//!
//! Breadth-first, one directory at a time, the same shape as the ISO 9660, XFS
//! and FAT walks. What is different is what a directory entry says.
//!
//! **A file identifier descriptor does not carry a length.** An ISO 9660
//! directory record carries where the file is *and* how long it is, so a walk of
//! a disc reads directories and nothing else. UDF puts the length in the file's
//! own entry, one block per file — the same arrangement as an inode, and the same
//! cost. So a scan of a UDF volume reads once per entry rather than once per
//! directory, and the only thing to be done about it is to read in the order the
//! medium prefers.
//!
//! Which is done here: a directory's children are read **in the order their
//! entries land in the file** rather than in the order they were named, because
//! a formatter allocates entries as it writes and the two orders differ. Not in
//! the order of their *block numbers*, which is not the same question: inside a
//! virtual disk two neighbouring blocks are wherever they were first written, so
//! the sort asks [`placement`](crate::vhdx::ReadAt::placement) where each read
//! will really go. On a plain image that is the identity and the two orders are
//! the same; on a disk that grew over months they are not.
//!
//! **A directory's own entry is read once.** Its size is not known when its
//! parent names it, so the node is created without one and the number is filled
//! in when the walk arrives — which costs a `nodes[index]` and saves a read per
//! directory on the volume.

use std::io;
use std::path::Path;

use rustc_hash::FxHashSet;

use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::scan::{Progress, ScanOptions};
use crate::tree::{ScanStats, SizeTree, Strategy};
use crate::vhdx::ReadAt;

use super::format::{Allocation, Descriptors, File};
use super::reader::{self, Entry, Filesystem, Volume};

/// Where a directory is: one pointer, and nothing else is needed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place {
    pub icb: Allocation,
}

/// One entry of a directory, as the walk collects it.
struct Child {
    name: (u32, u16),
    icb: Allocation,
    is_directory: bool,
    /// The file's own entry, read after every name in this directory is known so
    /// that the reads can be made in the order the medium likes. `None` for a
    /// directory, whose entry is read when the walk arrives at it, and for one
    /// that could not be read at all.
    file: Option<File>,
}

/// Where the volume's tree starts.
pub fn root<R: ReadAt>(fs: &Filesystem<R>) -> Place {
    Place {
        icb: fs.volume().root,
    }
}

/// What a file entry says it occupies.
///
/// **A file small enough to live inside its own file entry occupies no block of
/// its own**, and its recorded block count is zero — so the honest number is its
/// length. That is exactly what this does for a resident NTFS attribute, and for
/// the same reason: rounding to a block would report space that was never
/// allocated, and zero would report a file that is there as nothing.
///
/// A file entry's own block is not counted against the file, on the same rule
/// every other reader here follows: a filesystem's structures are not files. It
/// is why a total here is a block per entry short of what `udfinfo` calls the
/// volume's used blocks, which is a difference worth being able to explain rather
/// than one worth hiding.
fn allocation(volume: &Volume, file: &File) -> u64 {
    if file.descriptors == Descriptors::Embedded {
        return file.length;
    }
    file.blocks.saturating_mul(u64::from(volume.block_size))
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
    let block_size = u64::from(fs.volume().block_size);
    let volume_bytes = fs.volume().bytes();

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
        // A directory's apparent size is its contents', never its own
        // descriptors — the rule every reader here follows, and what makes a
        // tree measured two ways agree.
        logical: 0,
        // Filled in when the walk arrives, below.
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

    // The parent entry is skipped by its own flag, so the only way to arrive
    // somewhere already visited is an entry pointing back up the tree — which a
    // volume cannot legally hold, which a damaged one can, and which would be a
    // walk with no end.
    let mut entered: FxHashSet<(u16, u32)> = FxHashSet::default();
    entered.insert((start.icb.partition, start.icb.block));
    // Two names for one file, which UDF allows and which a formatter that
    // deduplicates uses. The bytes are on the volume once.
    let mut counted: FxHashSet<(u16, u32)> = FxHashSet::default();

    let mut frontier: Vec<(u32, Place)> = vec![(0, start)];
    let mut depth: u8 = 0;
    let mut entry: Vec<u8> = Vec::new();
    let mut child_entry: Vec<u8> = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    let mut runs: Vec<(usize, Allocation)> = Vec::new();
    let mut name: Vec<u8> = Vec::new();
    let mut children: Vec<Child> = Vec::new();
    let mut raw_names: Vec<u8> = Vec::new();
    let mut order: Vec<u32> = Vec::new();

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
            let found = fs.file(place.icb, &mut entry)?;
            // An entry that will not read, or one that is not a directory after
            // its parent said it was. The name is real and what it names is not.
            let Some((directory, icb)) = found.filter(|(file, _)| file.is_directory()) else {
                mark_unreadable(&mut nodes, parent_index, &mut stats);
                continue;
            };
            if let Some(node) = nodes.get_mut(parent_index as usize) {
                node.allocated = allocation(fs.volume(), &directory);
                node.mtime = directory.mtime;
            }

            children.clear();
            raw_names.clear();
            let whole = reader::entries(
                fs,
                &directory,
                &entry,
                icb,
                &mut buffer,
                &mut runs,
                &mut name,
                |found: Entry<'_>| {
                    let at = raw_names.len() as u32;
                    raw_names.extend_from_slice(found.name);
                    let len = u16::try_from(found.name.len()).unwrap_or(u16::MAX);
                    children.push(Child {
                        name: (at, len),
                        icb: found.icb,
                        is_directory: found.is_directory,
                        file: None,
                    });
                },
            );
            if !whole {
                mark_unreadable(&mut nodes, parent_index, &mut stats);
            }

            // **In the order the medium has them, not the order they are named.**
            // One read per file is what this format costs; reading them in a
            // sweep is what keeps that from also being one seek per file.
            order.clear();
            order.extend(
                (0..children.len() as u32).filter(|index| !children[*index as usize].is_directory),
            );
            order.sort_unstable_by_key(|index| fs.lands_at(children[*index as usize].icb));
            for index in &order {
                let child = &children[*index as usize];
                let icb = child.icb;
                let file = fs.file(icb, &mut child_entry)?.map(|(file, _)| file);
                children[*index as usize].file = file;
            }

            // Largest first is the interface's order, but the tree is built by
            // name so that two scans of one volume produce the same arena.
            children.sort_unstable_by(|a, b| {
                name_of(&raw_names, a.name).cmp(name_of(&raw_names, b.name))
            });

            let first_child = nodes.len() as u32;
            let mut kept = 0u32;
            for child in &children {
                let (name_off, name_len) = names.push_bytes(name_of(&raw_names, child.name));
                let mut flags = Flags::default();
                // A directory's numbers arrive when the walk does. A file's are
                // in the entry just read, and an entry that would not read is a
                // name with nothing behind it.
                let (mut logical, mut allocated, mtime, lost) =
                    match (child.is_directory, child.file) {
                        (true, _) => (0, 0, 0, false),
                        (false, Some(file)) => {
                            if file.is_symlink() {
                                // A link is a link first, exactly as in the walk: a
                                // symbolic link to a directory is measured as itself
                                // and has no children.
                                flags.insert(Flags::SYMLINK);
                            }
                            let allocated = allocation(fs.volume(), &file);
                            // A length longer than the volume that holds it is a
                            // flipped bit rather than a file.
                            let lost = file.length > volume_bytes;
                            (file.length, allocated, file.mtime, lost)
                        }
                        (false, None) => (0, 0, 0, true),
                    };
                if child.is_directory {
                    flags.insert(Flags::DIR);
                }
                if lost {
                    logical = 0;
                    allocated = 0;
                    stats.unreadable += 1;
                    stats.partial = true;
                    flags.insert(Flags::UNREADABLE);
                }
                // A hole in a file is a real thing on UDF: an extent may be
                // allocated and never written, which is what a file written
                // sparsely looks like on a rewritable volume.
                if !child.is_directory && !lost && allocated.saturating_add(block_size) < logical {
                    flags.insert(Flags::SPARSE);
                }
                if options.dedups_hardlinks(true)
                    && !child.is_directory
                    && !lost
                    && child.file.is_some_and(|file| file.links > 1)
                    && !counted.insert((child.icb.partition, child.icb.block))
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
                    subtree_files: u32::from(!child.is_directory),
                    name_off,
                    mtime,
                    name_len,
                    flags,
                    depth,
                });
                kept += 1;
                stats.entries += 1;
                if child.is_directory {
                    stats.dirs += 1;
                    if entered.insert((child.icb.partition, child.icb.block)) {
                        next.push((index, Place { icb: child.icb }));
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

/// Follow a path down from the root, for a scan pointed inside the volume.
///
/// **Case-sensitively, unlike the ISO 9660 walk.** A published ISO 9660 name is
/// uppercase because the standard allows nothing else, so the same directory is
/// `docs` or `DOCS` depending on which tree a disc carries — and matching
/// loosely is the only way to spare somebody knowing that. UDF has one tree and
/// one spelling of every name, so a loose match here would be a way of finding
/// the wrong directory rather than a way of finding the right one.
pub fn resolve<R: ReadAt>(fs: &mut Filesystem<R>, inside: &Path) -> io::Result<Option<Place>> {
    let mut at = root(fs);
    let mut entry: Vec<u8> = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    let mut runs: Vec<(usize, Allocation)> = Vec::new();
    let mut name: Vec<u8> = Vec::new();

    for component in inside.components() {
        let wanted = component.as_os_str().as_encoded_bytes().to_vec();
        if wanted.is_empty() {
            continue;
        }
        let Some((directory, icb)) = fs.file(at.icb, &mut entry)? else {
            return Ok(None);
        };
        if !directory.is_directory() {
            return Ok(None);
        }
        let mut found: Option<Place> = None;
        reader::entries(
            fs,
            &directory,
            &entry,
            icb,
            &mut buffer,
            &mut runs,
            &mut name,
            |child| {
                if found.is_none() && child.is_directory && child.name == wanted.as_slice() {
                    found = Some(Place { icb: child.icb });
                }
            },
        );
        match found {
            Some(place) => at = place,
            None => return Ok(None),
        }
    }
    Ok(Some(at))
}
