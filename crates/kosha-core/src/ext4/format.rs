// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! ext4 on-disk structures, as bytes.
//!
//! No I/O anywhere in this file: every function takes a slice and returns
//! numbers, so the whole format is under test on whichever machine it is being
//! edited on. The same arrangement as the `ntfs` parsers, and for the same
//! reason — a byte layout decided by reading a specification should be
//! checkable without the hardware it describes.
//!
//! What is here is only what a *metadata* scan needs. There is no block
//! allocator, no journal replay, no checksum verification beyond the
//! superblock's own magic, and nothing that reads the contents of a regular
//! file. The one kind of data block this ever follows is a directory's.

/// `s_magic`. The only thing that says this is an ext filesystem at all.
const MAGIC: u16 = 0xEF53;

/// The superblock always starts 1024 bytes into the volume, whatever the block
/// size is — which is why it survives a boot sector sitting in front of it.
pub const SUPERBLOCK_OFFSET: u64 = 1024;

/// The root of every ext filesystem. Fixed by the format, like NTFS' record 5.
pub const ROOT_INODE: u32 = 2;

// Features that change what this file has to understand. Everything else is
// either irrelevant to reading metadata or affects only writers.
const INCOMPAT_FILETYPE: u32 = 0x0002;
const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
const INCOMPAT_META_BG: u32 = 0x0010;
const INCOMPAT_64BIT: u32 = 0x0080;
const INCOMPAT_ENCRYPT: u32 = 0x1_0000;

const RO_COMPAT_HUGE_FILE: u32 = 0x0008;

/// `i_flags`: this inode's data is mapped by an extent tree rather than by the
/// older chain of indirect blocks.
pub const INODE_EXTENTS: u32 = 0x0008_0000;
/// `i_flags`: the data is inside the inode itself.
pub const INODE_INLINE_DATA: u32 = 0x1000_0000;
/// `i_flags`: `i_blocks` counts filesystem blocks rather than 512-byte sectors.
const INODE_HUGE_FILE: u32 = 0x0004_0000;

/// `eh_magic`, at the front of every extent header.
const EXTENT_MAGIC: u16 = 0xF30A;

/// What an inode is, from the top four bits of its mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Directory,
    File,
    Symlink,
    /// A device node, a socket, a fifo. Real entries that occupy no data.
    Other,
}

/// The geometry of a filesystem, and the handful of features that change how it
/// is read.
#[derive(Clone, Copy, Debug)]
pub struct Superblock {
    pub block_size: u64,
    pub blocks_count: u64,
    pub inodes_count: u32,
    pub inodes_per_group: u32,
    pub blocks_per_group: u32,
    pub first_data_block: u32,
    pub inode_size: u32,
    /// Bytes per group descriptor: 32, or 64 on a 64-bit filesystem.
    pub descriptor_size: u32,
    /// Directory entries carry the kind of thing they name, so a walk can tell
    /// a directory from a file without reading its inode. Set on every
    /// filesystem made this century, and worth an enormous amount here: it is
    /// what lets the whole tree be discovered before a single inode is read.
    pub filetype_in_dirents: bool,
    /// `i_blocks` may be counted in filesystem blocks on inodes that say so.
    pub huge_files: bool,
}

impl Superblock {
    /// How many block groups the filesystem has.
    pub fn group_count(&self) -> u64 {
        let data = self
            .blocks_count
            .saturating_sub(u64::from(self.first_data_block));
        data.div_ceil(u64::from(self.blocks_per_group))
    }

    /// The block holding the start of the group descriptor table.
    ///
    /// Always the one after the superblock's own block, which is block 1 on a
    /// filesystem with 1 KiB blocks and block 0 everywhere else — because with
    /// larger blocks the superblock shares block 0 with the padding in front of
    /// it. `first_data_block` is the format's way of saying which.
    pub fn descriptor_table_block(&self) -> u64 {
        u64::from(self.first_data_block) + 1
    }

    /// Where inode `number` lives, as an absolute byte offset.
    ///
    /// Needs the group's inode table, which the caller reads from the
    /// descriptor table — this only does the arithmetic inside a group.
    pub fn inode_offset(&self, table_block: u64, number: u32) -> Option<u64> {
        // Inodes are numbered from one, and inode zero does not exist.
        let index = number.checked_sub(1)?;
        let within = u64::from(index % self.inodes_per_group);
        table_block
            .checked_mul(self.block_size)?
            .checked_add(within.checked_mul(u64::from(self.inode_size))?)
    }

    /// Which block group an inode belongs to.
    pub fn group_of(&self, number: u32) -> Option<u64> {
        Some(u64::from(number.checked_sub(1)? / self.inodes_per_group))
    }
}

/// Why a filesystem cannot be read.
///
/// Named rather than collapsed into one error, because "kosha cannot read this
/// image" is not something anybody can act on and "this image is encrypted" is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unreadable {
    #[error("this is not an ext2, ext3 or ext4 filesystem")]
    NotExt,
    #[error("the filesystem's geometry does not make sense")]
    Impossible,
    #[error("this image is an external journal rather than a filesystem")]
    JournalDevice,
    #[error("this filesystem lays its group descriptors out in a way kosha does not read")]
    MetaBlockGroups,
    #[error("this filesystem's names are encrypted")]
    Encrypted,
}

/// Read a superblock out of the 1024 bytes at [`SUPERBLOCK_OFFSET`].
pub fn superblock(bytes: &[u8]) -> Result<Superblock, Unreadable> {
    if bytes.len() < 0x400 || u16_at(bytes, 0x38) != MAGIC {
        return Err(Unreadable::NotExt);
    }

    let incompat = u32_at(bytes, 0x60);
    if incompat & INCOMPAT_JOURNAL_DEV != 0 {
        return Err(Unreadable::JournalDevice);
    }
    if incompat & INCOMPAT_META_BG != 0 {
        // The descriptor table is scattered across the filesystem rather than
        // sitting after the superblock. Nothing WSL makes uses it, and reading
        // the table from the wrong place would produce a tree of nonsense
        // rather than an error.
        return Err(Unreadable::MetaBlockGroups);
    }
    if incompat & INCOMPAT_ENCRYPT != 0 {
        // Names are ciphertext. A tree of them would be worse than none.
        return Err(Unreadable::Encrypted);
    }

    let log_block_size = u32_at(bytes, 0x18);
    if log_block_size > 6 {
        return Err(Unreadable::Impossible);
    }
    let block_size = 1024u64 << log_block_size;

    // Revision 0 predates the field and fixes the size at 128 bytes.
    let revision = u32_at(bytes, 0x4C);
    let inode_size = if revision == 0 {
        128
    } else {
        u32::from(u16_at(bytes, 0x58))
    };

    let sixty_four_bit = incompat & INCOMPAT_64BIT != 0;
    let descriptor_size = if sixty_four_bit {
        u32::from(u16_at(bytes, 0xFE))
    } else {
        32
    };

    // The high half of the block count only exists on a 64-bit filesystem;
    // elsewhere those bytes are something else entirely.
    let mut blocks_count = u64::from(u32_at(bytes, 0x04));
    if sixty_four_bit {
        blocks_count |= u64::from(u32_at(bytes, 0x150)) << 32;
    }

    let superblock = Superblock {
        block_size,
        blocks_count,
        inodes_count: u32_at(bytes, 0x00),
        inodes_per_group: u32_at(bytes, 0x28),
        blocks_per_group: u32_at(bytes, 0x20),
        first_data_block: u32_at(bytes, 0x14),
        inode_size,
        descriptor_size,
        filetype_in_dirents: incompat & INCOMPAT_FILETYPE != 0,
        huge_files: u32_at(bytes, 0x64) & RO_COMPAT_HUGE_FILE != 0,
    };

    // Every one of these divides or indexes something later. A zero here is a
    // panic or an infinite loop a few hundred lines away.
    let sane = superblock.inodes_per_group > 0
        // A group's inode bitmap is exactly one block, so a group cannot hold
        // more inodes than that block has bits. This is the format's own bound
        // and not a guess, and without it a single flipped bit in
        // `inodes_per_group` asked for a 68 GB inode table — which is not an
        // error anybody can catch, because a failed allocation aborts the
        // process. Found by flipping every bit of every structure in turn.
        && u64::from(superblock.inodes_per_group) <= block_size * 8
        && superblock.blocks_per_group > 0
        // The same bound, for the same reason: the block bitmap is one block
        // too.
        && u64::from(superblock.blocks_per_group) <= block_size * 8
        && superblock.blocks_count > 0
        && superblock.inodes_count > 0
        && inode_size >= 128
        && inode_size.is_power_of_two()
        && u64::from(inode_size) <= block_size
        && (descriptor_size == 32 || (descriptor_size >= 64 && descriptor_size.is_power_of_two()))
        && u64::from(superblock.first_data_block) < superblock.blocks_count;
    if !sane {
        return Err(Unreadable::Impossible);
    }
    Ok(superblock)
}

/// The inode table block of one group, from its descriptor.
pub fn inode_table(descriptor: &[u8], descriptor_size: u32) -> Option<u64> {
    if descriptor.len() < 32 {
        return None;
    }
    let low = u64::from(u32_at(descriptor, 0x08));
    // The high half only exists on a 64-bit filesystem, where the descriptor
    // is twice as long. Reading it from a 32-byte descriptor would pick up the
    // *next group's* block bitmap.
    let high = if descriptor_size >= 64 && descriptor.len() >= 0x2C {
        u64::from(u32_at(descriptor, 0x28))
    } else {
        0
    };
    Some(low | (high << 32))
}

/// How many inodes a group has free, from its descriptor.
///
/// The high half exists only on a 64-bit filesystem, where the descriptor is
/// twice as long — and reading it from a 32-byte descriptor picks up the next
/// group's fields.
pub fn free_inodes(descriptor: &[u8]) -> u32 {
    let low = u32::from(u16_at(descriptor, 0x0E));
    let high = if descriptor.len() >= 0x30 {
        u32::from(u16_at(descriptor, 0x2E))
    } else {
        0
    };
    low | (high << 16)
}

/// What one inode says about the thing it describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inode {
    pub mode: u16,
    pub uid: u32,
    /// Apparent size in bytes.
    pub size: u64,
    /// Bytes actually occupied, from `i_blocks`. Exact, the way `st_blocks` is
    /// exact — sparse and transparently compressed files included.
    pub allocated: u64,
    /// Unix seconds, saturating at the epoch and at 2106 to match `Node`.
    pub mtime: u32,
    pub links: u16,
    pub flags: u32,
    /// `i_block`, the sixty bytes that are either an extent tree root or a list
    /// of block pointers. Carried raw because which one it is depends on
    /// [`flags`](Self::flags).
    pub block_map: [u8; 60],
}

/// Arrays derive `Default` only up to thirty-two elements, and `i_block` is
/// sixty bytes. Written out rather than shortening the field.
impl Default for Inode {
    fn default() -> Inode {
        Inode {
            mode: 0,
            uid: 0,
            size: 0,
            allocated: 0,
            mtime: 0,
            links: 0,
            flags: 0,
            block_map: [0u8; 60],
        }
    }
}

impl Inode {
    pub fn kind(&self) -> Kind {
        match self.mode & 0xF000 {
            0x4000 => Kind::Directory,
            0x8000 => Kind::File,
            0xA000 => Kind::Symlink,
            _ => Kind::Other,
        }
    }

    /// Whether an inode slot is in use.
    ///
    /// A freed inode keeps its bytes until the slot is reused, so a table is
    /// full of plausible-looking rubbish. Mode zero and no links is what a
    /// freed one looks like, and the directory entry that pointed at it is
    /// gone, so nothing should reach one anyway.
    pub fn in_use(&self) -> bool {
        self.mode != 0 && self.links > 0
    }
}

/// Read an inode from the bytes at its place in the table.
pub fn inode(bytes: &[u8], superblock: &Superblock) -> Option<Inode> {
    if bytes.len() < 128 {
        return None;
    }
    let flags = u32_at(bytes, 0x20);

    // `i_blocks` is in 512-byte units — *not* filesystem blocks, which is the
    // trap in the field's name. The exception is an inode that says otherwise,
    // on a filesystem whose features allow it to.
    let raw_blocks = u64::from(u32_at(bytes, 0x1C)) | (u64::from(u16_at(bytes, 0x74)) << 32);
    let allocated = if superblock.huge_files && flags & INODE_HUGE_FILE != 0 {
        raw_blocks.saturating_mul(superblock.block_size)
    } else {
        raw_blocks.saturating_mul(512)
    };

    let mut block_map = [0u8; 60];
    block_map.copy_from_slice(bytes.get(0x28..0x28 + 60)?);

    Some(Inode {
        mode: u16_at(bytes, 0x00),
        uid: u32::from(u16_at(bytes, 0x02)) | (u32::from(u16_at(bytes, 0x78)) << 16),
        size: u64::from(u32_at(bytes, 0x04)) | (u64::from(u32_at(bytes, 0x6C)) << 32),
        allocated,
        mtime: u32_at(bytes, 0x10),
        links: u16_at(bytes, 0x1A),
        flags,
        block_map,
    })
}

// ------------------------------------------------------------------ extents

/// The header at the front of every node of an extent tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExtentHeader {
    pub entries: u16,
    pub depth: u16,
}

/// Read an extent node's header, or `None` if there is not one here.
pub fn extent_header(bytes: &[u8]) -> Option<ExtentHeader> {
    if bytes.len() < 12 || u16_at(bytes, 0) != EXTENT_MAGIC {
        return None;
    }
    let entries = u16_at(bytes, 2);
    let max = u16_at(bytes, 4);
    let depth = u16_at(bytes, 6);
    // The tree is at most five deep by construction, and `entries` has to fit
    // in what follows the header.
    if depth > 5 || entries > max || usize::from(entries) * 12 + 12 > bytes.len() {
        return None;
    }
    Some(ExtentHeader { entries, depth })
}

/// A run of contiguous blocks, at a leaf of the tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent {
    /// First block of the file this run covers.
    pub logical: u32,
    /// How many blocks. Never more than 32768.
    pub length: u16,
    /// Where the run starts on the filesystem.
    pub start: u64,
}

/// The `index`th leaf entry of an extent node.
pub fn extent(bytes: &[u8], index: usize) -> Option<Extent> {
    let at = 12 + index * 12;
    let raw = bytes.get(at..at + 12)?;
    let length = u16_at(raw, 4);
    Some(Extent {
        logical: u32_at(raw, 0),
        // The top bit marks a run that is allocated but not yet written. Its
        // blocks are real and occupied, so for measuring they count exactly the
        // same; only the length has to come back into range.
        length: if length > 32768 {
            length - 32768
        } else {
            length
        },
        start: u64::from(u32_at(raw, 8)) | (u64::from(u16_at(raw, 6)) << 32),
    })
}

/// The block holding the child node of the `index`th interior entry.
pub fn extent_child(bytes: &[u8], index: usize) -> Option<u64> {
    let at = 12 + index * 12;
    let raw = bytes.get(at..at + 12)?;
    Some(u64::from(u32_at(raw, 4)) | (u64::from(u16_at(raw, 8)) << 32))
}

// --------------------------------------------------------- directory blocks

/// `ext4_dir_entry_2`'s file types, for the two that matter.
const FILE_TYPE_DIRECTORY: u8 = 2;
const FILE_TYPE_SYMLINK: u8 = 7;

/// One name in a directory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DirEntry<'a> {
    pub inode: u32,
    /// The kind the *directory* says it is, which is there on every filesystem
    /// with `filetype` set and saves reading the inode to find out.
    pub file_type: u8,
    /// Raw bytes. A Linux filename is bytes and need not be UTF-8.
    pub name: &'a [u8],
}

impl DirEntry<'_> {
    pub fn is_directory(&self) -> bool {
        self.file_type == FILE_TYPE_DIRECTORY
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type == FILE_TYPE_SYMLINK
    }

    /// `.` and `..`, which are the same directory and its parent and must never
    /// become children of anything.
    pub fn is_dot(&self) -> bool {
        self.name == b"." || self.name == b".."
    }
}

/// Walk the names in one directory block.
///
/// Stops rather than failing on anything malformed: a length that would leave
/// the block, run backwards, or break the four-byte alignment the format
/// requires. What was read before the bad record is still true, and the
/// alternative to stopping is a loop that never ends.
///
/// **Indexed directories need nothing special.** A large directory grows a hash
/// tree, and its interior nodes are stored as blocks that look like one empty
/// record covering the whole block — so walking every block linearly and
/// skipping the empty records visits each name exactly once and never visits an
/// index node's contents. The checksum record at the tail of a block, on a
/// filesystem with `metadata_csum`, is skipped by the same rule.
pub fn dir_entries(block: &[u8]) -> impl Iterator<Item = DirEntry<'_>> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        loop {
            let record = block.get(at..at + 8)?;
            let length = usize::from(u16_at(record, 4));
            if length < 8 || !length.is_multiple_of(4) || at + length > block.len() {
                return None;
            }

            let inode = u32_at(record, 0);
            let name_length = usize::from(record[6]);
            let start = at;
            at += length;

            // Inode zero is a hole: an erased entry, the padding a hash-tree
            // node is made of, or the checksum at the end of a block.
            if inode == 0 {
                continue;
            }
            let name = block.get(start + 8..start + 8 + name_length)?;
            if name.is_empty() {
                continue;
            }
            return Some(DirEntry {
                inode,
                file_type: record[7],
                name,
            });
        }
    })
}

// ------------------------------------------------------------------ scalars

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    match bytes.get(at..at + 2) {
        Some(two) => u16::from_le_bytes([two[0], two[1]]),
        None => 0,
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    match bytes.get(at..at + 4) {
        Some(four) => u32::from_le_bytes([four[0], four[1], four[2], four[3]]),
        None => 0,
    }
}
