// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Turning an ext filesystem into a [`SizeTree`].
//!
//! **Read the tables, then join** — the same shape as the MFT strategy, and for
//! the same reason. The obvious alternative is to walk the tree and read each
//! entry as it is met, which is what the ordinary directory walk does and what
//! this began as. On a spinning disk that took **eighteen seconds** where this
//! takes two, and the measurements say exactly why.
//!
//! Three phases, each reading in the order the disk wants rather than the order
//! the tree wants:
//!
//! 1. **Every inode in use.** Inode tables are per block group and most groups
//!    hold nothing: `alpine-musl` here has 8,192 groups and 67 million inodes —
//!    sixteen gigabytes of table — of which **thirty-five groups** and 70 MiB
//!    are in use. Each is one sequential read.
//! 2. **Every directory's blocks, sorted.** Of the 42,196 gaps between this
//!    filesystem's sorted directory blocks, **40,120 are adjacent**: they are
//!    very nearly one long run, because they were laid down as the image was
//!    built. Asked for in tree order they cross and re-cross that run; sorted,
//!    they are about fifteen hundred sequential reads.
//! 3. **The join.** Breadth-first over what is now in memory, touching no disk
//!    at all.
//!
//! The arena that comes out has to satisfy exactly the invariants the ordinary
//! walk's does — contiguous children, a child's index above its parent's, names
//! sorted — because everything downstream is written against them. The join is
//! breadth-first for that reason rather than the recursion the shape invites.

use std::io;
use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use super::format::{self, Kind};
use super::reader::{self, Filesystem};
use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::scan::{Progress, ScanOptions};
use crate::tree::{ScanStats, SizeTree, Strategy};
use crate::vhdx::ReadAt;

/// How many blocks to hold at once while reading directories.
///
/// Bounds what a filesystem with an enormous number of directories asks for.
/// The only cost of a smaller number is one extra seek where a run happens to
/// straddle a batch.
const GATHER_BATCH: usize = 16 * 1024;

/// Build the tree, starting from `start` — the filesystem root, or whatever
/// directory a scan was pointed at.
pub fn build<R: ReadAt>(
    fs: &mut Filesystem<R>,
    root_path: &Path,
    start: u32,
    options: &ScanOptions,
    mut publish: impl FnMut(Progress),
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<SizeTree> {
    let table = read_inodes(fs, &mut cancelled)?;
    let mut raw_names: Vec<u8> = Vec::new();
    let links = read_directories(fs, &table, &mut raw_names, &mut cancelled)?;

    // Where each directory's children sit in `links`, which is sorted by the
    // directory they are in.
    let mut index: FxHashMap<u32, (u32, u32)> = FxHashMap::default();
    let mut at = 0usize;
    while at < links.len() {
        let parent = links[at].parent;
        let mut end = at + 1;
        while end < links.len() && links[end].parent == parent {
            end += 1;
        }
        index.insert(parent, (at as u32, end as u32));
        at = end;
    }

    let block_size = fs.superblock().block_size;
    Ok(join(
        Joined {
            table: &table,
            links: &links,
            index: &index,
            raw_names: &raw_names,
            block_size,
        },
        root_path,
        start,
        options,
        &mut publish,
        &mut cancelled,
    ))
}

/// Everything the first two phases produced, ready to be walked.
struct Joined<'a> {
    table: &'a Table,
    links: &'a [Link],
    index: &'a FxHashMap<u32, (u32, u32)>,
    raw_names: &'a [u8],
    block_size: u64,
}

/// Everything one sweep of the inode tables produced.
struct Table {
    /// What each inode in use says about itself. Keyed by number, because the
    /// numbers are sparse — `alpine-musl` here uses 287,000 of 67 million.
    meta: FxHashMap<u32, format::Inode>,
    /// Something could not be read, so the tree's totals are a floor rather
    /// than quietly reporting less disk than there is.
    partial: bool,
}

/// One name, as a directory block gave it.
struct Link {
    parent: u32,
    child: u32,
    kind: Kind,
    at: u32,
    len: u16,
}

impl Link {
    fn name<'a>(&self, names: &'a [u8]) -> &'a [u8] {
        &names[self.at as usize..self.at as usize + self.len as usize]
    }
}

/// Phase one: every inode in use, one group's table at a time.
fn read_inodes<R: ReadAt>(
    fs: &mut Filesystem<R>,
    cancelled: &mut impl FnMut() -> bool,
) -> io::Result<Table> {
    let groups = fs.used_groups();
    let per_group = fs.superblock().inodes_per_group;
    let inode_size = fs.superblock().inode_size as usize;
    let superblock = *fs.superblock();

    let expected: usize = groups.iter().map(|(_, used, _)| *used as usize).sum();
    let mut meta: FxHashMap<u32, format::Inode> =
        FxHashMap::with_capacity_and_hasher(expected, Default::default());
    let mut raw: Vec<u8> = Vec::new();
    let mut partial = false;

    for (first, _, table) in groups {
        if cancelled() {
            return Ok(Table { meta, partial });
        }
        if fs.read_inode_table(table, &mut raw).is_err() {
            // A table that will not read leaves its inodes unknown, which the
            // join reports as entries it could not measure rather than as
            // entries that are not there.
            partial = true;
            continue;
        }
        for index in 0..per_group {
            let at = index as usize * inode_size;
            let Some(bytes) = raw.get(at..at + inode_size.min(256)) else {
                break;
            };
            let Some(inode) = format::inode(bytes, &superblock) else {
                continue;
            };
            if inode.in_use() {
                meta.insert(first + index, inode);
            }
        }
    }
    Ok(Table { meta, partial })
}

/// Phase two: every directory on the filesystem, read in disk order.
///
/// Every directory, including ones this scan will not report. That is a real
/// cost and a smaller one than it sounds: a directory is a few blocks, and
/// reading only the ones that matter *out of order* costs more than reading all
/// of them in order.
fn read_directories<R: ReadAt>(
    fs: &mut Filesystem<R>,
    table: &Table,
    names: &mut Vec<u8>,
    cancelled: &mut impl FnMut() -> bool,
) -> io::Result<Vec<Link>> {
    let mut directories: Vec<u32> = table
        .meta
        .iter()
        .filter(|(_, inode)| inode.kind() == Kind::Directory)
        .map(|(number, _)| *number)
        .collect();
    // By inode number, which is close to the order their blocks were allocated
    // in, so the few extent trees deep enough to need a read of their own are
    // read in order too.
    directories.sort_unstable();

    // Which directory each block belongs to. A block is one directory's, so
    // this is a plain list rather than a map.
    let mut owned: Vec<(u64, u32)> = Vec::with_capacity(directories.len() * 2);
    let mut scratch: Vec<u64> = Vec::new();
    for number in directories {
        if cancelled() {
            break;
        }
        let inode = table.meta[&number];
        if fs.directory_blocks(&inode, &mut scratch).is_err() {
            continue;
        }
        owned.extend(scratch.iter().map(|block| (*block, number)));
    }
    owned.sort_unstable();
    owned.dedup_by_key(|(block, _)| *block);

    let mut links: Vec<Link> = Vec::with_capacity(owned.len() * 8);
    let mut gathered = reader::Blocks::default();
    let mut wanted: Vec<u64> = Vec::with_capacity(GATHER_BATCH);
    let filetype = fs.superblock().filetype_in_dirents;

    for batch in owned.chunks(GATHER_BATCH) {
        if cancelled() {
            break;
        }
        wanted.clear();
        wanted.extend(batch.iter().map(|(block, _)| *block));
        fs.gather(&wanted, &mut gathered)?;

        for (block, parent) in batch {
            let Some(bytes) = gathered.get(*block) else {
                continue;
            };
            for entry in format::dir_entries(bytes) {
                // `.` and `..` are this directory and its parent. Following
                // either is how a walk becomes a loop.
                if entry.is_dot() {
                    continue;
                }
                let kind = if filetype {
                    if entry.is_directory() {
                        Kind::Directory
                    } else if entry.is_symlink() {
                        Kind::Symlink
                    } else {
                        Kind::File
                    }
                } else {
                    // An ext2 filesystem old enough to have no type in its
                    // entries. The inode is already in hand, which is the
                    // second thing reading the whole table buys.
                    match table.meta.get(&entry.inode) {
                        Some(inode) => inode.kind(),
                        None => continue,
                    }
                };

                let at = names.len() as u32;
                names.extend_from_slice(entry.name);
                links.push(Link {
                    parent: *parent,
                    child: entry.inode,
                    kind,
                    at,
                    len: entry.name.len() as u16,
                });
            }
        }
    }

    // Grouped by the directory they are in, so the join can find a directory's
    // children without searching.
    links.sort_unstable_by_key(|link| link.parent);
    Ok(links)
}

/// Phase three: the tree, breadth-first, from what is already in memory.
fn join(
    from: Joined<'_>,
    root_path: &Path,
    start: u32,
    options: &ScanOptions,
    publish: &mut impl FnMut(Progress),
    cancelled: &mut impl FnMut() -> bool,
) -> SizeTree {
    let Joined {
        table,
        links,
        index,
        raw_names,
        block_size,
    } = from;

    let mut nodes: Vec<Node> = Vec::with_capacity(links.len() + 1);
    let mut names = NameArena::with_capacity(raw_names.len() + 64);
    let mut stats = ScanStats {
        strategy: Strategy::DiskImage,
        partial: table.partial,
        // Free here — the inode number is in the structure being read — so the
        // rule is on unless somebody asked for it off, and either way the scan
        // records which, for the comparison to notice.
        dedup_hardlinks: options.dedups_hardlinks(true),
        ..ScanStats::default()
    };

    let root_meta = table.meta.get(&start).copied().unwrap_or_default();
    let root_label = root_path.file_name().unwrap_or(root_path.as_os_str());
    let (root_off, root_len) = names.push(root_label);
    nodes.push(Node {
        // A directory's apparent size is its contents', never its own entry
        // blocks. The same rule the walk follows, and the reason a tree
        // measured on Windows and on Linux agrees.
        logical: 0,
        allocated: root_meta.allocated,
        parent: 0,
        first_child: 0,
        child_count: 0,
        subtree_files: 0,
        name_off: root_off,
        mtime: root_meta.mtime,
        name_len: root_len,
        flags: Flags::DIR,
        depth: 0,
    });
    stats.dirs = 1;
    stats.entries = 1;

    // A directory cannot really be hardlinked, so a cycle means a corrupt
    // image — and a cycle here is a tree that never ends.
    let mut entered: FxHashSet<u32> = FxHashSet::default();
    entered.insert(start);
    // The first name a file is reached by owns its bytes.
    let mut counted: FxHashSet<u32> = FxHashSet::default();

    let mut frontier: Vec<(u32, u32)> = vec![(0, start)];
    let mut depth: u8 = 0;
    let mut children: Vec<&Link> = Vec::new();

    while !frontier.is_empty() {
        if cancelled() {
            stats.cancelled = true;
            stats.partial = true;
            break;
        }
        if options.depth_limit_reached(depth) {
            break;
        }

        let child_depth = depth.saturating_add(1);
        let mut next: Vec<(u32, u32)> = Vec::new();

        for &(parent_index, dir_inode) in &frontier {
            children.clear();
            if let Some(&(from, to)) = index.get(&dir_inode) {
                children.extend(&links[from as usize..to as usize]);
            }

            // Sorted by encoded name bytes, exactly as the walk sorts a
            // directory, so two scans of an unchanged tree — and two strategies
            // against each other — produce identical arenas.
            children.sort_unstable_by(|a, b| a.name(raw_names).cmp(b.name(raw_names)));

            let first_child = nodes.len() as u32;
            for link in &children {
                let (name_off, name_len) = push_name(&mut names, link.name(raw_names));

                let mut flags = Flags::default();
                // A link is a link first, exactly as in the walk: a symlink to
                // a directory is measured as itself and has no children.
                if link.kind == Kind::Symlink {
                    flags.insert(Flags::SYMLINK);
                } else if link.kind == Kind::Directory {
                    flags.insert(Flags::DIR);
                }
                let is_dir = link.kind == Kind::Directory;

                let mut logical = 0;
                let mut allocated = 0;
                let mut mtime = 0;
                match table.meta.get(&link.child) {
                    Some(inode) => {
                        mtime = inode.mtime;
                        // The bytes really occupied, from `i_blocks` — exact
                        // for a sparse file in the same way `st_blocks` is.
                        allocated = inode.allocated;
                        logical = if is_dir { 0 } else { inode.size };
                        // A whole block of slack, exactly as the Unix walk
                        // allows: without it every symlink short enough to live
                        // inside its own inode — which is nearly all of them —
                        // occupies nothing while claiming the length of its
                        // target, and comes out flagged as a sparse file.
                        // Saturating for the same reason the XFS reader's
                        // is: `allocated` is `i_blocks` times a block size,
                        // both off the disk, and a value near the top of a
                        // `u64` makes the sum a panic rather than a number.
                        // The sweep over this reader has never produced one —
                        // it needs `huge_file` set as well — which makes it a
                        // latent panic rather than an absent one.
                        if !is_dir && inode.allocated.saturating_add(block_size) < inode.size {
                            flags.insert(Flags::SPARSE);
                        }
                        if options.dedups_hardlinks(true)
                            && !is_dir
                            && inode.links > 1
                            && !counted.insert(link.child)
                        {
                            flags.insert(Flags::HARDLINK_DUP);
                            logical = 0;
                            allocated = 0;
                            stats.hardlinks_deduped += 1;
                        }
                    }
                    None => {
                        // A directory entry naming an inode that is not in use.
                        // The name is real and the thing it names is gone, so
                        // counting bytes for it would be counting bytes nothing
                        // owns.
                        flags.insert(Flags::UNREADABLE);
                        stats.unreadable += 1;
                        stats.partial = true;
                    }
                }

                if is_dir {
                    stats.dirs += 1;
                } else {
                    stats.files += 1;
                }

                nodes.push(Node {
                    logical,
                    allocated,
                    parent: parent_index,
                    first_child: 0,
                    child_count: 0,
                    subtree_files: u32::from(!is_dir),
                    name_off,
                    mtime,
                    name_len,
                    flags,
                    depth: child_depth,
                });

                if is_dir && entered.insert(link.child) {
                    next.push((nodes.len() as u32 - 1, link.child));
                }
            }

            nodes[parent_index as usize].first_child = first_child;
            nodes[parent_index as usize].child_count = children.len() as u32;
            stats.entries += children.len() as u64;
        }

        stats.max_depth = stats.max_depth.max(child_depth);
        publish(Progress {
            dirs_scanned: None,
            entries: stats.entries,
            dirs_pending: next.len() as u64,
            depth,
        });

        frontier = next;
        depth = child_depth;
    }

    crate::scan::aggregate(&mut nodes);
    nodes.shrink_to_fit();
    names.shrink_to_fit();

    stats.bytes_logical = nodes[0].logical;
    stats.bytes_allocated = nodes[0].allocated;

    SizeTree {
        nodes,
        names,
        owners: crate::owner::Owners::default(),
        created: Vec::new(),
        root_path: root_path.to_path_buf(),
        stats,
        // Every span came from `NameArena::push` or `push_bytes`, which store
        // only names this platform can hold.
        replaced_names: Vec::new(),
    }
}

/// Follow a path down from the filesystem root and say which inode it names.
///
/// A few directory reads, which is what makes scanning one directory of a
/// distribution cost what that directory costs rather than what the whole thing
/// costs.
///
/// `None` when some component is missing or is not a directory, which is the
/// ordinary answer for a path that was typed wrong and for one naming a file.
pub fn resolve<R: ReadAt>(fs: &mut Filesystem<R>, inside: &Path) -> io::Result<Option<u32>> {
    let mut current = format::ROOT_INODE;
    let mut blocks: Vec<u64> = Vec::new();
    let mut block = vec![0u8; fs.superblock().block_size as usize];

    for component in inside.components() {
        let wanted = component.as_os_str();
        let Some(inode) = fs.inode(current)? else {
            return Ok(None);
        };
        if inode.kind() != Kind::Directory || fs.directory_blocks(&inode, &mut blocks).is_err() {
            return Ok(None);
        }

        let mut found = None;
        let mine = std::mem::take(&mut blocks);
        'blocks: for number in &mine {
            if fs.read_block(*number, &mut block).is_err() {
                continue;
            }
            for entry in format::dir_entries(&block) {
                if entry.is_dot() {
                    continue;
                }
                // Compared as the caller spells it, which means comparing the
                // same lossy form the tree will store — otherwise a name that
                // is not UTF-8 could be resolved here and then be unfindable in
                // the tree it produced.
                if same_name(entry.name, wanted) {
                    found = Some(entry.inode);
                    break 'blocks;
                }
            }
        }
        blocks = mine;
        match found {
            Some(inode) => current = inode,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

#[cfg(unix)]
fn same_name(raw: &[u8], wanted: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    raw == wanted.as_bytes()
}

#[cfg(not(unix))]
fn same_name(raw: &[u8], wanted: &std::ffi::OsStr) -> bool {
    String::from_utf8_lossy(raw).as_bytes() == wanted.as_encoded_bytes()
}

/// Put a Linux name into the arena.
///
/// A Linux filename is any bytes but `/` and NUL, and the two platforms differ
/// in what they can carry back out. On Unix an `OsStr` *is* bytes, so a name
/// round-trips exactly. On Windows an `OsStr` is WTF-8 and arbitrary bytes are
/// not valid in it — the arena hands names back as `OsStr` without checking, so
/// storing raw bytes there would be unsound rather than merely untidy.
///
/// So on Windows a name that is not UTF-8 is stored lossily, with U+FFFD where
/// the bytes were. It is the same choice the export writers make and for the
/// same reason, and it means a scan of one image from the two platforms can
/// disagree about the spelling of a name that was never text. Every real
/// filename in a distribution is UTF-8.
#[cfg(unix)]
fn push_name(names: &mut NameArena, raw: &[u8]) -> (u32, u16) {
    use std::os::unix::ffi::OsStrExt;
    names.push(std::ffi::OsStr::from_bytes(raw))
}

#[cfg(not(unix))]
fn push_name(names: &mut NameArena, raw: &[u8]) -> (u32, u16) {
    names.push_bytes(String::from_utf8_lossy(raw).as_bytes())
}
