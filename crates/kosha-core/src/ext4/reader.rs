// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reading an ext filesystem: geometry, inodes, and a directory's blocks.
//!
//! Generic over [`ReadAt`], which is what lets the whole of this run against a
//! `Vec<u8>` in a test as readily as against a virtual disk. A synthetic
//! filesystem built byte by byte is walked by exactly the code that walks a
//! real one — there is no test-only path through any of it.
//!
//! ## What it reads, and what it does not
//!
//! Only two things are ever read off the disk: **inodes**, and the **data
//! blocks of directories**. A regular file's contents are never touched, which
//! is what makes measuring a whole distribution a matter of reading a few
//! hundred megabytes rather than the whole image.
//!
//! ## The cache
//!
//! Reads are 4 KiB structures scattered across a file that is tens or hundreds
//! of gigabytes. Asked one at a time that is hundreds of thousands of round
//! trips; asked in 256 KiB chunks with the chunks kept, it is a few thousand.
//! The cache is direct-mapped — slot `chunk % slots` — which costs one
//! comparison, never allocates after the first pass over it, and behaves well
//! for exactly the two access patterns here: a directory's blocks, which sit
//! near each other, and a sweep through an inode table, which is sequential.

use std::io;

use super::format::{self, Inode, Superblock};
use crate::vhdx::{Placement, ReadAt};

/// How much is read and kept at a time.
///
/// A quarter of the megabyte a VHDX block is, so a chunk never straddles two of
/// them and a chunk read is always one seek. Larger wastes reads on a sparse
/// inode table; smaller gives up the sequential throughput that the second pass
/// depends on.
const CHUNK: u64 = 256 * 1024;

/// How many chunks to keep: 64 MiB.
const SLOTS: usize = 64;

/// The most group descriptors worth reading.
///
/// A 1 TiB filesystem has eight thousand and the table is half a megabyte. The
/// cap is here so that a corrupt block count cannot ask for a terabyte of them.
const MAX_GROUPS: u64 = 1 << 22;

/// The largest inode table worth reading, per block group: 256 MiB.
///
/// A real one is a couple of megabytes. This exists because the size is a
/// product of two numbers read out of the image, and a product can overflow
/// what either factor's own bound allows.
const MAX_INODE_TABLE: u64 = 256 * 1024 * 1024;

/// How far apart two wanted blocks may be and still be read as one.
///
/// A seek on a spinning disk costs about as much as reading a quarter of a
/// megabyte, so merging across a gap smaller than that is free. Measured on
/// this machine's `alpine-musl`: 40,120 of 42,196 gaps between directory blocks
/// are *adjacent* and only 192 are far apart, so nearly all of it merges into a
/// handful of long sequential runs.
const GAP: u64 = 64;

/// The most blocks to read in one go, so the scratch buffer stays bounded.
const MAX_RUN: usize = 2048;

/// The most nodes of one inode's extent tree to visit.
///
/// The tree is five levels deep at the very most and a directory needs a
/// handful of nodes. This is not a limit anything real reaches; it is what
/// stops a corrupt image whose tree points at itself from being walked forever.
const MAX_EXTENT_NODES: usize = 4096;

/// An ext filesystem, open for reading.
pub struct Filesystem<R: ReadAt> {
    source: R,
    superblock: Superblock,
    /// The inode table's first block, per group. Read once at open: it is the
    /// only thing this needs out of the group descriptors, and looking one up
    /// per inode would mean a read per file.
    tables: Vec<u64>,
    /// How many inodes each group has in use.
    ///
    /// The number that makes the whole strategy affordable. This machine's
    /// `alpine-musl` has 8,192 groups and 67 million inodes — sixteen gigabytes
    /// of table — and **thirty-five** of those groups hold anything at all, for
    /// 70 MiB worth. Reading the tables that are in use is a sequential sweep;
    /// reading them all is not an option and reading one inode at a time is a
    /// few hundred thousand seeks.
    used: Vec<u32>,
    cache: Cache,
}

impl<R: ReadAt> Filesystem<R> {
    /// Read the superblock and the group descriptors.
    pub fn open(mut source: R) -> io::Result<Filesystem<R>> {
        let mut bytes = [0u8; 1024];
        source.read_exact_at(format::SUPERBLOCK_OFFSET, &mut bytes)?;
        let superblock = format::superblock(&bytes)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))?;

        let groups = superblock.group_count().min(MAX_GROUPS);
        let descriptor_size = u64::from(superblock.descriptor_size);
        let table_at = superblock.descriptor_table_block() * superblock.block_size;
        let length = usize::try_from(groups * descriptor_size)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many block groups"))?;

        let mut raw = vec![0u8; length];
        source.read_exact_at(table_at, &mut raw)?;

        let tables = raw
            .chunks_exact(descriptor_size as usize)
            .map(|descriptor| {
                format::inode_table(descriptor, superblock.descriptor_size).unwrap_or(0)
            })
            .collect();
        let used = raw
            .chunks_exact(descriptor_size as usize)
            .map(|descriptor| {
                superblock
                    .inodes_per_group
                    .saturating_sub(format::free_inodes(descriptor))
            })
            .collect();

        Ok(Filesystem {
            source,
            superblock,
            tables,
            used,
            cache: Cache::new(),
        })
    }

    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    /// Read one inode.
    ///
    /// `None` for a number outside the filesystem, or in a group whose
    /// descriptor gave no inode table — both of which mean a directory entry
    /// pointed somewhere there is nothing, which is corruption rather than an
    /// error worth failing a whole scan over.
    pub fn inode(&mut self, number: u32) -> io::Result<Option<Inode>> {
        if number == 0 || number > self.superblock.inodes_count {
            return Ok(None);
        }
        let Some(group) = self.superblock.group_of(number) else {
            return Ok(None);
        };
        let Some(&table) = self.tables.get(group as usize) else {
            return Ok(None);
        };
        if table == 0 {
            return Ok(None);
        }
        let Some(at) = self.superblock.inode_offset(table, number) else {
            return Ok(None);
        };

        // Only the first 128 bytes are guaranteed, and everything read here is
        // inside them except the two high halves, which are at 0x74 and 0x78.
        let mut bytes = vec![0u8; self.superblock.inode_size.min(256) as usize];
        self.read_at(at, &mut bytes)?;
        Ok(format::inode(&bytes, &self.superblock))
    }

    /// Every group holding inodes, as `(first inode number, how many, where
    /// the table starts)`.
    ///
    /// A group whose descriptor says nothing is in use is skipped outright,
    /// which is what turns sixteen gigabytes of table into seventy megabytes.
    pub fn used_groups(&self) -> Vec<(u32, u32, u64)> {
        self.used
            .iter()
            .zip(&self.tables)
            .enumerate()
            .filter(|(_, (used, table))| **used > 0 && **table != 0)
            .map(|(group, (used, table))| {
                let first = group as u32 * self.superblock.inodes_per_group + 1;
                (first, *used, *table)
            })
            .collect()
    }

    /// Read a whole group's inode table in one go.
    ///
    /// Past the cache, like [`gather`](Self::gather): two megabytes read once
    /// and never again would evict everything worth keeping.
    pub fn read_inode_table(&mut self, table: u64, into: &mut Vec<u8>) -> io::Result<()> {
        let bytes =
            u64::from(self.superblock.inodes_per_group) * u64::from(self.superblock.inode_size);
        // Both factors come out of the superblock, and `superblock()` bounds
        // each of them on its own. This bounds the product, which is the thing
        // actually allocated: two fields that are each individually plausible
        // can still multiply into an allocation no machine can serve, and a
        // failed allocation is an abort rather than an error — the process is
        // simply gone, mid-scan, with nothing written anywhere.
        //
        // A real group's table is a couple of megabytes; the comment above
        // says two. A quarter of a gigabyte is far past any filesystem anybody
        // will hand this and still small enough to fail politely.
        if bytes > MAX_INODE_TABLE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the inode table is larger than any real filesystem's",
            ));
        }
        let at = table
            .checked_mul(self.superblock.block_size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "table past the disk"))?;
        into.clear();
        into.resize(bytes as usize, 0);
        self.source.read_exact_at(at, into)
    }

    /// Read one filesystem block.
    pub fn read_block(&mut self, block: u64, into: &mut [u8]) -> io::Result<()> {
        let at = block
            .checked_mul(self.superblock.block_size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "block past the disk"))?;
        self.read_at(at, into)
    }

    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> io::Result<()> {
        self.cache.read(&mut self.source, offset, into)
    }

    /// Read many blocks at once, in the order the disk can serve them.
    ///
    /// **The difference between ten seconds and two.** Two things have to be
    /// sorted, and sorting only one of them is worse than useless:
    ///
    /// * The filesystem's own order. A breadth-first walk asks for directory
    ///   blocks in tree order while they were laid down in something closer to
    ///   creation order — on this machine's `alpine-musl`, 40,120 of the 42,196
    ///   gaps between sorted directory blocks are *adjacent*, so the blocks are
    ///   very nearly one long run that a tree-order walk crosses and re-crosses.
    /// * The *image's* order. A virtual disk is a map, and blocks next to each
    ///   other in the filesystem are wherever in the file they were first
    ///   written. Sorting by filesystem block and stopping there produced 1,570
    ///   runs that were each a full-stroke seek across a 28 GB file, which was
    ///   slower than not sorting at all.
    ///
    /// So every block is resolved to where it really is, sorted by *that*, and
    /// neighbours merged. What was forty thousand scattered reads becomes a
    /// sweep.
    ///
    /// Deliberately past the cache: these blocks are read once each and never
    /// again, and putting them through a cache of a few dozen slots evicts the
    /// inode chunks that are worth keeping.
    pub fn gather(&mut self, wanted: &[u64], into: &mut Blocks) -> io::Result<()> {
        let block_size = self.superblock.block_size as usize;
        into.reset(block_size, wanted.len());

        // Where each one really is. A block that was never written reads as
        // zeros and needs no read at all, which on a sparse image is most of
        // the address space and none of the directories.
        let mut placed: Vec<(u64, u64)> = Vec::with_capacity(wanted.len());
        for block in wanted {
            let Some(offset) = block.checked_mul(self.superblock.block_size) else {
                continue;
            };
            if let Placement::At(at) = self.source.placement(offset) {
                placed.push((at, *block));
            }
        }
        placed.sort_unstable();

        let gap = GAP * self.superblock.block_size;
        let max_run = (MAX_RUN * block_size) as u64;
        let mut scratch = vec![0u8; MAX_RUN * block_size];

        let mut at = 0usize;
        while at < placed.len() {
            let first = placed[at].0;
            let mut last = first;
            let mut end = at + 1;
            // Keep taking blocks while the next one is close enough that
            // reading the space between it and the last costs less than the
            // seek avoided.
            while end < placed.len()
                && placed[end].0.saturating_sub(last) <= gap
                && placed[end].0 - first + self.superblock.block_size <= max_run
            {
                last = placed[end].0;
                end += 1;
            }

            let bytes = (last - first) as usize + block_size;
            if self
                .source
                .read_placed(first, &mut scratch[..bytes])
                .is_err()
            {
                // A run that will not read at all. Every block in it is left
                // absent, which the caller reports as a directory it could not
                // read rather than as an empty one.
                at = end;
                continue;
            }

            for &(place, block) in &placed[at..end] {
                let within = (place - first) as usize;
                into.put(block, &scratch[within..within + block_size]);
            }
            at = end;
        }
        Ok(())
    }

    /// The filesystem blocks holding a directory's entries, in order.
    ///
    /// Two entirely different maps, and which one an inode uses is in its own
    /// flags rather than in the superblock: an extent tree on anything made
    /// this century, and the older chain of indirect blocks on an ext2 or ext3
    /// filesystem, or on a file that predates an upgrade.
    ///
    /// Only ever called for directories. A regular file's blocks are never
    /// wanted — its size and its allocation are both in the inode — so the
    /// cost of this is bounded by how many directories there are rather than by
    /// how much data they contain.
    pub fn directory_blocks(&mut self, inode: &Inode, out: &mut Vec<u64>) -> io::Result<()> {
        out.clear();
        if inode.flags & format::INODE_INLINE_DATA != 0 {
            // The entries are inside the inode and in its extended attributes,
            // in a second layout. Nothing WSL makes has the feature; a
            // directory that does is reported unreadable rather than empty,
            // because empty is a claim and unreadable is the truth.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this directory keeps its entries inside its inode",
            ));
        }
        if inode.flags & format::INODE_EXTENTS != 0 {
            self.extent_blocks(inode, out)
        } else {
            self.indirect_blocks(inode, out)
        }
    }

    /// Walk an extent tree, depth first, collecting every block it maps.
    ///
    /// Iterative with an explicit stack rather than recursive: the borrow of
    /// the node has to end before the next block can be read through `self`,
    /// and a stack of owned nodes says that plainly.
    fn extent_blocks(&mut self, inode: &Inode, out: &mut Vec<u64>) -> io::Result<()> {
        let mut stack: Vec<Vec<u8>> = vec![inode.block_map.to_vec()];
        let mut visited = 0usize;

        while let Some(node) = stack.pop() {
            visited += 1;
            if visited > MAX_EXTENT_NODES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "this extent tree does not end",
                ));
            }
            let Some(header) = format::extent_header(&node) else {
                // Not an extent node. A directory whose tree we cannot follow
                // is unreadable rather than empty.
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "this inode claims extents and does not have them",
                ));
            };

            for index in 0..usize::from(header.entries) {
                if header.depth == 0 {
                    let Some(extent) = format::extent(&node, index) else {
                        continue;
                    };
                    for block in 0..u64::from(extent.length) {
                        out.push(extent.start + block);
                    }
                } else {
                    let Some(child) = format::extent_child(&node, index) else {
                        continue;
                    };
                    let mut bytes = vec![0u8; self.superblock.block_size as usize];
                    self.read_block(child, &mut bytes)?;
                    stack.push(bytes);
                }
            }
        }
        Ok(())
    }

    /// The pre-extent map: twelve blocks named directly, then one, two and
    /// three levels of indirection.
    ///
    /// Only the first two levels are followed. The third addresses a directory
    /// larger than every directory that has ever existed, and pretending to
    /// support it would mean writing a path that can never be tested.
    fn indirect_blocks(&mut self, inode: &Inode, out: &mut Vec<u64>) -> io::Result<()> {
        let per_block = (self.superblock.block_size / 4) as usize;
        // A directory's size is the space its entry blocks take, so this is
        // exactly how many blocks to expect and a natural bound on the walk.
        let wanted = inode.size.div_ceil(self.superblock.block_size);

        // Sixty bytes is exactly fifteen pointers, which is what makes the
        // three indices below safe without checking.
        let pointers: [u32; 15] = std::array::from_fn(|index| {
            let four = inode.block_map[index * 4..index * 4 + 4]
                .try_into()
                .expect("a slice of exactly four bytes");
            u32::from_le_bytes(four)
        });

        for &direct in pointers.iter().take(12) {
            if out.len() as u64 >= wanted {
                return Ok(());
            }
            if direct != 0 {
                out.push(u64::from(direct));
            }
        }

        // Single indirect: a block full of block numbers.
        if pointers[12] != 0 && (out.len() as u64) < wanted {
            let single = self.pointer_block(u64::from(pointers[12]), per_block)?;
            for block in single {
                if out.len() as u64 >= wanted {
                    return Ok(());
                }
                if block != 0 {
                    out.push(u64::from(block));
                }
            }
        }

        // Double indirect: a block full of pointers to blocks full of numbers.
        if pointers[13] != 0 && (out.len() as u64) < wanted {
            let outer = self.pointer_block(u64::from(pointers[13]), per_block)?;
            for middle in outer {
                if middle == 0 || (out.len() as u64) >= wanted {
                    continue;
                }
                for block in self.pointer_block(u64::from(middle), per_block)? {
                    if out.len() as u64 >= wanted {
                        return Ok(());
                    }
                    if block != 0 {
                        out.push(u64::from(block));
                    }
                }
            }
        }
        Ok(())
    }

    fn pointer_block(&mut self, block: u64, per_block: usize) -> io::Result<Vec<u32>> {
        let mut bytes = vec![0u8; self.superblock.block_size as usize];
        self.read_block(block, &mut bytes)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .take(per_block)
            .map(|four| u32::from_le_bytes(*four))
            .collect())
    }
}

/// Blocks read by [`Filesystem::gather`], ready to be looked up in any order.
///
/// One byte buffer and an index into it rather than a map of vectors: a level
/// of a large filesystem is tens of thousands of blocks, and that many small
/// allocations costs more than reading them did.
#[derive(Default)]
pub struct Blocks {
    index: rustc_hash::FxHashMap<u64, u32>,
    bytes: Vec<u8>,
    block_size: usize,
}

impl Blocks {
    fn reset(&mut self, block_size: usize, expected: usize) {
        self.index.clear();
        self.bytes.clear();
        self.bytes.reserve(expected * block_size);
        self.block_size = block_size;
    }

    fn put(&mut self, block: u64, bytes: &[u8]) {
        let at = (self.bytes.len() / self.block_size) as u32;
        self.bytes.extend_from_slice(bytes);
        self.index.insert(block, at);
    }

    /// The bytes of one block, or `None` if it could not be read.
    pub fn get(&self, block: u64) -> Option<&[u8]> {
        let at = *self.index.get(&block)? as usize * self.block_size;
        self.bytes.get(at..at + self.block_size)
    }
}

// -------------------------------------------------------------------- cache

struct Slot {
    /// Which chunk is in `bytes`, and `u64::MAX` for a slot never filled.
    chunk: u64,
    bytes: Vec<u8>,
}

struct Cache {
    slots: Vec<Slot>,
}

impl Cache {
    fn new() -> Cache {
        Cache {
            slots: (0..SLOTS)
                .map(|_| Slot {
                    chunk: u64::MAX,
                    bytes: Vec::new(),
                })
                .collect(),
        }
    }

    /// Fill `into` from `offset`, a chunk at a time.
    ///
    /// A read can span two chunks — an inode never does, but a group
    /// descriptor table is half a megabyte — so this loops rather than
    /// assuming.
    fn read(&mut self, source: &mut impl ReadAt, offset: u64, into: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < into.len() {
            let at = offset + done as u64;
            let chunk = at / CHUNK;
            let within = (at % CHUNK) as usize;

            let slot = &mut self.slots[(chunk % SLOTS as u64) as usize];
            if slot.chunk != chunk || slot.bytes.is_empty() {
                // A chunk near the end of the image is short. `size` is what
                // says how short, and without it every read of the last chunk
                // would fail rather than returning what is there.
                let start = chunk * CHUNK;
                let length = CHUNK.min(source.size().saturating_sub(start));
                if length == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "read past the end of the filesystem",
                    ));
                }
                slot.bytes.clear();
                slot.bytes.resize(length as usize, 0);
                source.read_exact_at(start, &mut slot.bytes)?;
                slot.chunk = chunk;
            }

            let available = slot.bytes.len().saturating_sub(within);
            if available == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "read past the end of the filesystem",
                ));
            }
            let take = available.min(into.len() - done);
            into[done..done + take].copy_from_slice(&slot.bytes[within..within + take]);
            done += take;
        }
        Ok(())
    }
}
