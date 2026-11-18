// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! A UDF volume built byte by byte, and walked by exactly the code that walks a
//! real one.
//!
//! **Four volumes, because a volume may say the same thing four ways** and which
//! way is the choice of whoever wrote it. A reader that handles one of them reads
//! some real volumes and not others, and the ones it fails on look *empty*
//! rather than broken — which is the worst way for a reader to be wrong.
//!
//! | Shape | File entry | Directories | Extents | What writes it |
//! | --- | --- | --- | --- | --- |
//! | [`Shape::Short`] | `FE`, UDF 1.x | in extents | `short_ad` | mkisofs |
//! | [`Shape::Embedded`] | `EFE`, UDF 2.x | inside the entry | `short_ad` | mkudffs, Windows |
//! | [`Shape::Long`] | `EFE` | in extents | `long_ad` | |
//! | [`Shape::Extended`] | `EFE` | in extents | `ext_ad` | |
//!
//! All four hold the same tree and every size in it is the same, which is the
//! point of having four.
//!
//! **The tree is the one the ISO 9660 fixtures hold**, deliberately: the same
//! names and the same sizes, so the two readers are compared against each other
//! rather than each against its own expectations. On a real bridge disc they
//! describe one tree twice, and their exports come out identical.

use std::path::Path;

use super::format::{self, Unreadable};
use super::reader::Filesystem;
use crate::scan::{Progress, ScanOptions};

/// A block of the medium, for every fixture but the 512-byte one.
const BLOCK: usize = 2048;

/// Where the volume's structures go, in blocks of the medium. The numbers real
/// formatters use: the recognition sequence at 16 because that is where a
/// medium's is, and the anchor at 256 because the standard says so.
/// Where a pure volume's recognition sequence is. A disc that carries ISO 9660
/// as well puts its three descriptors at 16 and pushes this to 19, which is what
/// [`on_a_bridge_disc`] does.
const RECOGNITION: u32 = 16;
/// Where mkudffs puts the descriptors, and far enough in to leave the first
/// sixty-four blocks to an ISO 9660 filesystem sharing the medium.
const MAIN_SEQUENCE: u32 = 96;
const RESERVE_SEQUENCE: u32 = 112;
const ANCHOR: u32 = 256;
const PARTITION_START: u32 = 257;

/// How many blocks the partition gets: the same number of *bytes* whatever a
/// block is, so that the same tree fits in the 512-byte fixture.
fn partition_blocks(block_size: usize) -> u32 {
    (128 * BLOCK / block_size) as u32
}

/// One block past the partition, for the anchor at the end of the medium — which
/// UDF asks for as well, and which is the one a reader falls back to.
fn medium_blocks(block_size: usize) -> u32 {
    PARTITION_START + partition_blocks(block_size) + 1
}

/// 2026-09-25 15:03:11 UTC, which a volume in India records as 20:33:11 with an
/// offset of +330 minutes. The instant and the timezone on the disc this was
/// written against, and the reason the offset arithmetic is worth checking.
const WHEN: i64 = 1_790_348_591;
const ZONE_MINUTES: i32 = 330;

/// `FileType` in an ICB tag. Only a directory and a symbolic link mean anything
/// to the reader; everything else is a file, so the reader has no constant for
/// this and a fixture that writes volumes needs one.
const TYPE_FILE: u8 = 5;

/// `FileCharacteristics` in a file identifier descriptor.
const IS_DIRECTORY: u8 = 0x02;
const IS_DELETED: u8 = 0x04;
const IS_PARENT: u8 = 0x08;

/// What the tree holds, in one place, so the four fixtures cannot drift.
const TREE: &[(&str, u64)] = &[
    ("A long file name.bin", 100_000),
    ("README.TXT", 5_000),
    ("EMPTY.BIN", 0),
    ("docs", 0),
    ("note.md", 10),
    ("deeper", 0),
    ("Grüße 日本.txt", 10),
];

const LONG_FILE: usize = 0;
const README: usize = 1;
const EMPTY: usize = 2;
const DOCS: usize = 3;
const NOTE: usize = 4;
const DEEPER: usize = 5;
const UNICODE: usize = 6;

/// Which of the standard's arrangements a fixture is written in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shape {
    Short,
    Embedded,
    Long,
    Extended,
}

impl Shape {
    /// A UDF 1.x file entry, or the extended one 2.x replaced it with.
    fn extended_entry(self) -> bool {
        self != Shape::Short
    }

    /// The low three bits of an ICB tag's flags, which say what follows the
    /// entry.
    fn flags(self) -> u16 {
        match self {
            Shape::Short => 0,
            Shape::Long => 1,
            Shape::Extended => 2,
            Shape::Embedded => 3,
        }
    }

    /// How a *file* on this volume is addressed.
    ///
    /// **Not the same question as how a directory is**, because the choice is
    /// made per entry rather than per volume: a volume whose directories are
    /// small enough to live inside their own entries still has files of a hundred
    /// kilobytes, which cannot. That is exactly what mkudffs and Windows write,
    /// and getting it wrong here would have made the embedded fixture test the
    /// same path twice.
    fn for_a_file(self) -> Shape {
        match self {
            Shape::Embedded => Shape::Short,
            other => other,
        }
    }
}

// ---------------------------------------------------------------- the builder

/// A volume under construction.
struct Volume {
    bytes: Vec<u8>,
    block_size: usize,
    /// The next unused block of the partition.
    next: u32,
    shape: Shape,
    /// How far the physical block a structure is written at runs ahead of the
    /// block number it is addressed by.
    ///
    /// **Zero for every volume but a write-once one**, where it is what makes
    /// the test worth running: a table mapping *n* to *n* is one a reader could
    /// ignore entirely and still pass. With an offset, a reader that does not go
    /// through the table reads eight blocks before the file set and finds
    /// nothing at all.
    offset: u32,
    /// Which of the volume's partition maps everything in the tree is addressed
    /// through: the last one, which on a volume with an indirection is the
    /// indirection. **Addressing through map 0 is what the first version of this
    /// did**, and it made the metadata fixture pass without the metadata
    /// partition being used at all — the physical map resolves to the same
    /// blocks, so the test proved nothing.
    reference: u16,
}

impl Volume {
    fn new(shape: Shape, block_size: usize) -> Volume {
        Volume {
            bytes: vec![0u8; medium_blocks(block_size) as usize * block_size],
            block_size,
            next: 0,
            shape,
            offset: 0,
            reference: 0,
        }
    }

    /// Blocks of the partition, which are not blocks of the medium.
    fn claim(&mut self, count: u32) -> u32 {
        let at = self.next;
        self.next += count;
        assert!(
            self.next <= partition_blocks(self.block_size),
            "the fixture outgrew its partition"
        );
        at
    }

    fn write_medium(&mut self, block: u32, bytes: &[u8]) {
        let at = block as usize * self.block_size;
        self.bytes[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Write at the block a logical block number resolves to.
    fn write_partition(&mut self, block: u32, bytes: &[u8]) {
        self.write_medium(PARTITION_START + self.offset + block, bytes);
    }
}

/// Finish a descriptor: its CRC over what follows the tag, then its checksum.
///
/// **In that order and not the other**, because the checksum covers the two bytes
/// the CRC goes in. Doing it the other way round leaves a descriptor whose
/// checksum is wrong — which is what a reader is looking for, so a builder that
/// got this backwards would produce fixtures nothing could read.
fn seal(bytes: &mut [u8], identifier: u16, location: u32) {
    let covered = bytes.len() - 16;
    bytes[0..2].copy_from_slice(&identifier.to_le_bytes());
    // Descriptor version 3, which is what a UDF 2.x volume writes. The fixtures
    // that matter for the 1.x layout carry it too: which layout a file entry has
    // comes from its tag *identifier*, not from this, and a volume that mixed
    // them would still have to be read.
    bytes[2..4].copy_from_slice(&3u16.to_le_bytes());
    bytes[4] = 0;
    bytes[5] = 0;
    bytes[6..8].copy_from_slice(&1u16.to_le_bytes());
    let crc = format::crc16(&bytes[16..16 + covered]);
    bytes[8..10].copy_from_slice(&crc.to_le_bytes());
    bytes[10..12].copy_from_slice(&(covered as u16).to_le_bytes());
    bytes[12..16].copy_from_slice(&location.to_le_bytes());
    let sum = bytes[..16]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
    bytes[4] = sum;
}

/// A registered identifier: a flags byte, a name, and a suffix its own standard
/// defines.
fn regid(name: &str, suffix: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[1..1 + name.len()].copy_from_slice(name.as_bytes());
    out[24..24 + suffix.len()].copy_from_slice(suffix);
    out
}

fn long_ad(bytes: u32, block: u32, partition: u16, kind: u32) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&(bytes | (kind << 30)).to_le_bytes());
    out[4..8].copy_from_slice(&block.to_le_bytes());
    out[8..10].copy_from_slice(&partition.to_le_bytes());
    out
}

/// One allocation descriptor in whichever form a shape uses.
///
/// The `ext_ad` form carries a recorded length as well, and it is written here as
/// a number the reader must *not* take: what a scan reports as size on disk comes
/// from the file entry's own block count, and a second opinion in the extent
/// would be a place for the two to disagree. So this writes nonsense there on
/// purpose.
fn extent(shape: Shape, bytes: u32, block: u32, kind: u32, reference: u16) -> Vec<u8> {
    match shape {
        Shape::Short | Shape::Embedded => {
            let mut out = vec![0u8; format::SHORT_AD];
            out[0..4].copy_from_slice(&(bytes | (kind << 30)).to_le_bytes());
            out[4..8].copy_from_slice(&block.to_le_bytes());
            out
        }
        Shape::Long => long_ad(bytes, block, reference, kind).to_vec(),
        Shape::Extended => {
            let mut out = vec![0u8; format::EXT_AD];
            out[0..4].copy_from_slice(&(bytes | (kind << 30)).to_le_bytes());
            out[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            out[8..12].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            out[12..16].copy_from_slice(&block.to_le_bytes());
            out[16..18].copy_from_slice(&reference.to_le_bytes());
            out
        }
    }
}

/// The twelve bytes of a timestamp, as a volume in India records one.
fn timestamp() -> [u8; 12] {
    let local = WHEN + i64::from(ZONE_MINUTES) * 60;
    let (year, month, day) = civil_from_days(local.div_euclid(86_400));
    let rest = local.rem_euclid(86_400);
    let mut out = [0u8; 12];
    let kind_and_zone = (1u16 << 12) | (ZONE_MINUTES as u16 & 0x0FFF);
    out[0..2].copy_from_slice(&kind_and_zone.to_le_bytes());
    out[2..4].copy_from_slice(&(year as i16).to_le_bytes());
    out[4] = month as u8;
    out[5] = day as u8;
    out[6] = (rest / 3600) as u8;
    out[7] = ((rest % 3600) / 60) as u8;
    out[8] = (rest % 60) as u8;
    out
}

/// Days since 1970 back to a date, which a fixture needs and a reader does not:
/// a reader is handed the fields and a writer has to produce them.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let step = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * step + 2) / 5 + 1) as u32;
    let month = if step < 10 { step + 3 } else { step - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// A name in OSTA Compressed Unicode: one byte per character where every
/// character fits in one, and two big-endian bytes per code unit otherwise.
fn cs0(name: &str) -> Vec<u8> {
    if name.chars().all(|one| (one as u32) < 0x100) {
        let mut out = vec![8u8];
        out.extend(name.chars().map(|one| one as u8));
        out
    } else {
        let mut out = vec![16u8];
        for unit in name.encode_utf16() {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        out
    }
}

/// One entry of a directory, before it is known which block it will live in.
struct Named {
    characteristics: u8,
    icb: u32,
    name: Option<&'static str>,
}

/// A file identifier descriptor, padded to a multiple of four.
///
/// `at` is the partition block the descriptor will live in, which its tag has to
/// repeat — the check a walk of a directory relies on, and the reason these
/// cannot be built until the block is known.
fn fid(named: &Named, at: u32, reference: u16) -> Vec<u8> {
    let name = named.name.map(cs0).unwrap_or_default();
    let length = (format::FILE_ID_FIXED + name.len()).next_multiple_of(4);
    let mut out = vec![0u8; length];
    out[16..18].copy_from_slice(&1u16.to_le_bytes());
    out[18] = named.characteristics;
    out[19] = name.len() as u8;
    out[20..36].copy_from_slice(&long_ad(2048, named.icb, reference, 0));
    out[36..38].copy_from_slice(&0u16.to_le_bytes());
    out[format::FILE_ID_FIXED..format::FILE_ID_FIXED + name.len()].copy_from_slice(&name);
    seal(&mut out, format::TAG_FILE_ID, at);
    out
}

/// Every identifier of a directory, laid out at the block they will live in.
fn records(parent: u32, children: &[Named], at: u32, reference: u16) -> Vec<u8> {
    let mut out = fid(
        &Named {
            characteristics: IS_PARENT | IS_DIRECTORY,
            icb: parent,
            name: None,
        },
        at,
        reference,
    );
    for child in children {
        out.extend_from_slice(&fid(child, at, reference));
    }
    out
}

/// Everything a file entry says.
struct Entry {
    kind: u8,
    links: u16,
    length: u64,
    blocks: u64,
    /// How the bytes after the fixed part are to be read.
    shape: Shape,
    /// The allocation descriptors, or — for a shape that embeds — the file's own
    /// bytes.
    payload: Vec<u8>,
}

/// Write a file entry, of either layout, at a block of the partition.
fn write_entry(volume: &mut Volume, block: u32, entry: &Entry) {
    let extended = volume.shape.extended_entry();
    let fixed = if extended { 216 } else { 176 };
    let mut out = vec![0u8; fixed + entry.payload.len()];

    // The ICB tag: strategy 4, one entry, and the type and flags that say what
    // this is and how to read what follows it.
    out[20..22].copy_from_slice(&4u16.to_le_bytes());
    out[24..26].copy_from_slice(&1u16.to_le_bytes());
    out[16 + 11] = entry.kind;
    out[34..36].copy_from_slice(&entry.shape.flags().to_le_bytes());

    out[44..48].copy_from_slice(&0o755u32.to_le_bytes());
    out[48..50].copy_from_slice(&entry.links.to_le_bytes());
    out[56..64].copy_from_slice(&entry.length.to_le_bytes());
    let when = timestamp();
    let tail = if extended {
        // An extended entry inserts an object size at 64, which moves the
        // recorded blocks to 72 and everything after that by forty bytes.
        out[64..72].copy_from_slice(&entry.length.to_le_bytes());
        out[72..80].copy_from_slice(&entry.blocks.to_le_bytes());
        // Access, modification, creation, attribute.
        for at in [80, 92, 104, 116] {
            out[at..at + 12].copy_from_slice(&when);
        }
        out[200..208].copy_from_slice(&u64::from(block).to_le_bytes());
        208
    } else {
        out[64..72].copy_from_slice(&entry.blocks.to_le_bytes());
        // Access, modification, attribute — a 1.x entry has no creation time.
        for at in [72, 84, 96] {
            out[at..at + 12].copy_from_slice(&when);
        }
        out[160..168].copy_from_slice(&u64::from(block).to_le_bytes());
        168
    };
    out[tail..tail + 4].copy_from_slice(&0u32.to_le_bytes());
    out[tail + 4..tail + 8].copy_from_slice(&(entry.payload.len() as u32).to_le_bytes());
    out[fixed..].copy_from_slice(&entry.payload);

    let identifier = if extended {
        format::TAG_EXTENDED_FILE_ENTRY
    } else {
        format::TAG_FILE_ENTRY
    };
    seal(&mut out, identifier, block);
    volume.write_partition(block, &out);
}

/// A directory: its entry, and its identifiers wherever this shape puts them.
fn write_directory(volume: &mut Volume, at: u32, parent: u32, children: &[Named]) {
    if volume.shape == Shape::Embedded {
        // The identifiers go inside the entry, so their tags name the entry's own
        // block.
        let inside = records(parent, children, at, volume.reference);
        write_entry(
            volume,
            at,
            &Entry {
                kind: format::TYPE_DIRECTORY,
                links: 1,
                length: inside.len() as u64,
                // No block of its own is allocated to it, which is what a volume
                // says about a directory stored this way.
                blocks: 0,
                shape: Shape::Embedded,
                payload: inside,
            },
        );
        return;
    }

    let length = records(parent, children, 0, volume.reference).len() as u64;
    let blocks = length.div_ceil(volume.block_size as u64) as u32;
    let data = volume.claim(blocks);
    let written = records(parent, children, data, volume.reference);
    volume.write_partition(data, &written);
    write_entry(
        volume,
        at,
        &Entry {
            kind: format::TYPE_DIRECTORY,
            links: 1,
            length,
            blocks: u64::from(blocks),
            shape: volume.shape,
            payload: extent(volume.shape, length as u32, data, 0, volume.reference),
        },
    );
}

/// A file of `length` bytes: an entry, and blocks for its bytes.
fn write_file(volume: &mut Volume, length: u64, links: u16) -> u32 {
    let at = volume.claim(1);
    let shape = volume.shape.for_a_file();
    let blocks = length.div_ceil(volume.block_size as u64) as u32;
    let payload = if blocks == 0 {
        // A file of no bytes has no extents at all, which a reader has to cope
        // with — and which is not the same as an extent of no blocks.
        Vec::new()
    } else {
        let data = volume.claim(blocks);
        extent(shape, length as u32, data, 0, volume.reference)
    };
    write_entry(
        volume,
        at,
        &Entry {
            kind: TYPE_FILE,
            links,
            length,
            blocks: u64::from(blocks),
            shape,
            payload,
        },
    );
    at
}

fn write_file_set(volume: &mut Volume, at: u32, root: u32) {
    let mut out = vec![0u8; 512];
    out[16..28].copy_from_slice(&timestamp());
    out[28..30].copy_from_slice(&3u16.to_le_bytes());
    out[30..32].copy_from_slice(&3u16.to_le_bytes());
    out[400..416].copy_from_slice(&long_ad(2048, root, volume.reference, 0));
    out[416..448].copy_from_slice(&regid("*OSTA UDF Compliant", &0x0201u16.to_le_bytes()));
    seal(&mut out, format::TAG_FILE_SET, at);
    volume.write_partition(at, &out);
}

/// One partition map, which is what the tests about indirection vary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mapping {
    Physical,
    Sparable,
    /// A virtual partition, with a table at the end of what was recorded.
    /// `modern` picks which of the two forms the table is written in: UDF 2.00
    /// gave it a file type and a header, and 1.50 had neither.
    Virtual {
        modern: bool,
    },
    /// A metadata partition whose file is at this block of the physical one, plus
    /// the physical map it is built on — so two maps rather than one.
    Metadata(u32),
}

/// A volume descriptor sequence: a partition, a logical volume, a terminator.
fn write_sequence(volume: &mut Volume, at: u32, mapping: Mapping) {
    let block_size = volume.block_size;
    let mut partition = vec![0u8; 512];
    partition[16..20].copy_from_slice(&1u32.to_le_bytes());
    partition[20..22].copy_from_slice(&1u16.to_le_bytes());
    partition[22..24].copy_from_slice(&0u16.to_le_bytes());
    partition[24..56].copy_from_slice(&regid("+NSR03", &[]));
    partition[184..188].copy_from_slice(&4u32.to_le_bytes());
    partition[188..192].copy_from_slice(&PARTITION_START.to_le_bytes());
    partition[192..196].copy_from_slice(&partition_blocks(block_size).to_le_bytes());
    seal(&mut partition, format::TAG_PARTITION, at);
    volume.write_medium(at, &partition);

    // A virtual or metadata partition is built *on* a physical one, so such a
    // volume lists two maps and the interesting one is the second.
    let maps = match mapping {
        Mapping::Metadata(_) | Mapping::Virtual { .. } => 2u32,
        _ => 1,
    };
    let table: Vec<u8> = match mapping {
        Mapping::Physical => physical_map(),
        Mapping::Sparable => type_two("*UDF Sparable Partition Map", 0),
        Mapping::Virtual { .. } => {
            let mut table = physical_map();
            table.extend(type_two("*UDF Virtual Partition Map", 0));
            table
        }
        Mapping::Metadata(file) => {
            let mut table = physical_map();
            table.extend(type_two("*UDF Metadata Partition Map", file));
            table
        }
    };
    // The file set is in the last partition the volume lists, which for a
    // metadata volume is the metadata one — that is the whole arrangement: every
    // directory and file entry lives inside one file.
    let file_set_partition = (maps - 1) as u16;

    let mut logical = vec![0u8; format::LOGICAL_VOLUME_FIXED + table.len()];
    logical[16..20].copy_from_slice(&1u32.to_le_bytes());
    logical[212..216].copy_from_slice(&(block_size as u32).to_le_bytes());
    logical[216..248].copy_from_slice(&regid("*OSTA UDF Compliant", &0x0201u16.to_le_bytes()));
    logical[248..264].copy_from_slice(&long_ad(2048, 0, file_set_partition, 0));
    logical[264..268].copy_from_slice(&(table.len() as u32).to_le_bytes());
    logical[268..272].copy_from_slice(&maps.to_le_bytes());
    logical[format::LOGICAL_VOLUME_FIXED..].copy_from_slice(&table);
    seal(&mut logical, format::TAG_LOGICAL_VOLUME, at + 1);
    volume.write_medium(at + 1, &logical);

    let mut terminator = vec![0u8; 512];
    seal(&mut terminator, format::TAG_TERMINATING, at + 2);
    volume.write_medium(at + 2, &terminator);
}

fn physical_map() -> Vec<u8> {
    let mut out = vec![0u8; 6];
    out[0] = 1;
    out[1] = 6;
    out[2..4].copy_from_slice(&1u16.to_le_bytes());
    out[4..6].copy_from_slice(&0u16.to_le_bytes());
    out
}

/// A type 2 map: sixty-four bytes, of which the identifier is what says which of
/// the three indirections it is.
fn type_two(identifier: &str, file: u32) -> Vec<u8> {
    let mut out = vec![0u8; 64];
    out[0] = 2;
    out[1] = 64;
    out[4..36].copy_from_slice(&regid(identifier, &[]));
    out[36..38].copy_from_slice(&1u16.to_le_bytes());
    // Which physical partition this is built on: partition number 0, which is
    // the one the partition descriptor above declares.
    out[38..40].copy_from_slice(&0u16.to_le_bytes());
    out[40..44].copy_from_slice(&file.to_le_bytes());
    out
}

/// The virtual allocation table, written where a recording stops.
///
/// **The identity, deliberately.** Every other block in this fixture is written
/// at the partition block it was claimed at, so a table mapping virtual *n* to
/// physical *n* leaves the rest of the builder alone and tests the indirection
/// rather than a second layout. It is also what `mkudffs` writes on a freshly
/// formatted disc, which was checked against one.
fn write_vat(volume: &mut Volume, entries: u32, modern: bool) -> u32 {
    // Where the table itself goes: the block after the last one used, addressed
    // physically. **The table is the one thing on such a volume that is not
    // reached through the table**, so its own descriptor holds the block of the
    // partition it is really at.
    let at = volume.offset + entries;
    let mut table: Vec<u8> = Vec::new();
    for block in 0..entries {
        table.extend_from_slice(&(block + volume.offset).to_le_bytes());
    }

    let payload = if modern {
        // UDF 2.00 and later: a header whose own length says where the entries
        // begin, so a later revision could add fields without moving them.
        let header = 152u16;
        let mut out = vec![0u8; header as usize];
        out[0..2].copy_from_slice(&header.to_le_bytes());
        out[2..4].copy_from_slice(&0u16.to_le_bytes());
        out[132..136].copy_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(&table);
        out
    } else {
        // UDF 1.50: the entries, then an identifier saying what they were, then
        // where the previous table was. A reader knows this one by its tail.
        let mut out = table;
        out.extend_from_slice(&regid("*UDF Virtual Alloc Tbl", &0x0150u16.to_le_bytes()));
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out
    };

    // Written through the offset rather than around it, so that `write_entry`'s
    // tag lands on the physical block — which is what this one descriptor's
    // address means.
    let was = volume.offset;
    volume.offset = 0;
    write_entry(
        volume,
        at,
        &Entry {
            // A 1.50 table is a file of no particular type; a 2.x one has a
            // type of its own.
            kind: if modern { format::TYPE_VAT } else { 0 },
            links: 1,
            length: payload.len() as u64,
            blocks: 0,
            shape: Shape::Embedded,
            payload,
        },
    );
    volume.offset = was;
    at
}

fn write_anchor(volume: &mut Volume, at: u32, main: u32, reserve: u32) {
    let bytes = 16 * volume.block_size as u32;
    let mut out = vec![0u8; 512];
    out[16..20].copy_from_slice(&bytes.to_le_bytes());
    out[20..24].copy_from_slice(&main.to_le_bytes());
    out[24..28].copy_from_slice(&bytes.to_le_bytes());
    out[28..32].copy_from_slice(&reserve.to_le_bytes());
    seal(&mut out, format::TAG_ANCHOR, at);
    volume.write_medium(at, &out);
}

/// Where the fixture put things, for a test that wants to damage one of them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub anchor: u32,
    pub tail_anchor: u32,
    pub main: u32,
    pub block_size: usize,
}

pub(crate) fn fixture(shape: Shape) -> Vec<u8> {
    built(shape, BLOCK, Mapping::Physical).0
}

/// A whole volume: the tree above, in the shape asked for.
fn built(shape: Shape, block_size: usize, mapping: Mapping) -> (Vec<u8>, Layout) {
    built_at(shape, block_size, mapping, RECOGNITION)
}

fn built_at(
    shape: Shape,
    block_size: usize,
    mapping: Mapping,
    recognition: u32,
) -> (Vec<u8>, Layout) {
    let mut volume = Volume::new(shape, block_size);
    if matches!(mapping, Mapping::Virtual { .. } | Mapping::Metadata(_)) {
        // Everything in the tree is addressed through the second map, and the
        // blocks it resolves to are eight further along than the numbers used
        // to reach them. Without both of those an indirection fixture is the
        // identity and proves nothing.
        volume.offset = 8;
        volume.reference = 1;
    }

    // The recognition sequence. Nothing here reads it — the anchor is what says
    // there is a volume, and a bridge disc pushes this sequence down the medium
    // anyway — but a real volume has one.
    for (step, magic) in [b"BEA01", b"NSR03", b"TEA01"].into_iter().enumerate() {
        let mut block = vec![0u8; block_size];
        block[1..6].copy_from_slice(magic);
        block[6] = 1;
        volume.write_medium(recognition + step as u32, &block);
    }

    // The file set and the directory entries are claimed first because
    // everything points at them: the logical volume at the file set, the file set
    // at the root.
    let file_set = volume.claim(1);
    let root = volume.claim(1);
    let docs = volume.claim(1);
    let deeper = volume.claim(1);

    let mut root_children = Vec::new();
    for which in [LONG_FILE, README, EMPTY] {
        let (name, length) = TREE[which];
        root_children.push(Named {
            characteristics: 0,
            icb: write_file(&mut volume, length, 1),
            name: Some(name),
        });
    }
    root_children.push(Named {
        characteristics: IS_DIRECTORY,
        icb: docs,
        name: Some(TREE[DOCS].0),
    });

    let unicode = write_file(&mut volume, TREE[UNICODE].1, 1);
    write_directory(
        &mut volume,
        deeper,
        docs,
        &[Named {
            characteristics: 0,
            icb: unicode,
            name: Some(TREE[UNICODE].0),
        }],
    );
    let note = write_file(&mut volume, TREE[NOTE].1, 1);
    write_directory(
        &mut volume,
        docs,
        root,
        &[
            Named {
                characteristics: 0,
                icb: note,
                name: Some(TREE[NOTE].0),
            },
            Named {
                characteristics: IS_DIRECTORY,
                icb: deeper,
                name: Some(TREE[DEEPER].0),
            },
        ],
    );
    write_directory(&mut volume, root, root, &root_children);
    write_file_set(&mut volume, file_set, root);

    // A metadata volume's every structure above is inside the metadata file, so
    // that file is one extent covering the whole partition: the identity, which
    // is the only mapping under which the blocks written above are where they
    // were written. The indirection is what is being tested, not the arithmetic
    // of a partition laid out differently.
    if let Mapping::Metadata(at) = mapping {
        let blocks = partition_blocks(block_size);
        let mut out = vec![0u8; 216 + format::SHORT_AD];
        out[20..22].copy_from_slice(&4u16.to_le_bytes());
        out[24..26].copy_from_slice(&1u16.to_le_bytes());
        // File type 250 is a metadata file, which is a file like any other as far
        // as its extents go.
        out[16 + 11] = 250;
        out[34..36].copy_from_slice(&0u16.to_le_bytes());
        // **Starting at the offset, not at block 0.** A metadata partition laid
        // over the whole of the physical one is the identity, and a fixture like
        // that passes whether or not the reader goes through it.
        let covered = blocks - volume.offset;
        let bytes = covered * block_size as u32;
        out[56..64].copy_from_slice(&u64::from(bytes).to_le_bytes());
        out[72..80].copy_from_slice(&u64::from(covered).to_le_bytes());
        out[208..212].copy_from_slice(&0u32.to_le_bytes());
        out[212..216].copy_from_slice(&(format::SHORT_AD as u32).to_le_bytes());
        out[216..].copy_from_slice(&extent(Shape::Short, bytes, volume.offset, 0, 0));
        seal(&mut out, format::TAG_EXTENDED_FILE_ENTRY, at);
        volume.write_medium(PARTITION_START + at, &out);
    }

    write_sequence(&mut volume, MAIN_SEQUENCE, mapping);
    write_sequence(&mut volume, RESERVE_SEQUENCE, mapping);
    write_anchor(&mut volume, ANCHOR, MAIN_SEQUENCE, RESERVE_SEQUENCE);

    // **A write-once volume is a different shape of image, and the fixture has
    // to be that shape.** Its table is written where the recording stopped, so
    // the image *ends* there — there is no anchor at the end of the medium,
    // because there is no end of the medium yet, and the blocks after the table
    // were never written. Cutting the fixture there is exactly what reading such
    // a disc produces, and it is what the volume `mkudffs` wrote for this looks
    // like once the zeros it padded a pre-sized file with are gone.
    if let Mapping::Virtual { modern } = mapping {
        let entries = volume.next;
        let vat = write_vat(&mut volume, entries, modern);
        let end = (PARTITION_START + vat + 1) as usize * block_size;
        volume.bytes.truncate(end);
        let layout = Layout {
            anchor: ANCHOR,
            tail_anchor: PARTITION_START + vat,
            main: MAIN_SEQUENCE,
            block_size,
        };
        return (volume.bytes, layout);
    }

    write_anchor(
        &mut volume,
        medium_blocks(block_size) - 1,
        MAIN_SEQUENCE,
        RESERVE_SEQUENCE,
    );

    let layout = Layout {
        anchor: ANCHOR,
        tail_anchor: medium_blocks(block_size) - 1,
        main: MAIN_SEQUENCE,
        block_size,
    };
    (volume.bytes, layout)
}

// ------------------------------------------------------------------- the walk

fn tree_of(bytes: &[u8], options: &ScanOptions) -> crate::SizeTree {
    let mut fs = Filesystem::open(bytes).expect("a volume this reader can open");
    let start = super::build::root(&fs);
    super::build::build(
        &mut fs,
        Path::new("volume"),
        start,
        options,
        |_: Progress| {},
        || false,
    )
    .expect("a walk that finishes")
}

fn shape_of(tree: &crate::SizeTree) -> Vec<String> {
    let mut rows: Vec<String> = (0..tree.len())
        .map(crate::NodeId::from_index)
        .filter(|&id| id != tree.root())
        .map(|id| {
            let node = tree.node(id);
            format!(
                "{} {} {} {}",
                tree.name_lossy(id),
                node.logical,
                node.allocated,
                if node.is_dir() { "dir" } else { "file" }
            )
        })
        .collect();
    rows.sort();
    rows
}

/// What every shape has to produce, for a 2048-byte volume.
///
/// A directory's `allocated` is the whole of what is under it, so `docs` is its
/// own block plus everything in it — which is why the two block sizes cannot
/// share one expectation.
fn expected(block_size: usize) -> Vec<String> {
    let block = block_size as u64;
    let round = |length: u64| length.div_ceil(block) * block;
    let deeper = round(TREE[UNICODE].1) + block;
    let docs = round(TREE[NOTE].1) + deeper + block;
    let mut rows = vec![
        format!(
            "{} {} {} file",
            TREE[LONG_FILE].0,
            TREE[LONG_FILE].1,
            round(TREE[LONG_FILE].1)
        ),
        format!(
            "{} {} {} file",
            TREE[README].0,
            TREE[README].1,
            round(TREE[README].1)
        ),
        format!("{} 0 0 file", TREE[EMPTY].0),
        format!(
            "{} {} {} dir",
            TREE[DOCS].0,
            TREE[NOTE].1 + TREE[UNICODE].1,
            docs
        ),
        format!("{} {} {} dir", TREE[DEEPER].0, TREE[UNICODE].1, deeper),
        format!(
            "{} {} {} file",
            TREE[UNICODE].0,
            TREE[UNICODE].1,
            round(TREE[UNICODE].1)
        ),
        format!(
            "{} {} {} file",
            TREE[NOTE].0,
            TREE[NOTE].1,
            round(TREE[NOTE].1)
        ),
    ];
    rows.sort();
    rows
}

/// The one for a volume whose directories live inside their own entries, where a
/// directory occupies no block at all and its length is what it holds.
fn expected_embedded(block_size: usize) -> Vec<String> {
    let block = block_size as u64;
    let round = |length: u64| length.div_ceil(block) * block;
    // The identifiers of each directory, which is what an embedded one occupies.
    let inside = |children: &[Named]| records(0, children, 0, 0).len() as u64;
    let deeper_inside = inside(&[Named {
        characteristics: 0,
        icb: 0,
        name: Some(TREE[UNICODE].0),
    }]);
    let docs_inside = inside(&[
        Named {
            characteristics: 0,
            icb: 0,
            name: Some(TREE[NOTE].0),
        },
        Named {
            characteristics: IS_DIRECTORY,
            icb: 0,
            name: Some(TREE[DEEPER].0),
        },
    ]);
    let deeper = round(TREE[UNICODE].1) + deeper_inside;
    let docs = round(TREE[NOTE].1) + deeper + docs_inside;
    let mut rows = vec![
        format!(
            "{} {} {} file",
            TREE[LONG_FILE].0,
            TREE[LONG_FILE].1,
            round(TREE[LONG_FILE].1)
        ),
        format!(
            "{} {} {} file",
            TREE[README].0,
            TREE[README].1,
            round(TREE[README].1)
        ),
        format!("{} 0 0 file", TREE[EMPTY].0),
        format!(
            "{} {} {} dir",
            TREE[DOCS].0,
            TREE[NOTE].1 + TREE[UNICODE].1,
            docs
        ),
        format!("{} {} {} dir", TREE[DEEPER].0, TREE[UNICODE].1, deeper),
        format!(
            "{} {} {} file",
            TREE[UNICODE].0,
            TREE[UNICODE].1,
            round(TREE[UNICODE].1)
        ),
        format!(
            "{} {} {} file",
            TREE[NOTE].0,
            TREE[NOTE].1,
            round(TREE[NOTE].1)
        ),
    ];
    rows.sort();
    rows
}

// ------------------------------------------------- a volume of one directory

/// A volume under construction with its file set and root claimed, for a test
/// about one thing at a time.
///
/// The tree above is the right fixture for *is this read correctly*; it is the
/// wrong one for *what happens when a directory is in two extents*, where every
/// name in it is noise. This builds a volume holding exactly what a test asks
/// for, out of the same writers.
struct Small {
    volume: Volume,
    file_set: u32,
    root: u32,
}

fn small(shape: Shape) -> Small {
    let mut volume = Volume::new(shape, BLOCK);
    let file_set = volume.claim(1);
    let root = volume.claim(1);
    Small {
        volume,
        file_set,
        root,
    }
}

/// Close a small volume: its root directory, its file set, its descriptors and
/// its anchors.
fn finish(mut small: Small, children: &[Named]) -> Vec<u8> {
    write_directory(&mut small.volume, small.root, small.root, children);
    close(small)
}

/// The same, for a test that wrote the root's entry itself.
fn close(mut small: Small) -> Vec<u8> {
    let root = small.root;
    let file_set = small.file_set;
    write_file_set(&mut small.volume, file_set, root);
    write_sequence(&mut small.volume, MAIN_SEQUENCE, Mapping::Physical);
    write_sequence(&mut small.volume, RESERVE_SEQUENCE, Mapping::Physical);
    write_anchor(&mut small.volume, ANCHOR, MAIN_SEQUENCE, RESERVE_SEQUENCE);
    write_anchor(
        &mut small.volume,
        medium_blocks(BLOCK) - 1,
        MAIN_SEQUENCE,
        RESERVE_SEQUENCE,
    );
    small.volume.bytes
}

fn names_of(tree: &crate::SizeTree) -> Vec<String> {
    let mut rows: Vec<String> = (0..tree.len())
        .map(crate::NodeId::from_index)
        .filter(|&id| id != tree.root())
        .map(|id| tree.name_lossy(id).to_string())
        .collect();
    rows.sort();
    rows
}

fn node_named(tree: &crate::SizeTree, name: &str) -> crate::Node {
    let id = (0..tree.len())
        .map(crate::NodeId::from_index)
        .find(|&id| tree.name_lossy(id) == name)
        .unwrap_or_else(|| panic!("no entry named {name}"));
    *tree.node(id)
}

// ----------------------------------------------------------------- the bytes

/// The CRC in the anchor of a volume `mkudffs` wrote is 0x9317 over the 496
/// bytes after its tag.
///
/// **Checked against values rather than against a second implementation of the
/// same idea.** There are several CRC-16s and they differ only in whether the
/// bits are reversed and what the register starts at, so a wrong one still
/// produces a plausible number for every input — and a reader with the wrong one
/// refuses every volume there is.
#[test]
fn the_crc_is_the_one_ecma_167_asks_for() {
    assert_eq!(format::crc16(&[]), 0x0000);
    // The polynomial's defining property: one byte of 0x01 shifts out as 0x1021.
    assert_eq!(format::crc16(&[0x01]), 0x1021);
    // The worked example in the standard's own appendix, which is there for
    // exactly this reason.
    assert_eq!(format::crc16(b"\x70\x6A\x77"), 0x3299);
}

/// Every one of a tag's checks refuses a descriptor that fails it.
///
/// **The location check is the one that earns its keep.** A wrong checksum or CRC
/// means damage, which is rare. A descriptor at the wrong block means the reader
/// computed an offset in the wrong units — partition blocks for medium blocks, or
/// the other way round — which is the mistake that is actually made, and which
/// has no symptom at all without this.
#[test]
fn a_tag_is_refused_by_every_check_it_carries() {
    let mut anchor = vec![0u8; 512];
    anchor[16..20].copy_from_slice(&32768u32.to_le_bytes());
    anchor[20..24].copy_from_slice(&32u32.to_le_bytes());
    seal(&mut anchor, format::TAG_ANCHOR, 256);
    assert!(format::anchor(&anchor, 256).is_some(), "the good one");

    assert!(
        format::anchor(&anchor, 257).is_none(),
        "an anchor read as the wrong block was accepted"
    );

    let mut damaged = anchor.clone();
    damaged[4] ^= 1;
    assert!(format::anchor(&damaged, 256).is_none(), "the checksum");

    // A byte the checksum does not cover, which only the CRC can catch.
    let mut damaged = anchor.clone();
    damaged[100] ^= 1;
    assert!(format::anchor(&damaged, 256).is_none(), "the CRC");

    // A real descriptor of the wrong kind, which is what reading a volume
    // sequence in the wrong order finds.
    let mut other = anchor.clone();
    seal(&mut other, format::TAG_PARTITION, 256);
    assert!(format::anchor(&other, 256).is_none(), "the identifier");

    // Descriptor version 1, which nothing has written this century.
    let mut ancient = anchor.clone();
    ancient[2] = 1;
    let sum = ancient[..16]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
    ancient[4] = sum;
    assert!(format::anchor(&ancient, 256).is_none(), "the version");
}

/// A revision is binary coded decimal, so reading it as a number gets 2.80 for
/// the one Blu-ray uses.
#[test]
fn a_revision_is_spelt_the_way_its_specification_numbers_it() {
    assert_eq!(format::revision(0x0102), "1.02");
    assert_eq!(format::revision(0x0150), "1.50");
    assert_eq!(format::revision(0x0201), "2.01");
    assert_eq!(format::revision(0x0250), "2.50");
    assert_eq!(format::revision(0x0260), "2.60");
}

/// Both compressions, and a name that needs the second.
#[test]
fn a_name_comes_out_of_either_compression() {
    let mut out = Vec::new();
    assert!(format::push_name(&cs0("README.TXT"), &mut out));
    assert_eq!(String::from_utf8(out).unwrap(), "README.TXT");

    // One byte per character means the code point *is* the byte: Latin-1, which
    // is neither UTF-8 nor any code page.
    let mut out = Vec::new();
    assert!(format::push_name(&[8, 0xFC], &mut out));
    assert_eq!(String::from_utf8(out).unwrap(), "\u{FC}");

    let mut out = Vec::new();
    assert!(format::push_name(
        &cs0("Gr\u{FC}\u{DF}e \u{65E5}\u{672C}.txt"),
        &mut out
    ));
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Gr\u{FC}\u{DF}e \u{65E5}\u{672C}.txt"
    );

    // A character outside the basic plane is two code units and one character.
    let mut out = Vec::new();
    assert!(format::push_name(&cs0("a\u{1F642}b"), &mut out));
    assert_eq!(String::from_utf8(out).unwrap(), "a\u{1F642}b");
}

/// A compression identifier UDF does not define is refused rather than guessed
/// at, because a name invented out of the wrong bytes looks like a name.
#[test]
fn a_name_in_an_encoding_udf_does_not_define_is_refused() {
    let mut out = Vec::new();
    assert!(!format::push_name(&[9, b'h', b'i'], &mut out));
    assert!(!format::push_name(&[], &mut out));
    assert!(out.is_empty());
}

/// An unpaired surrogate is a replacement character rather than a refusal: the
/// rest of the name is real.
#[test]
fn an_unpaired_surrogate_does_not_lose_the_rest_of_the_name() {
    let mut recorded = vec![16u8];
    recorded.extend_from_slice(&0xD800u16.to_be_bytes());
    recorded.extend_from_slice(&u16::from(b'x').to_be_bytes());
    let mut out = Vec::new();
    assert!(format::push_name(&recorded, &mut out));
    assert_eq!(String::from_utf8(out).unwrap(), "\u{FFFD}x");
}

/// The recorded time is local and the offset is beside it, so the offset comes
/// off. Getting this wrong is a date out by hours, which looks like a date.
#[test]
fn the_timezone_is_taken_off_the_recorded_time() {
    assert_eq!(format::timestamp(&timestamp()), WHEN as u32);

    // The same fields with no timezone given, which is 2047 rather than zero.
    let mut none = timestamp();
    none[0..2].copy_from_slice(&((1u16 << 12) | 2047).to_le_bytes());
    assert_eq!(
        format::timestamp(&none),
        WHEN as u32 + (ZONE_MINUTES * 60) as u32,
        "an offset nobody gave was taken off anyway"
    );

    // And west of Greenwich, where the offset is negative and so adds.
    let mut west = timestamp();
    let minutes = -300i32;
    west[0..2].copy_from_slice(&((1u16 << 12) | (minutes as u16 & 0x0FFF)).to_le_bytes());
    assert_eq!(
        format::timestamp(&west),
        (WHEN + i64::from(ZONE_MINUTES - minutes) * 60) as u32
    );
}

/// A date that is not one is no date, rather than a date in the wrong month.
#[test]
fn a_date_that_is_not_one_is_no_date() {
    let mut february = timestamp();
    february[4] = 2;
    february[5] = 31;
    assert_eq!(format::timestamp(&february), 0);

    let mut thirteen = timestamp();
    thirteen[4] = 13;
    assert_eq!(format::timestamp(&thirteen), 0);

    assert_eq!(format::timestamp(&[0u8; 12]), 0);
    assert_eq!(format::timestamp(&[0u8; 4]), 0);
}

// ------------------------------------------------------------------ the walk

/// All four arrangements, one expectation for the sizes.
///
/// **What is on a volume cannot depend on which of the standard's arrangements
/// its author chose.** The embedded one differs in exactly one number and for a
/// real reason: a directory stored inside its own entry has no block allocated to
/// it, so what it occupies is its length — the rule a resident NTFS attribute
/// gets, and the reason it has its own expectation rather than a fudge in the
/// shared one.
#[test]
fn every_shape_walks_into_the_same_tree() {
    for shape in [Shape::Short, Shape::Long, Shape::Extended] {
        let tree = tree_of(&fixture(shape), &ScanOptions::default());
        assert_eq!(shape_of(&tree), expected(BLOCK), "{shape:?}");
        assert!(!tree.stats().partial, "{shape:?} reported a partial tree");
        assert_eq!(tree.stats().files, 5, "{shape:?}");
        assert_eq!(tree.stats().dirs, 3, "{shape:?}");
    }
    let tree = tree_of(&fixture(Shape::Embedded), &ScanOptions::default());
    assert_eq!(shape_of(&tree), expected_embedded(BLOCK));
    assert!(!tree.stats().partial);
}

/// Every date on the tree is the instant the volume recorded, offset and all.
#[test]
fn the_dates_on_the_tree_are_the_ones_the_volume_recorded() {
    let tree = tree_of(&fixture(Shape::Embedded), &ScanOptions::default());
    for name in [TREE[README].0, TREE[DOCS].0, TREE[UNICODE].0] {
        assert_eq!(node_named(&tree, name).mtime, WHEN as u32, "{name}");
    }
}

/// The anchor is at logical block 256 whatever a logical block turns out to be,
/// so finding it is also how the block size is discovered.
#[test]
fn a_volume_of_five_hundred_and_twelve_byte_blocks_is_found_and_read() {
    let (bytes, layout) = built(Shape::Short, 512, Mapping::Physical);
    assert_eq!(layout.block_size, 512);
    let mut fs = Filesystem::open(bytes.as_slice()).expect("a 512-byte volume");
    assert_eq!(fs.volume().block_size, 512);
    let start = super::build::root(&fs);
    let tree = super::build::build(
        &mut fs,
        Path::new("volume"),
        start,
        &ScanOptions::default(),
        |_: Progress| {},
        || false,
    )
    .expect("a walk that finishes");
    assert_eq!(shape_of(&tree), expected(512));
}

/// A spare copy of the descriptors is the whole reason UDF writes two, and a
/// volume whose main sequence was half written is what it is for.
#[test]
fn the_reserve_sequence_is_used_when_the_main_one_is_not_there() {
    let (mut bytes, layout) = built(Shape::Short, BLOCK, Mapping::Physical);
    // The main sequence's logical volume descriptor, which is the one thing in it
    // a reader cannot do without.
    let at = (layout.main + 1) as usize * BLOCK;
    bytes[at + 100] ^= 0xFF;

    let tree = tree_of(&bytes, &ScanOptions::default());
    assert_eq!(shape_of(&tree), expected(BLOCK), "the reserve was not used");
}

/// And the anchor at the end of the medium, for the same reason: these media fail
/// at their edges.
#[test]
fn the_anchor_at_the_end_is_used_when_the_first_one_is_gone() {
    let (mut bytes, layout) = built(Shape::Short, BLOCK, Mapping::Physical);
    let at = layout.anchor as usize * BLOCK;
    bytes[at..at + 512].fill(0);

    let tree = tree_of(&bytes, &ScanOptions::default());
    assert_eq!(shape_of(&tree), expected(BLOCK));

    // With both gone there is no volume, which is a refusal rather than a panic or
    // an empty tree.
    let tail = layout.tail_anchor as usize * BLOCK;
    bytes[tail..tail + 512].fill(0);
    assert!(Filesystem::open(bytes.as_slice()).is_err());
}

/// A write-once disc, where every block number goes through a table written
/// where the recording stopped.
///
/// **Both forms of the table**, because UDF 2.00 gave it a file type and a
/// header and 1.50 had neither — so the older one is recognised by an identifier
/// *after* its entries and the newer by one in front of them. A reader that does
/// one of the two reads half the discs of this kind.
#[test]
fn a_write_once_volume_is_read_through_its_table() {
    for modern in [false, true] {
        let (bytes, _) = built(Shape::Short, BLOCK, Mapping::Virtual { modern });
        let fs = Filesystem::open(bytes.as_slice())
            .unwrap_or_else(|error| panic!("modern={modern}: {error}"));
        assert_eq!(fs.in_a_note(), "revision 2.01, virtual partition");
        drop(fs);
        assert_eq!(
            shape_of(&tree_of(&bytes, &ScanOptions::default())),
            expected(BLOCK)
        );
    }
}

/// And an image of such a disc padded out to the size of the medium is refused
/// by name, because there is then nothing to say where the recording stopped.
///
/// **Measured on a real volume rather than reasoned about.** `mkudffs` writing a
/// write-once volume into a pre-sized file leaves exactly this: a perfectly good
/// table at block 299 and thirteen thousand blocks of zeros after it. Cut the
/// file where the recording ended — which is what an image of the disc would be
/// — and the same bytes read.
#[test]
fn a_write_once_volume_padded_past_its_recording_is_refused() {
    let (bytes, _) = built(Shape::Short, BLOCK, Mapping::Virtual { modern: false });
    let mut padded = bytes.clone();
    padded.resize(bytes.len() + 4096 * BLOCK, 0);

    let Err(error) = Filesystem::open(padded.as_slice()) else {
        panic!("a volume kosha cannot address was opened");
    };
    assert_eq!(
        error
            .get_ref()
            .and_then(|reason| reason.downcast_ref::<Unreadable>()),
        Some(&Unreadable::Unmapped)
    );
}

/// A rewritable disc's list of replaced blocks is not read, so a volume with one
/// says so on the tree: a total from a disc that has had a block replaced is not
/// quietly exact.
#[test]
fn a_sparable_partition_is_read_and_the_note_says_so() {
    let (bytes, _) = built(Shape::Short, BLOCK, Mapping::Sparable);
    let fs = Filesystem::open(bytes.as_slice()).expect("a sparable volume is readable");
    assert_eq!(fs.in_a_note(), "revision 2.01, sparable partition");
}

/// A partition whose contents are a file's contents, which is what a Blu-ray disc
/// has and what Windows writes for UDF 2.50.
#[test]
fn a_metadata_partition_is_one_more_indirection_and_is_followed() {
    // The metadata file's own entry goes at a block of the physical partition that
    // nothing else claimed.
    let (bytes, _) = built(Shape::Short, BLOCK, Mapping::Metadata(120));
    let fs = Filesystem::open(bytes.as_slice()).expect("a metadata volume");
    assert_eq!(fs.in_a_note(), "revision 2.01, metadata partition");
    drop(fs);
    let tree = tree_of(&bytes, &ScanOptions::default());
    assert_eq!(shape_of(&tree), expected(BLOCK));
}

/// A name whose file is gone. UDF leaves the entry in place and marks it, because
/// on a medium that cannot be rewritten that is the only way to delete anything.
#[test]
fn a_deleted_entry_is_not_in_the_tree() {
    let mut small = small(Shape::Embedded);
    let kept = write_file(&mut small.volume, 4096, 1);
    let children = [
        Named {
            characteristics: 0,
            icb: kept,
            name: Some("KEPT.TXT"),
        },
        Named {
            // Pointing nowhere on purpose: if this were followed rather than
            // skipped, the read would fail and the tree would say so.
            characteristics: IS_DELETED,
            icb: partition_blocks(BLOCK) + 100,
            name: Some("GONE.TXT"),
        },
    ];
    let tree = tree_of(&finish(small, &children), &ScanOptions::default());
    assert_eq!(names_of(&tree), vec!["KEPT.TXT".to_string()]);
    assert!(!tree.stats().partial);
}

/// Two names for one file. The bytes are on the volume once and are counted once.
#[test]
fn two_names_for_one_file_count_its_bytes_once() {
    let mut small = small(Shape::Embedded);
    let shared = write_file(&mut small.volume, 4096, 2);
    let children = [
        Named {
            characteristics: 0,
            icb: shared,
            name: Some("one.bin"),
        },
        Named {
            characteristics: 0,
            icb: shared,
            name: Some("two.bin"),
        },
    ];
    let bytes = finish(small, &children);

    // **The default, on both platforms.** `dedup_hardlinks` used to be
    // `cfg!(unix)`, because on a *walk* it costs an open per file on Windows — a
    // reason that has nothing to do with reading an image, where the link count
    // is a field in an entry that has already been read. It is `None` now, and
    // every reader that has the answer already acts on it.
    let tree = tree_of(&bytes, &ScanOptions::default());
    assert_eq!(tree.stats().hardlinks_deduped, 1);
    assert_eq!(
        node_named(&tree, "one.bin").allocated + node_named(&tree, "two.bin").allocated,
        4096,
        "one of the two names carries the bytes"
    );

    // With the option off, both names carry them — which is what somebody asking
    // what a directory would cost to copy wants.
    let tree = tree_of(
        &bytes,
        &ScanOptions {
            dedup_hardlinks: Some(false),
            ..ScanOptions::default()
        },
    );
    assert_eq!(tree.stats().hardlinks_deduped, 0);
    assert_eq!(node_named(&tree, "one.bin").allocated, 4096);
    assert_eq!(node_named(&tree, "two.bin").allocated, 4096);
}

/// An extent may be allocated and never written, which is what a file with a hole
/// in it is on a rewritable volume.
#[test]
fn a_file_with_fewer_blocks_than_its_length_is_sparse() {
    let mut small = small(Shape::Embedded);
    let at = small.volume.claim(1);
    let data = small.volume.claim(3);
    let mut payload = extent(Shape::Short, BLOCK as u32, data, 0, 0);
    // Two more extents that are allocated and were never written: a hole.
    payload.extend(extent(Shape::Short, 2 * BLOCK as u32, data + 1, 1, 0));
    write_entry(
        &mut small.volume,
        at,
        &Entry {
            kind: TYPE_FILE,
            links: 1,
            length: 3 * BLOCK as u64,
            // One block recorded of the three the length covers. A file one block
            // short of its length is not sparse but rounded, which is why the
            // hole here is two.
            blocks: 1,
            shape: Shape::Short,
            payload,
        },
    );
    let children = [Named {
        characteristics: 0,
        icb: at,
        name: Some("holes.bin"),
    }];
    let tree = tree_of(&finish(small, &children), &ScanOptions::default());
    let node = node_named(&tree, "holes.bin");
    assert_eq!(node.logical, 3 * BLOCK as u64);
    assert_eq!(node.allocated, BLOCK as u64);
    assert!(node.flags.contains(crate::Flags::SPARSE));
}

/// Enough names to fill more than one block of records.
const NAMES: &[&str] = &[
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "cccccccccccccccccccccccccccccccccccccccc",
    "dddddddddddddddddddddddddddddddddddddddd",
    "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    "ffffffffffffffffffffffffffffffffffffffff",
    "gggggggggggggggggggggggggggggggggggggggg",
    "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh",
    "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii",
    "jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj",
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "llllllllllllllllllllllllllllllllllllllll",
    "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
    "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn",
    "oooooooooooooooooooooooooooooooooooooooo",
    "pppppppppppppppppppppppppppppppppppppppp",
    "qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq",
    "rrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr",
    "ssssssssssssssssssssssssssssssssssssssss",
    "tttttttttttttttttttttttttttttttttttttttt",
    "uuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu",
    "vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv",
    "wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy",
    "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
];

/// Split a directory's records at a block boundary, building each half at the
/// block it will live in.
///
/// **The split falls wherever it falls, including inside a record.** A
/// directory's data is one byte stream and its extents are windows onto that
/// stream, so the record at the boundary begins in the first extent and ends in
/// the second. There is no padding to push it clear: the Linux driver carries
/// code to reassemble exactly this, which is how one knows it happens.
fn split_records(parent: u32, children: &[Named], first: u32, second: u32) -> (Vec<u8>, Vec<u8>) {
    let mut stream = fid(
        &Named {
            characteristics: IS_PARENT | IS_DIRECTORY,
            icb: parent,
            name: None,
        },
        first,
        0,
    );
    for child in children {
        // A descriptor's tag names the block it *starts* in, so which of the two
        // extents a record belongs to is decided by where it begins — and one of
        // them begins in the first and ends in the second.
        let block = if stream.len() < BLOCK { first } else { second };
        stream.extend_from_slice(&fid(child, block, 0));
    }
    assert!(
        stream.len() > BLOCK,
        "nothing was left for the second extent"
    );
    let tail = stream.split_off(BLOCK);
    (stream, tail)
}

/// A directory with more extents than fit in its entry carries on in an allocation
/// extent descriptor, which is a read this reader would otherwise never take.
#[test]
fn a_directory_whose_descriptors_continue_elsewhere_is_read_whole() {
    let mut small = small(Shape::Short);
    let root = small.root;
    let mut children = Vec::new();
    for name in NAMES {
        let icb = write_file(&mut small.volume, BLOCK as u64, 1);
        children.push(Named {
            characteristics: 0,
            icb,
            name: Some(name),
        });
    }

    let first = small.volume.claim(1);
    let second = small.volume.claim(1);
    let continued = small.volume.claim(1);
    let (head, tail) = split_records(root, &children, first, second);
    small.volume.write_partition(first, &head);
    small.volume.write_partition(second, &tail);

    // Two extents, and the descriptor after them says the rest are over there.
    let mut payload = extent(Shape::Short, BLOCK as u32, first, 0, 0);
    payload.extend(extent(Shape::Short, BLOCK as u32, continued, 3, 0));
    write_entry(
        &mut small.volume,
        root,
        &Entry {
            kind: format::TYPE_DIRECTORY,
            links: 1,
            length: (BLOCK + tail.len()) as u64,
            blocks: 2,
            shape: Shape::Short,
            payload,
        },
    );

    // The allocation extent descriptor, holding what did not fit.
    let mut aed = vec![0u8; 24 + format::SHORT_AD];
    aed[20..24].copy_from_slice(&(format::SHORT_AD as u32).to_le_bytes());
    aed[24..].copy_from_slice(&extent(Shape::Short, tail.len() as u32, second, 0, 0));
    seal(&mut aed, format::TAG_ALLOCATION_EXTENT, continued);
    small.volume.write_partition(continued, &aed);

    let tree = tree_of(&close(small), &ScanOptions::default());
    let mut wanted: Vec<String> = NAMES.iter().map(|one| one.to_string()).collect();
    wanted.sort();
    assert_eq!(names_of(&tree), wanted);
    assert!(!tree.stats().partial);
}

/// Strategy 4096 writes an indirect entry where the file entry would be and puts
/// the entry somewhere else. A reader that stopped at the first descriptor would
/// see no file at all.
#[test]
fn an_indirect_entry_is_followed_to_the_real_one() {
    let mut small = small(Shape::Embedded);
    let real = write_file(&mut small.volume, 4096, 1);
    let pointer = small.volume.claim(1);
    let mut indirect = vec![0u8; 52];
    indirect[20..22].copy_from_slice(&4096u16.to_le_bytes());
    indirect[24..26].copy_from_slice(&1u16.to_le_bytes());
    indirect[36..52].copy_from_slice(&long_ad(2048, real, 0, 0));
    seal(&mut indirect, format::TAG_INDIRECT, pointer);
    small.volume.write_partition(pointer, &indirect);

    let children = [Named {
        characteristics: 0,
        icb: pointer,
        name: Some("indirect.bin"),
    }];
    let tree = tree_of(&finish(small, &children), &ScanOptions::default());
    assert_eq!(node_named(&tree, "indirect.bin").allocated, 4096);
    assert!(!tree.stats().partial);
}

/// A depth limit stops where it says, and what is above it is still right.
#[test]
fn a_depth_limit_stops_the_walk_where_it_says() {
    let tree = tree_of(
        &fixture(Shape::Embedded),
        &ScanOptions {
            max_depth: Some(1),
            ..ScanOptions::default()
        },
    );
    assert_eq!(
        names_of(&tree),
        vec![
            "A long file name.bin".to_string(),
            "EMPTY.BIN".to_string(),
            "README.TXT".to_string(),
            "docs".to_string(),
        ]
    );
}

/// A scan pointed inside the volume starts there, which is a handful of directory
/// reads rather than a walk of the whole thing.
#[test]
fn a_path_inside_a_volume_resolves_to_the_directory_it_names() {
    let bytes = fixture(Shape::Embedded);
    let mut fs = Filesystem::open(bytes.as_slice()).expect("a volume");
    let inside = super::build::resolve(&mut fs, Path::new("docs/deeper"))
        .expect("a resolve that finishes")
        .expect("a directory that is there");
    let tree = super::build::build(
        &mut fs,
        Path::new("deeper"),
        inside,
        &ScanOptions::default(),
        |_: Progress| {},
        || false,
    )
    .expect("a walk");
    assert_eq!(names_of(&tree), vec![TREE[UNICODE].0.to_string()]);

    // A path that is not there is not there, which is not an error.
    assert!(
        super::build::resolve(&mut fs, Path::new("docs/missing"))
            .expect("a resolve that finishes")
            .is_none()
    );
    // Case matters, unlike on an ISO 9660 disc: UDF has one spelling of every
    // name, so a loose match would be a way of finding the wrong directory.
    assert!(
        super::build::resolve(&mut fs, Path::new("DOCS"))
            .expect("a resolve that finishes")
            .is_none()
    );
}

// ----------------------------------------------------------- what damage does

/// Every prefix of a volume produces a tree or an error, and never a panic.
///
/// **An image that stops in the middle is the ordinary way one is damaged**: a
/// copy that ran out of room, a download that was interrupted. Every offset in
/// this reader is computed from numbers on the volume, so this is the sweep that
/// says the arithmetic is bounded rather than lucky.
#[test]
fn every_truncation_produces_a_tree_or_an_error() {
    let bytes = fixture(Shape::Short);
    let mut at = 0usize;
    let mut step = 1usize;
    while at < bytes.len() {
        if let Ok(mut fs) = Filesystem::open(&bytes[..at]) {
            let start = super::build::root(&fs);
            let _ = super::build::build(
                &mut fs,
                Path::new("volume"),
                start,
                &ScanOptions::default(),
                |_: Progress| {},
                || false,
            );
        }
        at += step;
        // Every byte of the first block, then coarser: a sweep of every byte of a
        // volume this size would be a minute of a suite that runs in seconds.
        if at > BLOCK {
            step = 97;
        }
    }
}

/// Every flipped bit in the volume's structures produces a tree or an error.
///
/// Over the blocks that hold descriptors rather than the whole medium: a bit in a
/// file's data is read by nothing, and sweeping it would be a long test of
/// nothing.
#[test]
fn every_flipped_bit_produces_a_tree_or_an_error() {
    let (bytes, layout) = built(Shape::Short, BLOCK, Mapping::Physical);
    let partition = PARTITION_START as usize * BLOCK;
    let ranges = [
        layout.anchor as usize * BLOCK..(layout.anchor as usize + 1) * BLOCK,
        layout.main as usize * BLOCK..(layout.main as usize + 3) * BLOCK,
        // The file set, the root, and the entries under it.
        partition..partition + 8 * BLOCK,
    ];

    for range in ranges {
        for at in range.step_by(7) {
            for bit in [0u8, 3, 7] {
                let mut damaged = bytes.clone();
                damaged[at] ^= 1 << bit;
                if let Ok(mut fs) = Filesystem::open(damaged.as_slice()) {
                    let start = super::build::root(&fs);
                    let _ = super::build::build(
                        &mut fs,
                        Path::new("volume"),
                        start,
                        &ScanOptions::default(),
                        |_: Progress| {},
                        || false,
                    );
                }
            }
        }
    }
}

// ------------------------------------------------------------ a bridge disc

/// A disc carrying an ISO 9660 filesystem *and* a UDF volume over the same
/// medium, which is what a Windows installer is and what `mkisofs -udf` writes.
///
/// **Two true answers, and which one a scan means is a decision.** The same
/// bytes are honestly a disc and a volume; the ISO 9660 tree is there so that
/// anything unable to read UDF can see something, and it cannot describe a file
/// over four gigabytes in one extent — which on an installer is the file
/// somebody is scanning for. So UDF wins, and the test below damages the UDF
/// volume to show that the other answer was really available all along.
mod on_a_bridge_disc {
    use super::*;

    /// The UDF fixture with an ISO 9660 filesystem laid into the blocks it
    /// leaves free at the front, and its own recognition sequence pushed to 19 —
    /// where a real bridge disc has it, because blocks 16 to 18 are ISO 9660's.
    fn bridge() -> (Vec<u8>, Layout) {
        let (mut bytes, layout) = built_at(Shape::Short, BLOCK, Mapping::Physical, 19);
        let disc = crate::iso::tests::fixture(crate::iso::tests::Dialect::RockRidge);
        assert!(
            disc.len() <= MAIN_SEQUENCE as usize * BLOCK,
            "the disc would land on the volume's descriptors"
        );
        bytes[..disc.len()].copy_from_slice(&disc);
        (bytes, layout)
    }

    fn scan_image(disk: &[u8]) -> Result<crate::SizeTree, crate::ScanError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installer.iso");
        std::fs::write(&path, disk).unwrap();
        crate::Scanner::new(ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        })
        .scan(&path)
    }

    #[test]
    fn the_volume_is_what_a_scan_of_the_disc_means() {
        let (disk, _) = bridge();
        let tree = scan_image(&disk).expect("a bridge disc is an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("UDF · revision 2.01"),
            "the volume is the answer, not the compatibility copy beside it"
        );
    }

    /// And the ISO 9660 filesystem really is there, and really would have been
    /// read. Damaging the volume's anchors is the only way to show the order
    /// above is a decision rather than an accident of what the fixture holds.
    #[test]
    fn the_iso_9660_filesystem_is_what_is_left_when_the_volume_is_not_readable() {
        let (mut disk, layout) = bridge();
        for anchor in [layout.anchor, layout.tail_anchor] {
            let at = anchor as usize * BLOCK;
            disk[at..at + 512].fill(0);
        }

        let tree = scan_image(&disk).expect("a disc is still an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("ISO 9660 · Rock Ridge"),
            "the disc is the fallback, and it is a real one"
        );
    }

    /// A volume whose anchor is found and whose blocks cannot be addressed says
    /// so, rather than falling back to the older tree beside it.
    ///
    /// **Falling back would be the wrong kindness.** The two trees do not hold
    /// the same thing on a real disc, so quietly answering from the one that was
    /// not asked for would produce a total that is smaller than the disc for a
    /// reason nobody was told. Being told *this is UDF and kosha cannot follow
    /// it* is a fact somebody can act on.
    #[test]
    fn a_volume_that_cannot_be_addressed_is_named_rather_than_stepped_over() {
        let (mut bytes, _) = built_at(Shape::Short, BLOCK, Mapping::Virtual { modern: false }, 19);
        // Padded past where the recording stopped, so the table cannot be found
        // and the volume is named rather than read.
        bytes.resize(bytes.len() + 4096 * BLOCK, 0);
        let disc = crate::iso::tests::fixture(crate::iso::tests::Dialect::RockRidge);
        bytes[..disc.len()].copy_from_slice(&disc);

        let error = scan_image(&bytes).expect_err("a volume kosha cannot address");
        assert_eq!(
            error.reason(),
            "the disk image holds UDF, and its blocks are behind an indirection kosha does not follow"
        );
    }
}

/// A directory whose extents claim far more than its own length reads its own
/// length, and asks for no more memory than that.
///
/// **What this guards is a number, not a behaviour.** An extent's length is
/// thirty bits off the volume, so one can claim a gigabyte — and an extent that
/// is *allocated and never written* costs no read at all, just a gigabyte of
/// zeros. The buffer was bounded after each extent rather than within one, so a
/// file entry of two kilobytes could ask for that gigabyte before anything
/// noticed.
///
/// The tree is the same either way, which is why this is worth writing down —
/// but the cost is visible: **0.29 s without the bound and 0.00 s with it**,
/// measured by taking it out. A regression here is a gigabyte of zeros for a
/// directory of forty bytes, and nothing in the result would say so.
#[test]
fn a_directory_whose_extents_claim_a_gigabyte_reads_its_own_length() {
    let mut small = small(Shape::Short);
    let root = small.root;
    let kept = write_file(&mut small.volume, 4096, 1);
    let children = [Named {
        characteristics: 0,
        icb: kept,
        name: Some("KEPT.TXT"),
    }];

    let data = small.volume.claim(1);
    let written = records(root, &children, data, 0);
    small.volume.write_partition(data, &written);

    // One block of real records, then an extent that is allocated, never
    // written, and as long as the thirty bits of an extent length allow. The
    // directory's own length covers three blocks, so the hole is *reached* with
    // room still to fill — which is the case the bound is for. A length that
    // stopped at the first extent would be bounded by leaving the loop instead,
    // and this would test that rather than this.
    let mut payload = extent(Shape::Short, BLOCK as u32, data, 0, 0);
    payload.extend(extent(Shape::Short, 0x3FFF_FFFF, data + 1, 1, 0));
    write_entry(
        &mut small.volume,
        root,
        &Entry {
            kind: format::TYPE_DIRECTORY,
            links: 1,
            length: 3 * BLOCK as u64,
            blocks: 1,
            shape: Shape::Short,
            payload,
        },
    );

    let tree = tree_of(&close(small), &ScanOptions::default());
    assert_eq!(names_of(&tree), vec!["KEPT.TXT".to_string()]);
    assert_eq!(node_named(&tree, "KEPT.TXT").allocated, 4096);
}

/// A volume larger than a test can hold, with real bytes only where the fixture
/// put them and zeros everywhere else.
///
/// **What a sparse file reads as**, and what the blocks of a file nobody wrote
/// hold. It exists for one case: a file over four gigabytes, which is the whole
/// reason UDF is on a Windows installer at all — ISO 9660 cannot describe one in
/// a single extent and `install.wim` is larger than that. Proving kosha reads
/// such a file by *building* one would cost ten gigabytes of disk and several
/// minutes to exercise a `u64` read; this costs neither and exercises the same
/// one.
struct Sparse {
    bytes: Vec<u8>,
    size: u64,
}

impl crate::vhdx::ReadAt for Sparse {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> std::io::Result<()> {
        if offset.saturating_add(buffer.len() as u64) > self.size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "past the end of the volume",
            ));
        }
        buffer.fill(0);
        let Ok(at) = usize::try_from(offset) else {
            return Ok(());
        };
        if let Some(real) = self.bytes.get(at..) {
            let taken = real.len().min(buffer.len());
            buffer[..taken].copy_from_slice(&real[..taken]);
        }
        Ok(())
    }
}

/// A file of five gigabytes, which is why this format is on a Windows installer.
///
/// ISO 9660 keeps a file's length in a `u32` and cannot describe one larger in a
/// single extent; UDF keeps it in a `u64`. Reading the wrong half of that field,
/// or taking the length from the wrong offset in an entry whose layout moved
/// between revisions, gives a number that looks like a number.
#[test]
fn a_file_larger_than_four_gigabytes_is_read_as_what_it_is() {
    const HUGE: u64 = 5 * 1024 * 1024 * 1024;

    for shape in [Shape::Short, Shape::Embedded] {
        let mut small = small(shape);
        let at = small.volume.claim(1);
        let data = small.volume.next;
        // One extent that is allocated and never written, so the fixture holds
        // no data at all — a scan never reads a file's bytes, and this is what
        // it means for it not to.
        write_entry(
            &mut small.volume,
            at,
            &Entry {
                kind: TYPE_FILE,
                links: 1,
                length: HUGE,
                blocks: HUGE / BLOCK as u64,
                shape: Shape::Short,
                payload: extent(Shape::Short, 0x3FFF_FFFF, data, 1, 0),
            },
        );
        let children = [Named {
            characteristics: 0,
            icb: at,
            name: Some("install.wim"),
        }];
        let bytes = finish(small, &children);

        // A medium of eight gigabytes, so that five is a file on it rather than
        // a length longer than the volume — which is what a flipped bit in a
        // length looks like, and is refused.
        let mut fs = Filesystem::open(Sparse {
            bytes,
            size: 8 * 1024 * 1024 * 1024,
        })
        .expect("a volume");
        let start = super::build::root(&fs);
        let tree = super::build::build(
            &mut fs,
            Path::new("volume"),
            start,
            &ScanOptions::default(),
            |_: Progress| {},
            || false,
        )
        .expect("a walk that finishes");

        let node = node_named(&tree, "install.wim");
        assert_eq!(node.logical, HUGE, "{shape:?}");
        assert_eq!(node.allocated, HUGE, "{shape:?}");
        assert!(!node.flags.contains(crate::Flags::UNREADABLE), "{shape:?}");
        assert!(!tree.stats().partial, "{shape:?}");
    }
}
