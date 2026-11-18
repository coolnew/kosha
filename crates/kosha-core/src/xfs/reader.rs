// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reading inodes and directories out of an XFS filesystem.
//!
//! The layer between [`format`](super::format), which turns bytes into
//! numbers, and [`build`](super::build), which turns a walk into a tree. What
//! it adds is the reads: where an inode is, which blocks a directory's entries
//! are in, and what is in them.
//!
//! **No file's extents are ever read.** A file's size and its allocation are
//! both in its inode, so the block map — which is most of the format — is only
//! ever walked for directories, and only far enough to find the blocks holding
//! entries. A directory's hash index is skipped by the same rule.

use std::io;

use crate::vhdx::ReadAt;

use super::format::{self, Fork, Inode, Superblock};

/// The most blocks one directory may be read as.
///
/// A directory's fork says how many extents it has, and that number comes off
/// the disk. Without a bound, one flipped bit asks for an allocation no
/// machine can serve — which is not an error anybody can catch, because a
/// failed allocation aborts the process. The same shape as the 68 GB inode
/// table the ext4 sweep found, and bounded here for the same reason.
///
/// Sixteen thousand blocks is a directory of tens of millions of entries, far
/// past anything a filesystem holds in one.
const MAX_DIRECTORY_BLOCKS: usize = 16 * 1024;

/// An open XFS filesystem.
pub struct Filesystem<R> {
    source: R,
    superblock: Superblock,
    /// One inode's bytes, reused. Every inode is the same size, and a scan
    /// reads a million of them.
    inode_buffer: Vec<u8>,
}

impl<R: ReadAt> Filesystem<R> {
    /// Read the superblock and take the geometry from it.
    pub fn open(mut source: R) -> io::Result<Filesystem<R>> {
        let mut bytes = [0u8; format::SUPERBLOCK_BYTES];
        source.read_exact_at(format::SUPERBLOCK_OFFSET, &mut bytes)?;
        let superblock = format::superblock(&bytes)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))?;

        Ok(Filesystem {
            inode_buffer: vec![0u8; superblock.inode_size as usize],
            source,
            superblock,
        })
    }

    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    /// Read one inode, and hand back both what it says and its raw bytes.
    ///
    /// The bytes come back too because a shortform directory lives inside
    /// them: reading the inode twice — once for the numbers and once for the
    /// fork — would double the cost of the one read a scan does most.
    pub fn inode(&mut self, number: u64) -> io::Result<Option<(Inode, &[u8])>> {
        let Some(at) = self.superblock.inode_offset(number) else {
            return Ok(None);
        };
        self.source.read_exact_at(at, &mut self.inode_buffer)?;
        let Some(inode) = format::inode(&self.inode_buffer, &self.superblock) else {
            return Ok(None);
        };
        Ok(Some((inode, &self.inode_buffer)))
    }

    /// Where a directory's entries are, as filesystem block numbers.
    ///
    /// Only the blocks that hold entries: a directory large enough to need one
    /// keeps a hash index above a fixed offset, and that index has no names in
    /// it. Skipping it is a correctness matter as much as a speed one — its
    /// blocks would parse as entries and produce a directory of rubbish.
    pub fn directory_blocks(&mut self, inode: &Inode, bytes: &[u8]) -> io::Result<Vec<u64>> {
        let extents = match inode.fork {
            // The entries are in the inode; there are no blocks.
            Fork::Local => return Ok(Vec::new()),
            Fork::Extents => {
                let fork = format::fork_of(bytes, inode);
                let wanted = inode.extents.min(MAX_DIRECTORY_BLOCKS as u64) as usize;
                (0..wanted)
                    .map_while(|index| format::extent(fork, index))
                    .collect()
            }
            Fork::Btree => self.btree_extents(inode, bytes)?,
        };

        let leaf = self.superblock.leaf_block();
        let per_directory_block = 1u64 << self.superblock.dir_block_log;
        let mut blocks = Vec::new();
        for extent in extents {
            if extent.start_offset >= leaf {
                continue;
            }
            // A directory block is several filesystem blocks, and only the
            // first of each carries a header — so the run is taken in
            // directory-block steps rather than one block at a time.
            let mut taken = 0u64;
            while taken < extent.blocks {
                if blocks.len() >= MAX_DIRECTORY_BLOCKS {
                    return Err(malformed(
                        "a directory claims more blocks than kosha will read",
                    ));
                }
                blocks.push(extent.start_block + taken);
                taken += per_directory_block;
            }
        }
        Ok(blocks)
    }

    /// Walk a directory's block-map B+tree, collecting the extents in it.
    ///
    /// **Not only for enormous directories.** A tree happens when the extents
    /// stop fitting in the inode, and that is fragmentation rather than size:
    /// on the real `xfs` disk this was checked against, twelve directories of
    /// a few hundred entries each had one, because the filesystem they live on
    /// is written to constantly. Declining them lost whole subtrees.
    fn btree_extents(&mut self, inode: &Inode, bytes: &[u8]) -> io::Result<Vec<format::Extent>> {
        let fork = format::fork_of(bytes, inode);
        let root = format::btree_root(fork)
            .ok_or_else(|| malformed("this directory's block map has no root"))?;

        let block_size = self.superblock.block_size as usize;
        let mut buffer = vec![0u8; block_size];
        let mut extents = Vec::new();

        // Depth first, one node at a time, with the level each pointer was
        // found at carried alongside it — so a node claiming to be at a level
        // it was not reached from is refused rather than followed.
        let mut pending: Vec<(u64, u16)> = Vec::new();
        for index in 0..usize::from(root.count) {
            if let Some(pointer) = format::btree_pointer(fork, &root, index) {
                pending.push((pointer, root.level - 1));
            }
        }

        let mut visited = 0usize;
        while let Some((block, expected)) = pending.pop() {
            visited += 1;
            // Every node is a block, so the same bound that stops a directory
            // being read forever stops its block map being walked forever —
            // including a tree whose pointers form a cycle.
            if visited > MAX_DIRECTORY_BLOCKS {
                return Err(malformed(
                    "a directory's block map is larger than kosha will read",
                ));
            }

            let Some(at) = self.superblock.block_offset(block) else {
                continue;
            };
            if self.source.read_exact_at(at, &mut buffer).is_err() {
                continue;
            }
            let Some(node) = format::btree_node(&buffer, block_size) else {
                continue;
            };
            if node.level != expected {
                continue;
            }

            if node.level == 0 {
                let records = &buffer[node.header..];
                for index in 0..usize::from(node.count) {
                    let Some(extent) = format::extent(records, index) else {
                        break;
                    };
                    extents.push(extent);
                }
            } else {
                for index in 0..usize::from(node.count) {
                    if let Some(pointer) = format::btree_pointer(&buffer, &node, index) {
                        pending.push((pointer, node.level - 1));
                    }
                }
            }
        }
        Ok(extents)
    }

    /// Read one directory block into `buffer`, which is sized for one.
    pub fn read_directory_block(&mut self, block: u64, buffer: &mut [u8]) -> io::Result<()> {
        let at = self
            .superblock
            .block_offset(block)
            .ok_or_else(|| malformed("a block outside the disk"))?;
        self.source.read_exact_at(at, buffer)
    }
}

/// Every entry of a directory, whatever shape it is stored in.
///
/// `each` is handed a name and an inode number and nothing else — the child's
/// own size is in its inode, which the caller reads when it gets there. A
/// directory that cannot be read at all is an error; one whose blocks are
/// individually damaged reports the entries it could read, because half a
/// directory is worth more than none and the tree says it is partial.
pub fn entries<R: ReadAt>(
    fs: &mut Filesystem<R>,
    inode: &Inode,
    bytes: &[u8],
    buffer: &mut Vec<u8>,
    mut each: impl FnMut(&[u8], u64),
) -> io::Result<bool> {
    let has_ftype = fs.superblock().has_ftype;

    if inode.fork == Fork::Local {
        let fork = format::fork_of(bytes, inode);
        let read = format::shortform(fork, has_ftype, |entry| each(entry.name, entry.inode));
        return Ok(read.is_some());
    }

    let blocks = fs.directory_blocks(inode, bytes)?;
    let size = fs.superblock().dir_block_size() as usize;
    buffer.resize(size, 0);

    let mut whole = true;
    for block in blocks {
        if fs.read_directory_block(block, buffer).is_err() {
            whole = false;
            continue;
        }
        let Some((kind, start)) = format::block_kind(buffer) else {
            whole = false;
            continue;
        };
        let end = match kind {
            format::Block::Data => size,
            format::Block::Single => match format::single_block_end(buffer) {
                Some(end) => end,
                None => {
                    whole = false;
                    continue;
                }
            },
            // A hash index or a free-space map: no names in it, and skipping
            // it is what makes the answer right rather than what makes it
            // fast.
            format::Block::Other => continue,
        };
        if format::data_entries(buffer, start, end, has_ftype, |entry| {
            each(entry.name, entry.inode)
        })
        .is_none()
        {
            whole = false;
        }
    }
    Ok(whole)
}

fn malformed(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}
