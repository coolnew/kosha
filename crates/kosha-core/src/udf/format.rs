// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! UDF on-disc structures, as bytes in and numbers out.
//!
//! **Every structure here carries a checksum and a CRC, and that is the one
//! way UDF is easier than what it replaced.** ISO 9660 has nothing to check a
//! number against, so its reader validates by making the numbers agree with
//! each other. Here a descriptor checks out or it does not, and a reader that
//! checks can search a volume for a structure instead of being told where it
//! is — which is what finding the anchor is.
//!
//! ## Two standards, and which one each name comes from
//!
//! **ECMA-167** defines the descriptors: the tag, the volume sequence, the
//! file set, the file entry, the allocation descriptor. **UDF** is a profile of
//! it — which parts a disc may use, plus the encoding for names and a few
//! identifiers of its own. A reader needs both and the split shows: a *logical
//! volume* is ECMA-167 and *OSTA Compressed Unicode* is UDF.
//!
//! ## How a volume is found
//!
//! ```text
//! block 16..     BEA01, NSR02 or NSR03, TEA01   the volume recognition sequence
//! block 256      anchor                         where the descriptors are
//!   descriptors  a partition, a logical volume  where the blocks are
//!   file set     the root's ICB                 where the tree starts
//!   file entry   a length, and its extents      one per file
//! ```
//!
//! The anchor is at logical block 256 of the medium, and at its last block, and
//! usually at 256 back from the last as well. Three copies, because this was
//! written for media that fail at the edges.
//!
//! **A logical block is not a byte offset.** ECMA-167 separates the medium's
//! blocks from a *partition's*, and everything below the file set counts in
//! partition blocks — so the root "at block 3" is at block 3 of a partition
//! starting at 257, and a reader that forgets reads the wrong 2048 bytes. The
//! partition descriptor is where those two numbers meet.
//!
//! ## What is not here
//!
//! Nothing that writes, and nothing that reads: this file is handed slices.
//! Following an extent, an anchor or an ICB costs a read and that belongs to
//! [`reader`](super::reader).

use crate::calendar;

/// Block sizes a volume may have, in the order they are tried.
///
/// 2048 first because it is what an optical disc is and so what every disc
/// image has. The others exist because UDF is also what a rewritable stick gets
/// formatted as, where the block size is the device's sector size.
pub const BLOCK_SIZES: [u32; 4] = [2048, 512, 1024, 4096];

/// Where the first anchor is, in logical blocks.
pub const ANCHOR_BLOCK: u64 = 256;

/// Where the volume recognition sequence starts, in bytes.
///
/// Its blocks are 2048 whatever the volume's own block size is: the sequence
/// belongs to the medium rather than to the volume, the same reason ISO 9660's
/// descriptors are at 32768 on a disc of any size.
pub const RECOGNITION_AT: u64 = 16 * 2048;

/// How many blocks of the recognition sequence to look at.
///
/// A pure UDF volume puts `NSR0x` in the second. A disc carrying ISO 9660 as
/// well puts its own three descriptors first and pushes this down, so on an
/// ordinary bridge disc the sequence really starts at block 19.
pub const RECOGNITION_BLOCKS: u64 = 8;

/// A descriptor tag, which every structure below the recognition sequence
/// begins with.
pub const TAG_BYTES: usize = 16;

pub const TAG_ANCHOR: u16 = 2;
pub const TAG_PARTITION: u16 = 5;
pub const TAG_LOGICAL_VOLUME: u16 = 6;
pub const TAG_TERMINATING: u16 = 8;
pub const TAG_FILE_SET: u16 = 256;
pub const TAG_FILE_ID: u16 = 257;
pub const TAG_ALLOCATION_EXTENT: u16 = 258;
pub const TAG_INDIRECT: u16 = 259;
pub const TAG_FILE_ENTRY: u16 = 261;
pub const TAG_EXTENDED_FILE_ENTRY: u16 = 266;

/// An anchor and a file set are both 512 bytes, whatever the block size.
pub const ANCHOR_BYTES: usize = 512;
/// The fixed part of a logical volume descriptor, before its partition maps.
pub const LOGICAL_VOLUME_FIXED: usize = 440;
/// A `long_ad`, which is how one structure points at another.
pub const LONG_AD: usize = 16;
/// A `short_ad`, which is how a file inside one partition points at its own
/// blocks.
pub const SHORT_AD: usize = 8;
/// An `ext_ad`, which adds a recorded length to a `long_ad`.
pub const EXT_AD: usize = 20;
/// A file identifier descriptor, before its name and its padding.
pub const FILE_ID_FIXED: usize = 38;

/// `FileType` in an ICB tag: the ones that mean something here.
pub const TYPE_DIRECTORY: u8 = 4;
pub const TYPE_SYMLINK: u8 = 12;
/// A virtual allocation table, on a volume written to UDF 2.00 or later. A
/// 1.50 one has no type of its own and is known by what is at the end of it.
pub const TYPE_VAT: u8 = 248;

/// What a UDF 1.50 virtual allocation table keeps after its entries: a
/// registered identifier saying what it is, and where the previous one was.
const VAT_TAIL: usize = 36;

/// `FileCharacteristics` in a file identifier descriptor.
///
/// Bit 0 is *hidden*, which is read by nothing here: FAT has the same bit and
/// the FAT reader ignores it too, because a scan that left a hidden file out of
/// a total would be answering a question nobody asked with a number that does
/// not add up.
const DIRECTORY: u8 = 0x02;
const DELETED: u8 = 0x04;
const PARENT: u8 = 0x08;

/// Why a UDF filesystem could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unreadable {
    #[error("this is not a UDF filesystem")]
    NotUdf,
    #[error("the volume's geometry does not make sense")]
    Impossible,
    /// Every descriptor was read and none was a logical volume with a partition
    /// this reader can address. Separate from
    /// [`Impossible`](Unreadable::Impossible) because the volume is not damaged
    /// — it uses an indirection kosha has no rule for, which is a different
    /// thing to be told.
    #[error("the volume's blocks are somewhere kosha cannot follow")]
    Unmapped,
}

// ------------------------------------------------------------------ numbers

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(eight)
}

// --------------------------------------------------------------------- CRC

/// The CRC every descriptor carries, and the reason a reader can trust one.
///
/// ECMA-167 calls for the CRC-ITU-T polynomial — x^16 + x^12 + x^5 + 1, which
/// is 0x1021 — starting from zero, most significant bit first, with nothing
/// done to the result. The same polynomial as in a floppy disk's sector headers
/// and in XMODEM, and a table of 256 entries is the ordinary way to compute it
/// a byte at a time.
const CRC_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut value = (index as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000 != 0 {
                (value << 1) ^ 0x1021
            } else {
                value << 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in bytes {
        crc = CRC_TABLE[usize::from(((crc >> 8) as u8) ^ byte)] ^ (crc << 8);
    }
    crc
}

// --------------------------------------------------------------------- tags

/// A descriptor tag: what this is, and where it says it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tag {
    pub identifier: u16,
    /// 2 for a volume written to UDF 1.x and 3 for 2.x, which is the only
    /// place the revision shows in a structure a reader has to parse.
    pub version: u16,
    /// Where the structure says it is, in logical blocks — of the partition for
    /// anything below the file set, and of the medium above it.
    pub location: u32,
    /// How many bytes after the tag the CRC covers.
    pub crc_length: u16,
}

/// Read and check the tag at the front of `bytes`.
///
/// **Three checks, and `at` makes it four.** The checksum catches a tag read
/// from the wrong place; the CRC catches damage anywhere in the descriptor; the
/// identifier catches the right structure in the wrong slot. And `location`
/// catches the mistake a reader actually makes, which is computing an offset in
/// the wrong units — a descriptor found at the wrong block says so, because it
/// carries its own address.
///
/// That last check is what makes *searching* for the anchor safe. There is no
/// plausible way for 512 bytes of something else to be tag 2, sum to its own
/// checksum, match a CRC over 496 bytes and claim to be at block 256.
///
/// `at` is the block the caller believes this is, or `None` where the caller
/// has no opinion — reading the descriptors of a volume sequence, which the
/// extent locates and which are not obliged to be in any particular order.
pub fn tag(bytes: &[u8], at: Option<u32>) -> Option<Tag> {
    if bytes.len() < TAG_BYTES {
        return None;
    }
    // The checksum is the sum of the tag's own sixteen bytes with the checksum
    // byte itself left out, modulo 256.
    let sum = bytes[..TAG_BYTES]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
    if sum != bytes[4] {
        return None;
    }
    let version = le16(bytes, 2);
    // Version 1 is ECMA-167 first edition, which no UDF revision uses and
    // nothing has written this century. 2 is UDF 1.x and 3 is UDF 2.x.
    if version != 2 && version != 3 {
        return None;
    }
    // The CRC comes before the length it covers, which is the one field order
    // in this file that reads backwards — and swapping the two is a mistake that
    // refuses every descriptor on every volume rather than accepting a wrong
    // one, because a CRC read as a length is tens of kilobytes and runs off the
    // end of the block.
    let crc_length = le16(bytes, 10);
    let covered = bytes.get(TAG_BYTES..TAG_BYTES + usize::from(crc_length))?;
    if crc16(covered) != le16(bytes, 8) {
        return None;
    }
    let location = le32(bytes, 12);
    if let Some(block) = at
        && block != location
    {
        return None;
    }
    Some(Tag {
        identifier: le16(bytes, 0),
        version,
        location,
        crc_length,
    })
}

/// The same, where the caller wants one particular kind of descriptor.
pub fn tagged(bytes: &[u8], identifier: u16, at: Option<u32>) -> Option<Tag> {
    tag(bytes, at).filter(|found| found.identifier == identifier)
}

// ------------------------------------------------------------------ extents

/// An `extent_ad`: a length in bytes and a block, and no partition — the volume
/// sequence is on the medium rather than in a partition.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Extent {
    pub bytes: u32,
    pub block: u32,
}

fn extent(bytes: &[u8], at: usize) -> Extent {
    Extent {
        bytes: le32(bytes, at),
        block: le32(bytes, at + 4),
    }
}

/// What the top two bits of a length say about the blocks under it.
///
/// They are not part of the length. A file with a hole has an extent that is
/// allocated and never written, and one being written to has an extent that is
/// neither — both are length without bytes, which is where a sparse file's
/// numbers come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recorded {
    /// Recorded and allocated: there are bytes there.
    Yes,
    /// Allocated and not recorded, or not even allocated. A hole.
    No,
    /// Not data at all: the extent is where the *next* allocation descriptors
    /// are, which is how a file with more extents than fit in its entry carries
    /// on.
    Continued,
}

impl Recorded {
    fn of(kind: u32) -> Recorded {
        match kind {
            0 => Recorded::Yes,
            1 | 2 => Recorded::No,
            _ => Recorded::Continued,
        }
    }
}

/// One allocation descriptor, whichever of the three forms it was written in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Allocation {
    pub bytes: u32,
    pub block: u32,
    /// Which partition, for a `long_ad`. A `short_ad` has no room for one and
    /// means the partition its file entry is in.
    pub partition: u16,
    pub recorded: Recorded,
}

/// A `long_ad`, which is how the structures above a file point at one.
pub fn long(bytes: &[u8], at: usize) -> Option<Allocation> {
    let bytes = bytes.get(at..at + LONG_AD)?;
    let length = le32(bytes, 0);
    Some(Allocation {
        bytes: length & 0x3FFF_FFFF,
        recorded: Recorded::of(length >> 30),
        block: le32(bytes, 4),
        partition: le16(bytes, 8),
    })
}

/// A `short_ad`, in the partition the caller is already in.
pub fn short(bytes: &[u8], at: usize, partition: u16) -> Option<Allocation> {
    let bytes = bytes.get(at..at + SHORT_AD)?;
    let length = le32(bytes, 0);
    Some(Allocation {
        bytes: length & 0x3FFF_FFFF,
        recorded: Recorded::of(length >> 30),
        block: le32(bytes, 4),
        partition,
    })
}

/// An `ext_ad`, which records how much of the extent was written as well as how
/// much is there.
///
/// Only the first length is read. What a scan reports as size on disk is the
/// file entry's own `LogicalBlocksRecorded`, and a second opinion taken from
/// here would be a place for the two to disagree.
pub fn extended(bytes: &[u8], at: usize) -> Option<Allocation> {
    let bytes = bytes.get(at..at + EXT_AD)?;
    let length = le32(bytes, 0);
    Some(Allocation {
        bytes: length & 0x3FFF_FFFF,
        recorded: Recorded::of(length >> 30),
        block: le32(bytes, 12),
        partition: le16(bytes, 16),
    })
}

// ------------------------------------------------------------------- anchor

/// The anchor: where the volume descriptors are, twice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Anchor {
    pub main: Extent,
    pub reserve: Extent,
}

/// Read an anchor the caller believes is at block `at`.
pub fn anchor(bytes: &[u8], at: u32) -> Option<Anchor> {
    tagged(bytes, TAG_ANCHOR, Some(at))?;
    let main = extent(bytes, 16);
    let reserve = extent(bytes, 24);
    // An anchor with no main sequence is not one. The reserve may be empty, on
    // a volume written by something that saw no point in a spare copy.
    if main.bytes == 0 {
        return None;
    }
    Some(Anchor { main, reserve })
}

// ---------------------------------------------------------------- partition

/// A partition descriptor: where a partition's blocks are on the medium.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Partition {
    pub number: u16,
    pub start: u32,
    pub blocks: u32,
    /// The volume descriptor sequence number, which is how a later description
    /// of the same partition supersedes an earlier one.
    pub sequence: u32,
}

pub fn partition(bytes: &[u8]) -> Option<Partition> {
    tagged(bytes, TAG_PARTITION, None)?;
    let found = Partition {
        sequence: le32(bytes, 16),
        number: le16(bytes, 22),
        start: le32(bytes, 188),
        blocks: le32(bytes, 192),
    };
    if found.blocks == 0 {
        return None;
    }
    Some(found)
}

// ----------------------------------------------------------- logical volume

/// A logical volume descriptor, without its partition maps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LogicalVolume {
    pub sequence: u32,
    pub block_size: u32,
    /// Which UDF revision the volume was written to, as the two bytes in the
    /// domain identifier's suffix: 0x0102 for 1.02, 0x0250 for 2.50. Binary
    /// coded decimal, so each nibble is a digit and printing it in hexadecimal
    /// is what spells it — see [`revision`].
    pub revision: u16,
    /// Where the file set is, which is where the tree starts.
    pub file_set: Allocation,
    pub maps: u32,
    pub map_table_bytes: u32,
}

pub fn logical_volume(bytes: &[u8]) -> Option<LogicalVolume> {
    tagged(bytes, TAG_LOGICAL_VOLUME, None)?;
    if bytes.len() < LOGICAL_VOLUME_FIXED {
        return None;
    }
    let block_size = le32(bytes, 212);
    if !BLOCK_SIZES.contains(&block_size) {
        return None;
    }
    let file_set = long(bytes, 248)?;
    if file_set.bytes == 0 {
        return None;
    }
    Some(LogicalVolume {
        sequence: le32(bytes, 16),
        block_size,
        // The domain identifier is a 32-byte registered identifier at 216, whose
        // last eight bytes are a suffix its own standard defines — and for
        // `*OSTA UDF Compliant` the first two of those are the revision. The one
        // place a volume says which UDF it is.
        revision: le16(bytes, 240),
        file_set,
        maps: le32(bytes, 268),
        map_table_bytes: le32(bytes, 264),
    })
}

/// Spell a revision the way its own specification is numbered.
///
/// The field is binary coded decimal — 0x0201 is 2.01 and 0x0250 is 2.50 — so
/// printing the nibbles as hexadecimal digits is what produces the number, and
/// reading it as an integer would produce 2.80 for the revision Blu-ray uses.
pub fn revision(packed: u16) -> String {
    format!("{:x}.{:02x}", packed >> 8, packed & 0xFF)
}

/// How a logical volume's blocks reach a partition's.
///
/// **Four kinds, and three of them exist because optical media are strange.** A
/// physical map is the identity and is what a disc image, a hard disk and a
/// stick all have. The others are indirections invented to make a medium that
/// cannot be rewritten look like one that can.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Map {
    /// Type 1: this logical partition *is* that physical one.
    Physical { partition: u16 },
    /// Type 2 `*UDF Sparable Partition Map`: the identity, plus a table of
    /// blocks that went bad and where they were moved to.
    Sparable { partition: u16 },
    /// Type 2 `*UDF Virtual Partition Map`: every block number goes through a
    /// table written at the end of what has been recorded, which is how a
    /// write-once disc pretends to be rewritable. `partition` is the physical
    /// one the table's entries are block numbers of.
    Virtual { partition: u16 },
    /// Type 2 `*UDF Metadata Partition Map`: the partition's contents are the
    /// contents of a *file*, so every block in it is one more indirection
    /// through that file's extents. What a Blu-ray disc has.
    Metadata { partition: u16, file: u32 },
    /// A type this has no rule for, which is not the same as a broken one.
    Unknown,
}

/// Walk a logical volume descriptor's partition maps.
///
/// `each` returns false to stop. The maps are variable-length and a logical
/// volume's *partition reference numbers* are indices into this list, so the
/// order they come out in is the order they have to be kept in.
pub fn maps(bytes: &[u8], count: u32, table_bytes: u32, mut each: impl FnMut(Map) -> bool) {
    let end = LOGICAL_VOLUME_FIXED.saturating_add(table_bytes as usize);
    let Some(table) = bytes.get(LOGICAL_VOLUME_FIXED..end.min(bytes.len())) else {
        return;
    };
    let mut at = 0usize;
    for _ in 0..count {
        if at + 2 > table.len() {
            return;
        }
        let kind = table[at];
        let length = usize::from(table[at + 1]);
        if length < 2 || at + length > table.len() {
            return;
        }
        let one = &table[at..at + length];
        let map = match kind {
            1 if length >= 6 => Map::Physical {
                partition: le16(one, 4),
            },
            2 if length >= 44 => type_two(one),
            _ => Map::Unknown,
        };
        if !each(map) {
            return;
        }
        at += length;
    }
}

/// Which of the three type 2 maps this is, by the identifier in it.
///
/// The identifier is a UDF one rather than an ECMA-167 one, which is why these
/// are strings while the rest of this file is numbers.
fn type_two(one: &[u8]) -> Map {
    // A registered identifier is a *flags* byte and then the name, so the name
    // starts one byte in. Comparing from the front matches nothing, which makes
    // every sparable and every metadata volume look like one kosha cannot
    // address — and both are real: a rewritable disc is the first and a Blu-ray
    // is the second.
    let identifier = &one[5..36];
    let partition = le16(one, 38);
    if identifier.starts_with(b"*UDF Sparable Partition") {
        Map::Sparable { partition }
    } else if identifier.starts_with(b"*UDF Virtual Partition") {
        Map::Virtual { partition }
    } else if identifier.starts_with(b"*UDF Metadata Partition") {
        Map::Metadata {
            partition,
            file: le32(one, 40),
        }
    } else {
        Map::Unknown
    }
}

// ------------------------------------------------------------- the file set

/// Where the root of the tree is.
///
/// A file set descriptor also names a *system stream* directory, which holds
/// named streams rather than files and which mkudffs writes on every volume it
/// formats. It is deliberately not read: nothing reachable from it is a file,
/// and every other reader here leaves a filesystem's own structures out of the
/// tree for the same reason.
pub fn file_set(bytes: &[u8]) -> Option<Allocation> {
    tagged(bytes, TAG_FILE_SET, None)?;
    let root = long(bytes, 400)?;
    if root.bytes == 0 {
        return None;
    }
    Some(root)
}

// ------------------------------------------------------------- file entries

/// Where a file entry keeps its allocation descriptors, and in what form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Descriptors {
    Short,
    Long,
    Extended,
    /// The file's own bytes are in the entry, where its descriptors would have
    /// been. **How almost every small directory on a UDF volume is stored** —
    /// mkudffs and Windows both do it — so a reader that treats this as a
    /// corner gets an empty root and no error at all.
    Embedded,
}

/// A file entry, of either kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct File {
    /// `FileType` from the ICB tag: a directory, a file, a symbolic link, or
    /// something a scan has no column for.
    pub kind: u8,
    pub descriptors: Descriptors,
    /// The file's length, which is what a scan calls apparent size.
    pub length: u64,
    /// Blocks actually recorded, which is what a scan calls size on disk. Not
    /// derived from the length, because a UDF file may have holes.
    pub blocks: u64,
    /// How many names this entry has. Above one, the bytes are counted once and
    /// every other name is a second name for them.
    pub links: u16,
    pub mtime: u32,
    /// The entry's own identity, which is what tells two names for one file
    /// apart from two files.
    pub unique: u64,
    /// Where the descriptors — or the embedded bytes — are inside the entry.
    pub at: usize,
    pub bytes: usize,
}

impl File {
    pub fn is_directory(&self) -> bool {
        self.kind == TYPE_DIRECTORY
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == TYPE_SYMLINK
    }
}

/// Read a file entry, which is either an `FE` or an `EFE`.
///
/// **Two layouts for one structure, and a volume has whichever its revision
/// says.** UDF 1.x writes a file entry; 2.x writes an extended one, which
/// inserts an object size and a creation time and so moves every field after
/// them. mkisofs writes the first, Windows and mkudffs the second — and a
/// reader that assumed either would find a length in the wrong place, which
/// looks like a plausible number rather than like an error. So which layout it
/// is comes from the tag rather than from the volume's stated revision.
pub fn file(bytes: &[u8]) -> Option<File> {
    let tag = tag(bytes, None)?;
    let extended = match tag.identifier {
        TAG_FILE_ENTRY => false,
        TAG_EXTENDED_FILE_ENTRY => true,
        _ => return None,
    };
    // The ICB tag, which is the same twenty bytes in both: the file type is its
    // twelfth byte and the flags its last two.
    let kind = *bytes.get(16 + 11)?;
    let flags = le16(bytes.get(..36)?, 34);
    let descriptors = match flags & 0x07 {
        0 => Descriptors::Short,
        1 => Descriptors::Long,
        2 => Descriptors::Extended,
        3 => Descriptors::Embedded,
        // 4 to 7 are not allocated by ECMA-167, and a reader that guessed would
        // read a file's own bytes as descriptors.
        _ => return None,
    };

    let links = le16(bytes, 48);
    let length = le64(bytes, 56);
    // The difference between the two layouts that matters: an extended entry
    // puts an object size at 64, which pushes the recorded blocks to 72 and
    // everything after it by forty bytes.
    let (blocks, modified, tail) = if extended {
        (le64(bytes, 72), 92, 208)
    } else {
        (le64(bytes, 64), 84, 168)
    };
    let mtime = timestamp(bytes.get(modified..modified + 12)?);
    let unique = le64(bytes, if extended { 200 } else { 160 });

    let attributes = le32(bytes.get(..tail + 8)?, tail) as usize;
    let count = le32(bytes, tail + 4) as usize;
    let at = (tail + 8).checked_add(attributes)?;
    // A count that runs past the end of the block the entry was read into. An
    // ICB is one block by construction, so this is damage rather than something
    // to read more of.
    if at.checked_add(count)? > bytes.len() {
        return None;
    }

    Some(File {
        kind,
        descriptors,
        length,
        blocks,
        links,
        mtime,
        unique,
        at,
        bytes: count,
    })
}

/// Where an indirect entry points, on a volume written with the two-step
/// strategy.
///
/// Strategy 4096 writes a pair: an indirect entry in a fixed place, and the
/// real file entry wherever there was room. Rare, legal, and one `long_ad`
/// away, which is cheaper than refusing a volume over it.
pub fn indirect(bytes: &[u8]) -> Option<Allocation> {
    tagged(bytes, TAG_INDIRECT, None)?;
    long(bytes, 36)
}

/// The header of an allocation extent descriptor, after which more allocation
/// descriptors follow. Returns where they start and how many bytes of them.
pub fn allocation_extent(bytes: &[u8], at: u32) -> Option<(usize, usize)> {
    tagged(bytes, TAG_ALLOCATION_EXTENT, Some(at))?;
    let count = le32(bytes.get(..24)?, 20) as usize;
    if 24usize.checked_add(count)? > bytes.len() {
        return None;
    }
    Some((24, count))
}

// -------------------------------------------------------- file identifiers

/// One entry of a directory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Identifier<'a> {
    /// The whole descriptor's length, padding included, which is how the next
    /// one is found.
    pub length: usize,
    pub characteristics: u8,
    pub icb: Allocation,
    /// The name as recorded, in OSTA Compressed Unicode.
    pub name: &'a [u8],
}

impl Identifier<'_> {
    pub fn is_directory(&self) -> bool {
        self.characteristics & DIRECTORY != 0
    }

    /// The entry for the directory above this one, which every directory has as
    /// its first and which carries no name.
    pub fn is_parent(&self) -> bool {
        self.characteristics & PARENT != 0
    }

    /// A name whose file is gone. UDF leaves the entry in place and marks it,
    /// because on a medium that cannot be rewritten that is the only way to
    /// delete anything.
    pub fn is_deleted(&self) -> bool {
        self.characteristics & DELETED != 0
    }
}

/// Read the file identifier descriptor at the front of `bytes`.
///
/// `at` is the logical block this descriptor begins in, which its tag repeats
/// and which is therefore the check that a walk is still aligned. A directory
/// is a run of these and there are no lengths anywhere else, so one misread
/// length would otherwise turn the rest of the directory into noise that
/// happens to parse.
pub fn identifier(bytes: &[u8], at: Option<u32>) -> Option<Identifier<'_>> {
    tagged(bytes, TAG_FILE_ID, at)?;
    if bytes.len() < FILE_ID_FIXED {
        return None;
    }
    let name_bytes = usize::from(bytes[19]);
    let use_bytes = usize::from(le16(bytes, 36));
    let body = FILE_ID_FIXED
        .checked_add(use_bytes)?
        .checked_add(name_bytes)?;
    // Every descriptor begins on a four-byte boundary, so one is padded up to a
    // multiple of four. The padding is inside the length and inside the CRC.
    let length = body.next_multiple_of(4);
    if length > bytes.len() {
        return None;
    }
    let name_at = FILE_ID_FIXED + use_bytes;
    Some(Identifier {
        length,
        characteristics: bytes[18],
        icb: long(bytes, 20)?,
        name: &bytes[name_at..name_at + name_bytes],
    })
}

// -------------------------------------------------------------------- names

/// Turn a recorded name into UTF-8.
///
/// **OSTA Compressed Unicode is UTF-16 with one byte in front saying whether
/// the top half of every code unit was worth storing.** A name of nothing but
/// Latin-1 is written one byte per character behind an 8; anything else is
/// written two bytes per code unit, big-endian, behind a 16. So `README.TXT`
/// costs eleven bytes and `Grüße 日本.txt` costs twenty-one, and both are
/// Unicode — which is the difference between this and Joliet, where every name
/// costs two bytes a character whatever is in it.
///
/// Returns false for a name this cannot spell, which is a compression
/// identifier that is neither: the volume is using something UDF does not
/// define, and inventing characters for it would be worse than saying so.
pub fn push_name(recorded: &[u8], out: &mut Vec<u8>) -> bool {
    let Some((&compression, rest)) = recorded.split_first() else {
        // A name of no bytes at all, which is legal only for the parent entry —
        // and that one is never asked for a name.
        return false;
    };
    match compression {
        8 => {
            let mut buffer = [0u8; 4];
            for &byte in rest {
                // One byte per character means the code point *is* the byte:
                // Latin-1, not UTF-8 and not any code page. 0xFC is ü — and
                // each of the 256 is a character, so none of this can fail.
                out.extend_from_slice(char::from(byte).encode_utf8(&mut buffer).as_bytes());
            }
            true
        }
        16 => {
            // A trailing odd byte is dropped, which is a name whose last code
            // unit was cut in half — there is no character there to produce.
            //
            // **`decode_utf16` rather than a loop over the units**, which is
            // what the FAT and ISO 9660 readers do with their own UTF-16 and is
            // why neither of them has ever had a surrogate bug. This did have
            // one: it consumed the next unit to find out whether it completed a
            // pair, and lost it when it did not, so a stray surrogate followed
            // by an `x` came back without the `x`. Four readers here decode
            // UTF-16, and this was the only one that spelt the decoding out.
            let units = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair));
            let mut buffer = [0u8; 4];
            for character in char::decode_utf16(units) {
                // An unpaired surrogate has no character of its own, and a name
                // that cannot be rendered is still a name that takes up space.
                let character = character.unwrap_or(char::REPLACEMENT_CHARACTER);
                out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
            true
        }
        _ => false,
    }
}

// --------------------------------------------------------------- timestamps

/// A twelve-byte timestamp, as seconds since 1970.
///
/// **The only date format kosha reads that says which clock it was.** The first
/// two bytes hold a type and a timezone in minutes, twelve bits and signed, so
/// a file written in India records 19:29 and +330 — and subtracting the offset
/// is the difference between a date that is right and one out by hours. FAT
/// records local time and no offset, which is why its dates cannot be fixed.
///
/// Zero where the date is not one, which is what the other readers here do: a
/// date out by a month looks like a fact.
pub fn timestamp(bytes: &[u8]) -> u32 {
    if bytes.len() < 12 {
        return 0;
    }
    let kind_and_zone = le16(bytes, 0);
    let year = i64::from(i16::from_le_bytes([bytes[2], bytes[3]]));
    let Some(seconds) = calendar::unix_seconds(
        year,
        u32::from(bytes[4]),
        u32::from(bytes[5]),
        u32::from(bytes[6]),
        u32::from(bytes[7]),
        u32::from(bytes[8]),
    ) else {
        return 0;
    };

    // The low twelve bits are the offset in minutes, two's complement. 2047
    // means the volume declined to say, and a type other than 1 means the
    // fields are not local time and so want no adjusting.
    let zone = kind_and_zone & 0x0FFF;
    if kind_and_zone >> 12 != 1 || zone == 2047 {
        return seconds;
    }
    let minutes = if zone & 0x0800 != 0 {
        i32::from(zone) - 0x1000
    } else {
        i32::from(zone)
    };
    // An offset past a day in either direction is damage rather than a
    // timezone.
    if minutes.abs() > 24 * 60 {
        return seconds;
    }
    let shifted = i64::from(seconds) - i64::from(minutes) * 60;
    u32::try_from(shifted).unwrap_or(seconds)
}

// ------------------------------------------------ the recognition sequence

/// Whether these bytes are a volume structure descriptor naming UDF.
///
/// The sequence is a list of headers, one per block: a structure type, five
/// characters and a version. `NSR02` is ECMA-167 second edition and `NSR03` the
/// third, and a reader needs neither number.
///
/// **What this is for is the question one layer out**, which is *is this file a
/// disk image at all*. That question is asked of a path that has already failed
/// to scan, by something with no block size and no anchor — and the answer is
/// the medium's own announcement, which is here and is the same six bytes on
/// every volume whatever its geometry.
pub fn recognises_udf(block: &[u8]) -> bool {
    block.len() >= 6 && (&block[1..6] == b"NSR02" || &block[1..6] == b"NSR03")
}

/// Whether these bytes end the recognition sequence, after which there is no
/// point in reading more of it.
pub fn ends_recognition(block: &[u8]) -> bool {
    block.len() >= 6 && &block[1..6] == b"TEA01"
}

// ------------------------------------------ the virtual allocation table

/// Where a virtual allocation table's entries are inside the file that holds
/// it: the offset of the first, and how many there are.
///
/// **Two forms, told apart by the file's type.** UDF 2.00 gave the table a file
/// type of its own and put a header in front of the entries; 1.50 had neither,
/// and a reader knows a 1.50 table by the registered identifier written *after*
/// the entries, at a fixed distance from the end. So the older one is recognised
/// by its tail and the newer by its head, which is the sort of thing that
/// happens to a format revised for a medium that was already obsolete.
///
/// `None` where this is not a table at all, which is the answer for every other
/// file that might be the last one written to a disc.
pub fn vat(data: &[u8], kind: u8) -> Option<(usize, usize)> {
    if kind == TYPE_VAT {
        // A header whose own length says where it ends, so that a revision may
        // add fields to it without moving the entries.
        let header = usize::from(le16(data.get(..4)?, 0));
        let entries = data.len().checked_sub(header)?;
        return Some((header, entries / 4));
    }
    // A 1.50 table is a file of no particular type, so this has to be sure:
    // the identifier is twenty-two characters at a known distance from the end,
    // and nothing else a disc ends with looks like that.
    let tail = data.len().checked_sub(VAT_TAIL)?;
    let regid = data.get(tail..tail + 32)?;
    if !regid[1..].starts_with(b"*UDF Virtual Alloc Tbl") {
        return None;
    }
    Some((0, tail / 4))
}

/// One entry of a virtual allocation table, or `None` where it names no block.
///
/// 0xFFFFFFFF is how a table says a virtual block was never written — which on
/// a medium where nothing can be erased is what a deleted file leaves behind.
pub fn vat_entry(entries: &[u8], index: usize) -> Option<u32> {
    let at = index.checked_mul(4)?;
    let block = le32(entries.get(at..at + 4)?, 0);
    (block != u32::MAX).then_some(block)
}
