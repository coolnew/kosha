// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! XFS on-disk structures, as bytes in and numbers out.
//!
//! **Big-endian, all of it.** That is the first thing that differs from every
//! other format kosha reads, and it is the difference that produces plausible
//! nonsense rather than an obvious failure when it is forgotten: a block size
//! of 4096 read the wrong way round is 1,048,576, which is a number a
//! filesystem could have.
//!
//! ## What is needed, and what is not
//!
//! Measuring a disk needs every file's name, parent and size. In XFS the sizes
//! are both in the inode — `di_size` and `di_nblocks` — so **a file's extents
//! are never read at all**. What has to be walked is directories: their fork,
//! and the blocks it points at. That cuts out the block map B+tree for
//! regular files, which is most of the format and none of the answer.
//!
//! ```text
//! 0x0000  superblock       "XFSB", geometry, where the root inode is
//! ino     inode            "IN", a mode, a size, and a fork
//! fork    shortform dir    entries inline, for a directory that is small
//!   or    extent list      packed 128-bit records, pointing at blocks
//! block   directory data   "XDD3"/"XDB3", entries with names in them
//! ```
//!
//! ## What is declined, and why it is named
//!
//! A directory whose fork is a B+tree rather than an extent list, and a
//! filesystem whose features move the furniture — those are named refusals
//! rather than silent gaps, because a directory reported empty is a wrong
//! answer and a directory reported unreadable is a true one.

/// Where the superblock is, and how much of it is worth reading.
pub const SUPERBLOCK_OFFSET: u64 = 0;
pub const SUPERBLOCK_BYTES: usize = 512;

const MAGIC: &[u8; 4] = b"XFSB";
const INODE_MAGIC: &[u8; 2] = b"IN";

/// `sb_versionnum` keeps the version in its low nibble and feature bits above.
const VERSION_MASK: u16 = 0x000F;
/// The feature bit that says `sb_features2` is worth reading at all.
const VERSION_MOREBITS: u16 = 0x8000;
/// `sb_features2`: directory entries carry a file type. The v5 spelling of the
/// same thing is an incompat bit; both are read, because both exist on disks.
const FEATURES2_FTYPE: u32 = 0x0000_0200;
/// `sb_features_incompat`, v5 only.
const INCOMPAT_FTYPE: u32 = 0x0000_0001;
/// Everything in `sb_features_incompat` that kosha knows about. A bit outside
/// this set means the disk uses something that was invented after this reader,
/// and the honest answer is to decline rather than to guess.
const INCOMPAT_KNOWN: u32 = 0x0000_0001 // ftype
    | 0x0000_0002 // sparse inodes: changes allocation, not addressing
    | 0x0000_0004 // meta uuid
    | 0x0000_0008 // big timestamps
    | 0x0000_0010 // needsrepair — the filesystem says so, and reading is still reading
    | INCOMPAT_NREXT64;

/// `sb_features_incompat`: 64-bit extent counters.
///
/// **This one moves the furniture, which is why it is not just another bit.**
/// An inode's data-fork extent count normally sits at 0x4C as a `u32`; with
/// this feature it is a `u64` at 0x18, in the eight bytes that were padding,
/// and 0x4C becomes padding instead. Reading the old place on a new
/// filesystem gives zero, and a directory with zero extents is a directory
/// reported empty — a wrong answer that looks like a right one.
///
/// Not hypothetical: the 195 GiB `xfs` data disk this reader was checked
/// against has it, and `mkfs.xfs` has been turning it on by default since
/// xfsprogs 6.0.
const INCOMPAT_NREXT64: u32 = 0x0000_0020;

/// A directory's blocks below this logical offset hold entries; at or above it
/// they hold the hash index, which is not needed to enumerate a directory.
///
/// Fixed by the format at 32 GiB, expressed in blocks.
pub const LEAF_OFFSET_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// Sizes a real filesystem uses, and the bounds a flipped bit has to fall
/// outside of. Every one of these divides or indexes something later.
const MIN_BLOCK: u64 = 512;
const MAX_BLOCK: u64 = 65536;
const MIN_INODE: u64 = 256;
const MAX_INODE: u64 = 2048;

/// Why an XFS filesystem could not be read.
///
/// Named rather than collapsed into one error, for the same reason the ext
/// reader names its own: "kosha cannot read this" is not something anybody can
/// act on, and "this filesystem uses a feature kosha does not know" is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unreadable {
    #[error("this is not an XFS filesystem")]
    NotXfs,
    #[error("the filesystem's geometry does not make sense")]
    Impossible,
    #[error("this filesystem uses a feature kosha does not know about")]
    UnknownFeature,
}

/// The geometry of a filesystem, and the one feature that changes how a
/// directory entry is laid out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Superblock {
    pub block_size: u64,
    pub inode_size: u64,
    pub ag_blocks: u64,
    pub ag_count: u32,
    pub root_inode: u64,
    pub total_blocks: u64,
    /// `sb_inopblog`: how many bits of an inode number are the slot within a
    /// block.
    pub inode_log: u32,
    /// `sb_agblklog`: how many bits are the block within an allocation group.
    pub ag_block_log: u32,
    /// `sb_dirblklog`: a directory block is this many filesystem blocks.
    pub dir_block_log: u32,
    /// 4 or 5. Version 5 puts a CRC header on every metadata block, which
    /// moves every directory entry 48 bytes further in.
    pub version: u16,
    /// Directory entries carry a one-byte file type after the name.
    pub has_ftype: bool,
    /// Extent counts are 64-bit and live somewhere else in the inode. See
    /// [`INCOMPAT_NREXT64`].
    pub big_extent_counts: bool,
}

impl Superblock {
    /// Bytes in one directory block, which is not the same as a filesystem
    /// block: `mkfs.xfs -n size=` makes them bigger so a large directory needs
    /// fewer of them.
    pub fn dir_block_size(&self) -> u64 {
        self.block_size << self.dir_block_log
    }

    /// Where a filesystem block starts, in bytes.
    ///
    /// A block number is an allocation group and an offset within it, packed
    /// into one integer — so this is two shifts and a multiply rather than one
    /// multiply, and getting it wrong reads a plausible block from the wrong
    /// group.
    pub fn block_offset(&self, block: u64) -> Option<u64> {
        let group = block >> self.ag_block_log;
        let within = block & ((1u64 << self.ag_block_log) - 1);
        if group >= u64::from(self.ag_count) || within >= self.ag_blocks {
            return None;
        }
        group
            .checked_mul(self.ag_blocks)?
            .checked_add(within)?
            .checked_mul(self.block_size)
    }

    /// Where an inode is, in bytes.
    ///
    /// The same packing again with a third field: the group, the block within
    /// it, and the slot within the block.
    pub fn inode_offset(&self, inode: u64) -> Option<u64> {
        let shift = self.ag_block_log.checked_add(self.inode_log)?;
        if shift >= 64 {
            return None;
        }
        let group = inode >> shift;
        let block = (inode >> self.inode_log) & ((1u64 << self.ag_block_log) - 1);
        let slot = inode & ((1u64 << self.inode_log) - 1);
        if group >= u64::from(self.ag_count) || block >= self.ag_blocks {
            return None;
        }
        group
            .checked_mul(self.ag_blocks)?
            .checked_add(block)?
            .checked_mul(self.block_size)?
            .checked_add(slot.checked_mul(self.inode_size)?)
    }

    /// The first directory block that holds the hash index rather than
    /// entries.
    pub fn leaf_block(&self) -> u64 {
        LEAF_OFFSET_BYTES / self.block_size
    }
}

/// Read a superblock out of the first sector.
pub fn superblock(bytes: &[u8]) -> Result<Superblock, Unreadable> {
    if bytes.len() < SUPERBLOCK_BYTES || &bytes[0..4] != MAGIC {
        return Err(Unreadable::NotXfs);
    }

    let block_size = u64::from(be32(bytes, 0x04));
    let total_blocks = be64(bytes, 0x08);
    let root_inode = be64(bytes, 0x38);
    let ag_blocks = u64::from(be32(bytes, 0x54));
    let ag_count = be32(bytes, 0x58);
    let versionnum = be16(bytes, 0x64);
    let inode_size = u64::from(be16(bytes, 0x68));
    let inode_log = u32::from(bytes[0x7B]);
    let ag_block_log = u32::from(bytes[0x7C]);
    let dir_block_log = u32::from(bytes[0xC0]);
    let version = versionnum & VERSION_MASK;

    if !(MIN_BLOCK..=MAX_BLOCK).contains(&block_size) || !block_size.is_power_of_two() {
        return Err(Unreadable::Impossible);
    }
    if !(MIN_INODE..=MAX_INODE).contains(&inode_size)
        || !inode_size.is_power_of_two()
        || inode_size > block_size
    {
        return Err(Unreadable::Impossible);
    }
    // **Every shift is bounded before it is used, and that ordering is the
    // whole of this paragraph.** These three fields are single bytes off the
    // disk, so each can be 255, and `1u64 << 255` is not a large number — it
    // is a panic. This check sat one line *below* the first shift that used
    // it until a sweep flipped a bit in `sb_inopblog`, which is the sort of
    // thing that reads correctly and runs wrong.
    if ag_block_log >= 64 || inode_log >= 32 || dir_block_log >= 32 {
        return Err(Unreadable::Impossible);
    }
    if ag_count == 0 || ag_blocks == 0 {
        return Err(Unreadable::Impossible);
    }
    // The slot bits have to be exactly the ones an inode of this size needs,
    // or every inode address is computed against a geometry the disk does not
    // have. Checked rather than derived, because the field is on the disk and
    // a disagreement is corruption rather than a detail.
    if block_size / inode_size != 1u64 << inode_log {
        return Err(Unreadable::Impossible);
    }
    // A group cannot hold more blocks than its own log says it can address.
    if ag_blocks > 1u64 << ag_block_log {
        return Err(Unreadable::Impossible);
    }
    // A directory block of more than 64 KiB is not something mkfs makes, and
    // the field is a shift: believing a large one asks for an allocation.
    if dir_block_log > 6 || block_size << dir_block_log > MAX_BLOCK {
        return Err(Unreadable::Impossible);
    }
    if root_inode == 0 {
        return Err(Unreadable::Impossible);
    }
    if !(4..=5).contains(&version) {
        return Err(Unreadable::UnknownFeature);
    }

    let (has_ftype, big_extent_counts) = if version == 5 {
        let incompat = be32(bytes, 0xD8);
        if incompat & !INCOMPAT_KNOWN != 0 {
            return Err(Unreadable::UnknownFeature);
        }
        (
            incompat & INCOMPAT_FTYPE != 0,
            incompat & INCOMPAT_NREXT64 != 0,
        )
    } else {
        // Version 4 predates both, and `sb_features2` is only worth reading
        // when the superblock says it was written.
        let ftype = versionnum & VERSION_MOREBITS != 0 && be32(bytes, 0xC8) & FEATURES2_FTYPE != 0;
        (ftype, false)
    };

    Ok(Superblock {
        block_size,
        inode_size,
        ag_blocks,
        ag_count,
        root_inode,
        total_blocks,
        inode_log,
        ag_block_log,
        dir_block_log,
        version,
        has_ftype,
        big_extent_counts,
    })
}

// ------------------------------------------------------------------- inodes

/// What an inode is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Directory,
    File,
    Symlink,
    /// A device, socket or fifo: it occupies an inode and no data.
    Other,
}

/// How an inode's data fork is laid out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fork {
    /// The data is inside the inode — a shortform directory, or a symlink's
    /// target.
    Local,
    /// A list of extents, inside the inode.
    Extents,
    /// A B+tree whose root is inside the inode. Only reached by a directory
    /// with far more entries than an extent list can address.
    Btree,
}

/// One inode, as much of it as measuring a disk needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inode {
    pub kind: Kind,
    pub fork: Fork,
    /// Bytes the file claims, which for a directory is the size of its entry
    /// data rather than anything a person would recognise.
    pub size: u64,
    /// Filesystem blocks really occupied. The number that matters.
    pub blocks: u64,
    /// Extents in the data fork. Sixty-four bits because that is what a
    /// filesystem with [`INCOMPAT_NREXT64`] writes, and because a count read
    /// off a disk is bounded by whoever loops over it rather than by its type.
    pub extents: u64,
    pub links: u32,
    pub mtime: i64,
    /// Where the data fork begins inside the inode.
    pub fork_at: usize,
    /// How many bytes of the inode the data fork may use.
    pub fork_bytes: usize,
}

/// Read an inode out of `bytes`, which must be one whole inode.
pub fn inode(bytes: &[u8], superblock: &Superblock) -> Option<Inode> {
    if bytes.len() < 100 || &bytes[0..2] != INODE_MAGIC {
        return None;
    }

    let mode = be16(bytes, 0x02);
    let version = bytes[0x04];
    let format = bytes[0x05];

    // Version 3 is what a v5 filesystem writes, and its core is 176 bytes
    // rather than 100. Reading a fork at the wrong offset finds plausible
    // rubbish rather than nothing, which is why this is taken from the inode
    // itself and not assumed from the superblock.
    let fork_at = if version >= 3 { 176 } else { 100 };
    // An inode too small to hold its own core is not one, whatever its magic
    // says — and the superblock's size is what every later read is cut to.
    if bytes.len() < fork_at || (superblock.inode_size as usize) < fork_at {
        return None;
    }

    let kind = match mode & 0xF000 {
        0x4000 => Kind::Directory,
        0x8000 => Kind::File,
        0xA000 => Kind::Symlink,
        _ => Kind::Other,
    };
    let fork = match format {
        1 => Fork::Local,
        2 => Fork::Extents,
        3 => Fork::Btree,
        _ => return None,
    };

    // `di_forkoff` is in eight-byte units from the start of the literal area,
    // and zero means the data fork has all of it — an inode with no extended
    // attributes, which is most of them.
    let fork_off = usize::from(bytes[0x52]);
    let available = bytes.len().saturating_sub(fork_at);
    let fork_bytes = if fork_off == 0 {
        available
    } else {
        available.min(fork_off * 8)
    };

    Some(Inode {
        kind,
        fork,
        size: be64(bytes, 0x38),
        blocks: be64(bytes, 0x40),
        extents: if superblock.big_extent_counts {
            be64(bytes, 0x18)
        } else {
            u64::from(be32(bytes, 0x4C))
        },
        links: be32(bytes, 0x10),
        // Seconds since the epoch, from the modify timestamp's first half.
        // `di_mtime` and not `di_atime` eight bytes before it, which is the
        // same shape of number and the wrong one.
        mtime: be32(bytes, 0x28) as i32 as i64,
        fork_at,
        fork_bytes,
    })
}

/// The data fork's bytes.
pub fn fork_of<'a>(bytes: &'a [u8], inode: &Inode) -> &'a [u8] {
    let end = inode
        .fork_at
        .saturating_add(inode.fork_bytes)
        .min(bytes.len());
    bytes.get(inode.fork_at..end).unwrap_or(&[])
}

// ------------------------------------------------------------------ extents

/// One extent of a fork: where it starts in the file, where on the disk, and
/// how long.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent {
    pub start_offset: u64,
    pub start_block: u64,
    pub blocks: u64,
}

/// Decode the `index`th extent record of a fork.
///
/// A record is 128 bits with five fields packed across the boundary between
/// them, which is why this is bit arithmetic rather than field reads:
///
/// ```text
/// bit 127      unwritten flag
/// bits 126..73 starting offset in the file, 54 bits
/// bits  72..21 starting block on the disk,  52 bits
/// bits  20..0  length in blocks,            21 bits
/// ```
pub fn extent(fork: &[u8], index: usize) -> Option<Extent> {
    let at = index.checked_mul(16)?;
    let record = fork.get(at..at + 16)?;
    let high = u64::from_be_bytes(record[0..8].try_into().ok()?);
    let low = u64::from_be_bytes(record[8..16].try_into().ok()?);

    Some(Extent {
        start_offset: (high & 0x7FFF_FFFF_FFFF_FFFF) >> 9,
        start_block: ((high & 0x1FF) << 43) | (low >> 21),
        blocks: low & 0x001F_FFFF,
    })
}

// ------------------------------------------------------------ the block map

/// How deep a block-map tree may go before this stops believing it.
///
/// XFS's own maximum is nine levels for the largest filesystem it supports.
/// The level comes off the disk, so the loop that descends needs a bound that
/// is not the disk's word for it.
pub const MAX_BTREE_DEPTH: usize = 16;

/// A node of the block-map B+tree, wherever it was found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BtreeNode {
    /// Zero for a leaf, which holds extents; above zero for an internal node,
    /// which holds keys and pointers to nodes one level down.
    pub level: u16,
    /// How many records, keys or pointers this node holds.
    pub count: u16,
    /// Where those begin.
    pub header: usize,
    /// How many records the node has room for, which is what the pointer array
    /// is offset by. **Not the same as `count`**: the two arrays are laid out
    /// for a full node and only partly filled, so using the count here reads
    /// the pointers from the middle of the keys.
    pub capacity: usize,
}

/// The root of the tree, which lives in the inode's fork.
///
/// A short header and then the keys and pointers, sized to whatever room the
/// fork has. There are no records here even at level zero — a tree only exists
/// because the extents did not fit.
pub fn btree_root(fork: &[u8]) -> Option<BtreeNode> {
    const HEADER: usize = 4;
    if fork.len() < 4 {
        return None;
    }
    let level = be16(fork, 0);
    let count = be16(fork, 2);
    if level == 0 || level as usize > MAX_BTREE_DEPTH {
        return None;
    }
    // A key and a pointer are eight bytes each.
    let capacity = fork.len().checked_sub(HEADER)? / 16;
    if usize::from(count) > capacity {
        return None;
    }
    Some(BtreeNode {
        level,
        count,
        header: HEADER,
        capacity,
    })
}

/// A node read off the disk, rather than out of the inode.
///
/// Version 5 puts a 56-byte checksummed header in front of the 16-byte one,
/// which moves every key, pointer and record with it.
pub fn btree_node(block: &[u8], block_size: usize) -> Option<BtreeNode> {
    let header = match block.get(0..4)? {
        b"BMA3" => 72,
        b"BMAP" => 24,
        _ => return None,
    };
    let level = be16(block, 4);
    let count = be16(block, 6);
    if level as usize > MAX_BTREE_DEPTH {
        return None;
    }
    let room = block_size.checked_sub(header)?;
    // A leaf holds 16-byte extent records; an internal node holds an 8-byte
    // key and an 8-byte pointer per slot, which is the same 16 bytes and the
    // reason one number serves both.
    let capacity = room / 16;
    if usize::from(count) > capacity {
        return None;
    }
    Some(BtreeNode {
        level,
        count,
        header,
        capacity,
    })
}

/// The `index`th pointer of an internal node: the block one level down.
///
/// The pointers sit after the *whole* key array, which is sized for a full
/// node. Using the live count instead lands in the middle of the keys and
/// descends to a block number that is really half of a file offset.
pub fn btree_pointer(bytes: &[u8], node: &BtreeNode, index: usize) -> Option<u64> {
    if index >= usize::from(node.count) {
        return None;
    }
    let at = node
        .header
        .checked_add(node.capacity.checked_mul(8)?)?
        .checked_add(index.checked_mul(8)?)?;
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

// -------------------------------------------------------------- directories

/// One entry of a directory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry<'a> {
    pub name: &'a [u8],
    pub inode: u64,
}

/// Read a shortform directory — the entries are inside the inode.
///
/// The layout is a count, a parent, and then entries whose inode numbers are
/// four bytes or eight *for the whole directory* rather than per entry: the
/// kernel rewrites the lot the first time one of them needs the extra room.
/// Reading that per entry instead produces a directory of nonsense, which is
/// the kind of mistake that still parses.
pub fn shortform(fork: &[u8], has_ftype: bool, mut each: impl FnMut(Entry<'_>)) -> Option<()> {
    let count = usize::from(*fork.first()?);
    let big = *fork.get(1)? != 0;
    let inode_bytes = if big { 8 } else { 4 };

    // The header is the two counts and the parent's inode number.
    let mut at = 2 + inode_bytes;
    for _ in 0..count {
        let name_len = usize::from(*fork.get(at)?);
        // One byte of length, two of offset.
        let name_at = at + 3;
        let name = fork.get(name_at..name_at + name_len)?;
        let after = name_at + name_len + usize::from(has_ftype);
        let inode = read_inode_number(fork.get(after..after + inode_bytes)?, big)?;
        each(Entry { name, inode });
        at = after + inode_bytes;
    }
    Some(())
}

fn read_inode_number(bytes: &[u8], big: bool) -> Option<u64> {
    if big {
        Some(u64::from_be_bytes(bytes.get(0..8)?.try_into().ok()?) & 0x00FF_FFFF_FFFF_FFFF)
    } else {
        Some(u64::from(u32::from_be_bytes(
            bytes.get(0..4)?.try_into().ok()?,
        )))
    }
}

/// The magic numbers a directory block can begin with, and what each means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    /// A whole directory in one block: entries, then a hash index at the tail.
    Single,
    /// One of several blocks of entries.
    Data,
    /// A hash index, a free-space map, or something else with no entries in
    /// it. Skipped rather than parsed.
    Other,
}

/// What kind of directory block this is, and where its entries start.
pub fn block_kind(block: &[u8]) -> Option<(Block, usize)> {
    let magic = block.get(0..4)?;
    Some(match magic {
        // Version 5: a 48-byte CRC header in front of the 16-byte one.
        b"XDB3" => (Block::Single, 64),
        b"XDD3" => (Block::Data, 64),
        // Version 4.
        b"XD2B" => (Block::Single, 16),
        b"XD2D" => (Block::Data, 16),
        _ => (Block::Other, 0),
    })
}

/// Where the entries of a single-block directory stop.
///
/// The tail of such a block holds the hash index, and the count of it is the
/// last four bytes but one. Parsing into that index finds entries whose names
/// are hashes, which is a directory full of rubbish rather than an error.
pub fn single_block_end(block: &[u8]) -> Option<usize> {
    let len = block.len();
    let count = be32(block, len.checked_sub(8)?) as usize;
    // Eight bytes of tail, and eight per index entry.
    let index = count.checked_mul(8)?.checked_add(8)?;
    len.checked_sub(index)
}

/// Walk the entries of a directory data block.
///
/// Used entries and free space are interleaved, and free space announces
/// itself with a length rather than being inferred — which is what lets this
/// be a forward scan with no lookahead.
pub fn data_entries(
    block: &[u8],
    from: usize,
    to: usize,
    has_ftype: bool,
    mut each: impl FnMut(Entry<'_>),
) -> Option<()> {
    /// A "used entry" whose first two bytes are this is really free space.
    const FREE: u16 = 0xFFFF;

    let to = to.min(block.len());
    let mut at = from;
    while at + 3 <= to {
        if be16(block, at) == FREE {
            let length = usize::from(be16(block, at + 2));
            // A zero-length hole would never end, and a negative one does not
            // exist: either is a block that cannot be walked.
            if length < 8 {
                return None;
            }
            at = at.checked_add(length)?;
            continue;
        }

        // inumber(8) + namelen(1) + name + ftype? + tag(2), rounded up to 8.
        let name_len = usize::from(*block.get(at + 8)?);
        if name_len == 0 {
            return None;
        }
        let name_at = at + 9;
        let name = block.get(name_at..name_at + name_len)?;
        let inode = u64::from_be_bytes(block.get(at..at + 8)?.try_into().ok()?);
        each(Entry { name, inode });

        let used = 9 + name_len + usize::from(has_ftype) + 2;
        at = at.checked_add(used.next_multiple_of(8))?;
    }
    Some(())
}

// ------------------------------------------------------------------ scalars

fn be16(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at + 2)
        .and_then(|b| b.try_into().ok())
        .map(u16::from_be_bytes)
        .unwrap_or(0)
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_be_bytes)
        .unwrap_or(0)
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_be_bytes)
        .unwrap_or(0)
}
