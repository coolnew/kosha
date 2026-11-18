// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! ISO 9660 on-disc structures, as bytes in and numbers out.
//!
//! **The oldest format kosha reads, and the simplest.** It was written for a
//! medium nobody could rewrite, which removes from the problem almost
//! everything the other formats exist to solve: there is no allocation table,
//! no free-space map, no chain to follow and no journal. A file is one run of
//! consecutive blocks, and a directory is one run of consecutive blocks full
//! of records. Everything here is a record and an offset.
//!
//! ```text
//! 0x8000  volume descriptors   one per block, until a terminator
//!   +156  root directory record  where the tree starts
//! extent  directory records    a length, a name, and where the bytes are
//!   tail  system use area      Rock Ridge, when the disc carries it
//! ```
//!
//! ## Three names for one file, and which one is used
//!
//! ISO 9660 as published allows eight characters, a dot, three more, and a
//! semicolon with a version number: `README.TXT;1`. Nobody could live with
//! that, so there are two extensions and a disc usually carries both.
//!
//! - **Rock Ridge** hides POSIX metadata in the space after each record's
//!   name, including an `NM` entry holding the real one. The disc keeps one
//!   tree and the names in it are the true ones.
//! - **Joliet** writes a *second* volume descriptor with a *second* tree, in
//!   which names are UTF-16. That is what Windows reads.
//!
//! Which to prefer is a real choice and it is made the way `mount` makes it:
//! Rock Ridge where the disc has it, because it is the tree the author of the
//! disc meant for a machine that understands files, and because Joliet
//! truncates a name at sixty-four characters. Joliet where there is no Rock
//! Ridge. The published names where there is neither.
//!
//! ## What is needed, and what is not
//!
//! A file's length and its start are both in the record that names it, and a
//! file is contiguous by construction, so nothing here ever reads a file. The
//! path tables — two of them, one in each byte order — are an index for
//! seeking to a directory by path, and a walk that visits every directory
//! anyway has no use for either.

use crate::calendar;

/// Where the volume descriptors begin: sixteen blocks of 2048 bytes.
///
/// The space in front of them belongs to whoever wrote the disc. On a
/// bootable image it holds a master boot record and a partition table, which
/// is why one of those can describe the same bytes without disturbing
/// anything — and why a reader that looks for a partition table first finds a
/// two-megabyte boot partition on a five-gigabyte disc.
pub const DESCRIPTORS_AT: u64 = 16 * 2048;

/// The block a descriptor occupies, which is fixed by the standard at 2048
/// whatever the volume's own logical block size turns out to be.
pub const DESCRIPTOR_BYTES: usize = 2048;

const MAGIC: &[u8; 5] = b"CD001";

/// Volume descriptor types.
const PRIMARY: u8 = 1;
const SUPPLEMENTARY: u8 = 2;
const TERMINATOR: u8 = 255;

/// The most descriptors to walk before giving up.
///
/// A disc has three or four. The list ends with a terminator, and a disc whose
/// terminator was never written would otherwise be read to its end one block
/// at a time.
pub const MAX_DESCRIPTORS: usize = 64;

/// One directory record's fixed part, before the name.
pub const RECORD_HEADER: usize = 33;

/// `flags` in a directory record.
const FLAG_DIRECTORY: u8 = 0x02;
/// The record is not the last extent of this file: its length adds to the
/// next record's, and both name the same file. How ISO 9660 stores a file
/// larger than four gigabytes.
const FLAG_MULTI_EXTENT: u8 = 0x80;

/// Block sizes a volume may state. Every offset here is a multiple of one.
const BLOCK_SIZES: [u32; 4] = [512, 1024, 2048, 4096];

/// Why an ISO 9660 filesystem could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unreadable {
    #[error("this is not an ISO 9660 filesystem")]
    NotIso,
    #[error("the volume's geometry does not make sense")]
    Impossible,
}

/// Which tree a walk is reading, and so how a name in it is spelt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Naming {
    /// The published names: uppercase, and with a version number after a
    /// semicolon. Only where the disc carries neither extension.
    Published,
    /// Rock Ridge `NM` entries in the primary tree.
    RockRidge,
    /// A Joliet tree of its own, with names in UTF-16, big-endian.
    Joliet,
}

impl Naming {
    /// What to put on the tree, so that somebody can see why the names look
    /// the way they do.
    ///
    /// **Which tree was read is a judgement and so it is recorded**, the same
    /// rule the choice between partitions follows. It matters most when the
    /// answer is `Published`: those names are uppercase and mangled to eight
    /// characters, and somebody looking at `GR__E___.TXT` deserves to be told
    /// that the disc carries nothing better rather than left wondering what
    /// kosha did to it.
    pub fn in_a_note(self) -> &'static str {
        match self {
            Naming::Published => "short names only",
            Naming::RockRidge => "Rock Ridge",
            Naming::Joliet => "Joliet",
        }
    }
}

/// A volume descriptor that names a tree: the primary one, or a Joliet
/// supplement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Volume {
    pub block_size: u32,
    /// How many blocks the volume says it occupies.
    pub blocks: u32,
    /// Where the root directory's records are, and how many bytes of them.
    pub root_extent: u32,
    pub root_bytes: u32,
    /// True for a supplementary descriptor whose escape sequence says UTF-16.
    pub joliet: bool,
}

impl Volume {
    /// Where a block starts, in bytes.
    pub fn block_offset(&self, block: u32) -> Option<u64> {
        if block >= self.blocks {
            return None;
        }
        u64::from(block).checked_mul(u64::from(self.block_size))
    }

    /// How many bytes the volume says it is.
    ///
    /// Used to refuse a file longer than the disc that holds it, which is what
    /// a flipped bit in a length looks like.
    pub fn bytes(&self) -> u64 {
        u64::from(self.blocks).saturating_mul(u64::from(self.block_size))
    }

    /// How much of the disc a file of this length occupies.
    ///
    /// Its length rounded up to a block, and exactly that: a file is one run
    /// of consecutive blocks on a medium that cannot be rewritten, so there is
    /// nothing else it could be. The rounding is not a rounding error — the
    /// tail of the last block of every file on a disc is the reason a disc of
    /// ten thousand small files is larger than the files in it.
    pub fn allocation(&self, length: u64) -> u64 {
        let block = u64::from(self.block_size);
        if block == 0 {
            return 0;
        }
        length.div_ceil(block).saturating_mul(block)
    }
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Whether a block holds a volume descriptor, and which type.
///
/// **A disc says what it is in six bytes and they are the whole of the
/// check.** There is no checksum anywhere in ISO 9660 — the medium was
/// supposed to be the guarantee — so the numbers that follow are validated by
/// agreeing with each other and with the length of the file, and by nothing
/// else.
pub fn descriptor(block: &[u8]) -> Option<u8> {
    if block.len() < DESCRIPTOR_BYTES || &block[1..6] != MAGIC {
        return None;
    }
    // Version 1 is the only one ever published. A disc claiming another is one
    // written to a standard that does not exist.
    if block[6] != 1 {
        return None;
    }
    Some(block[0])
}

pub fn is_primary(kind: u8) -> bool {
    kind == PRIMARY
}

pub fn is_supplementary(kind: u8) -> bool {
    kind == SUPPLEMENTARY
}

pub fn is_terminator(kind: u8) -> bool {
    kind == TERMINATOR
}

/// Read a primary or supplementary descriptor.
///
/// The two have the same layout; a supplementary one fills the thirty-two
/// bytes at 88, which the primary leaves blank, with an escape sequence saying
/// what its names are encoded in.
pub fn volume(block: &[u8]) -> Result<Volume, Unreadable> {
    let kind = descriptor(block).ok_or(Unreadable::NotIso)?;
    if kind != PRIMARY && kind != SUPPLEMENTARY {
        return Err(Unreadable::NotIso);
    }

    // Both-endian fields: the same number written little-endian and then
    // big-endian, so that a machine of either kind could read the disc without
    // swapping bytes. Only the first half is read here — the second is the
    // same number, and a disc where the two disagree is damaged in a way that
    // taking either answer would not fix.
    let blocks = le32(block, 80);
    let block_size = u32::from(le16(block, 128));
    if !BLOCK_SIZES.contains(&block_size) {
        return Err(Unreadable::Impossible);
    }
    if blocks == 0 {
        return Err(Unreadable::Impossible);
    }

    let root = record(&block[156..]).ok_or(Unreadable::Impossible)?;
    if !root.is_directory() {
        return Err(Unreadable::Impossible);
    }
    if root.extent >= blocks {
        return Err(Unreadable::Impossible);
    }

    Ok(Volume {
        block_size,
        blocks,
        root_extent: root.extent,
        root_bytes: root.size,
        joliet: kind == SUPPLEMENTARY && joliet_escape(block),
    })
}

/// Whether a supplementary descriptor's escape sequence says Joliet.
///
/// Three sequences, one per level of the Joliet specification, differing only
/// in how long a name may be — which is not something a reader cares about,
/// because it is reading names rather than writing them.
fn joliet_escape(block: &[u8]) -> bool {
    let escapes = &block[88..120];
    escapes.starts_with(b"%/@") || escapes.starts_with(b"%/C") || escapes.starts_with(b"%/E")
}

/// One directory record: a length, where the bytes are, and a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record<'a> {
    /// The whole record's length, which is how the next one is found.
    pub length: usize,
    pub extent: u32,
    pub size: u32,
    pub flags: u8,
    pub mtime: u32,
    /// As recorded: a published name with its version suffix, or UTF-16 for a
    /// Joliet tree. One byte of 0x00 is `.` and one of 0x01 is `..`.
    pub name: &'a [u8],
    /// Whatever follows the name, which is where Rock Ridge lives.
    pub system_use: &'a [u8],
}

impl Record<'_> {
    pub fn is_directory(&self) -> bool {
        self.flags & FLAG_DIRECTORY != 0
    }

    /// Whether the file continues in the record after this one.
    pub fn continues(&self) -> bool {
        self.flags & FLAG_MULTI_EXTENT != 0
    }

    /// `.`, which every directory records as its own first entry.
    pub fn is_self(&self) -> bool {
        self.name == [0x00]
    }

    /// `..`, the second.
    pub fn is_parent(&self) -> bool {
        self.name == [0x01]
    }
}

/// Read the record at the front of `bytes`.
///
/// `None` where there is no record: a zero length is how a directory says the
/// rest of this block is padding, which is not damage — the standard forbids a
/// record from straddling a block boundary, so a record that would have to is
/// pushed to the next block and zeros fill the gap.
pub fn record(bytes: &[u8]) -> Option<Record<'_>> {
    let length = usize::from(*bytes.first()?);
    if length < RECORD_HEADER || length > bytes.len() {
        return None;
    }
    let name_length = usize::from(bytes[32]);
    let name_end = RECORD_HEADER.checked_add(name_length)?;
    if name_end > length {
        return None;
    }
    // A pad byte follows a name of even length, so that every record begins on
    // an even offset — which is what makes the word-sized fields of the next
    // one aligned on the machines this was written for.
    let after = name_end + usize::from(name_length % 2 == 0);
    Some(Record {
        length,
        extent: le32(bytes, 2),
        size: le32(bytes, 10),
        flags: bytes[25],
        mtime: timestamp(&bytes[18..25]),
        name: &bytes[RECORD_HEADER..name_end],
        system_use: bytes.get(after..length).unwrap_or(&[]),
    })
}

// ------------------------------------------------------------- Rock Ridge

/// One entry of a System Use area: two letters, a length, a version, and data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SystemUse<'a> {
    pub signature: [u8; 2],
    pub data: &'a [u8],
}

/// Walk a System Use area, handing each entry to `each`.
///
/// Stops at `ST`, at a length that cannot be one, or at the end of the area.
/// **A `CE` entry is handed over like any other** rather than followed here,
/// because following it is a read and this file does not do reads — the reader
/// decides whether to spend one.
pub fn system_use(area: &[u8], mut each: impl FnMut(SystemUse<'_>) -> bool) {
    let mut at = 0usize;
    while at + 4 <= area.len() {
        let length = usize::from(area[at + 2]);
        if length < 4 || at + length > area.len() {
            return;
        }
        let signature = [area[at], area[at + 1]];
        // `ST` ends the area early, which a disc uses when the space after it
        // is padding rather than more entries.
        if &signature == b"ST" {
            return;
        }
        if !each(SystemUse {
            signature,
            data: &area[at + 4..at + length],
        }) {
            return;
        }
        at += length;
    }
}

/// Whether this area announces that the disc uses System Use sharing at all.
///
/// The `SP` entry, which lives only in the `.` record of the root directory
/// and carries two bytes nothing else on a disc contains. Its presence is what
/// says Rock Ridge may be here; its absence says the bytes after every name
/// are somebody else's and must not be parsed.
pub fn announces_sharing(area: &[u8]) -> bool {
    let mut found = false;
    system_use(area, |entry| {
        if &entry.signature == b"SP" && entry.data.len() >= 3 && entry.data[0..2] == [0xBE, 0xEF] {
            found = true;
            return false;
        }
        true
    });
    found
}

/// A `CE` entry: more System Use entries, somewhere else on the disc.
///
/// Reached when a record's own tail fills up, which a long name and a full set
/// of POSIX attributes will do — a directory record may not exceed 255 bytes,
/// and that is the whole of why this exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Continuation {
    pub block: u32,
    pub offset: u32,
    pub length: u32,
}

pub fn continuation(entry: &SystemUse<'_>) -> Option<Continuation> {
    if &entry.signature != b"CE" || entry.data.len() < 24 {
        return None;
    }
    Some(Continuation {
        block: le32(entry.data, 0),
        offset: le32(entry.data, 8),
        length: le32(entry.data, 16),
    })
}

/// A `PX` entry's `st_mode`, which is the only thing on a disc that says an
/// entry is a symbolic link.
///
/// A record's own flags have one bit for "directory" and nothing for a link, so
/// without this a link is a file of no bytes — which is what it looked like
/// until this was read, while the ext4, XFS and UDF readers beside it all said
/// `symlink` about the same tree.
///
/// The entry is 36 bytes, or 44 where it carries an inode number as well;
/// `st_mode` is the first field and that is all this takes. The link count sits
/// right behind it and is deliberately not read: `xorrisofs` writes 1 there for
/// every name of a file it has pooled, so it says nothing true about sharing —
/// see [`build`](super::build), which keys on the extent instead.
pub fn posix_mode(entry: &SystemUse<'_>) -> Option<u32> {
    if &entry.signature != b"PX" || entry.data.len() < 8 {
        return None;
    }
    Some(le32(entry.data, 0))
}

/// `S_IFMT`, the four bits of a mode that say what kind of thing it is.
const MODE_FORMAT: u32 = 0xF000;
/// `S_IFLNK`.
const MODE_SYMLINK: u32 = 0xA000;

/// Whether a `PX` mode is a symbolic link's.
pub fn mode_is_symlink(mode: u32) -> bool {
    mode & MODE_FORMAT == MODE_SYMLINK
}

/// An `NM` entry's flags: this is a piece of a name and more follows.
const NM_CONTINUE: u8 = 0x01;
/// The name is `.` or `..`, and the bytes after the flags are not a name.
const NM_CURRENT: u8 = 0x02;
const NM_PARENT: u8 = 0x04;

/// Append an `NM` entry's piece of a name, and say whether more is coming.
///
/// **A name arrives in pieces because an entry is at most 255 bytes**, so a
/// long one is several `NM` entries in a row, each but the last flagged to
/// continue. Returning the flag rather than accumulating here keeps the
/// decision about how many pieces to accept with the caller, which is where
/// the bound belongs.
pub fn push_name(entry: &SystemUse<'_>, out: &mut Vec<u8>) -> Option<bool> {
    if &entry.signature != b"NM" || entry.data.is_empty() {
        return None;
    }
    let flags = entry.data[0];
    // `.` and `..` spelt in Rock Ridge. They name nothing new and the bytes
    // after the flags are not a name, so appending them would invent one.
    if flags & (NM_CURRENT | NM_PARENT) != 0 {
        return None;
    }
    out.extend_from_slice(&entry.data[1..]);
    Some(flags & NM_CONTINUE != 0)
}

// --------------------------------------------------------------------- text

/// A published ISO 9660 name, with the parts nobody wants removed.
///
/// Two of them. The `;1` is a version number, which the standard requires and
/// which no filesystem that ever mounted one has shown. A trailing `.` is what
/// a file with no extension is recorded as, and it is not part of the name
/// either.
pub fn push_published_name(name: &[u8], out: &mut Vec<u8>) {
    let end = name
        .iter()
        .rposition(|byte| *byte == b';')
        .unwrap_or(name.len());
    let name = &name[..end];
    let name = match name.strip_suffix(b".") {
        Some(shorter) => shorter,
        None => name,
    };
    out.extend_from_slice(name);
}

/// A Joliet name: UTF-16, big-endian, and a version suffix in UTF-16 too.
///
/// Big-endian because Joliet is specified in terms of UCS-2 as ISO defines it,
/// which is network order — the one place in this whole reader where a number
/// is not little-endian, and the one that produces Chinese characters out of
/// an English filename when it is forgotten.
pub fn push_joliet_name(name: &[u8], out: &mut Vec<u8>) {
    let units: Vec<u16> = name
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .collect();
    let end = units
        .iter()
        .rposition(|unit| *unit == u16::from(b';'))
        .unwrap_or(units.len());
    let units = &units[..end];
    let units = match units.split_last() {
        Some((last, rest)) if *last == u16::from(b'.') => rest,
        _ => units,
    };
    let mut buffer = [0u8; 4];
    for character in char::decode_utf16(units.iter().copied()) {
        let character = character.unwrap_or(char::REPLACEMENT_CHARACTER);
        out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
    }
}

// --------------------------------------------------------------------- time

/// The seven bytes a directory record spends on a date.
///
/// Readable on sight, which is unusual here: a year since 1900, a month, a
/// day, an hour, a minute, a second, and then an offset from UTC in
/// fifteen-minute steps as a signed byte. **The zone is recorded**, unlike
/// FAT, so this is a real instant rather than a wall clock with no place
/// attached to it.
pub fn timestamp(bytes: &[u8]) -> u32 {
    if bytes.len() < 7 {
        return 0;
    }
    // An all-zero field is how a record says no date was written, and 1900 is
    // not an answer to give for it.
    if bytes[..6].iter().all(|byte| *byte == 0) {
        return 0;
    }
    let Some(seconds) = calendar::unix_seconds(
        1900 + i64::from(bytes[0]),
        u32::from(bytes[1]),
        u32::from(bytes[2]),
        u32::from(bytes[3]),
        u32::from(bytes[4]),
        u32::from(bytes[5]),
    ) else {
        return 0;
    };
    let steps = i64::from(bytes[6] as i8);
    u32::try_from(i64::from(seconds) - steps * 15 * 60).unwrap_or(0)
}
