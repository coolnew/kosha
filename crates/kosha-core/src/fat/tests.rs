// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Four filesystems built byte by byte, and walked by exactly the code that
//! walks a real one.
//!
//! There is no test-only path through the reader. The builders below write a
//! boot sector, a cluster table, short records, long-name records and exFAT
//! record sets the way the two formats define them — so these run, and catch
//! things, on a machine with no removable disk in it, which is every machine
//! this is likely to be edited on.
//!
//! **All four are built, and that is the point rather than thoroughness for
//! its own sake.** FAT12, FAT16 and FAT32 differ only in how wide a table
//! entry is, and the width is chosen by a cluster count the boot sector never
//! states — so a fixture of each is the only way to know the choice is being
//! made correctly. The FAT32 fixture is thirty-four megabytes because it has
//! to be: sixty-five thousand five hundred and twenty-five clusters is what
//! makes a volume FAT32, and a smaller one would be a FAT16 volume wearing the
//! name.

use std::path::Path;

use super::format::{self, Flavour};
use super::reader::Filesystem;
use crate::scan::{Progress, ScanOptions};

const SECTOR: usize = 512;

fn put16(image: &mut [u8], at: usize, value: u16) {
    image[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(image: &mut [u8], at: usize, value: u64) {
    image[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// 2024-05-17 14:30:20, as the two sixteen-bit fields FAT keeps it in.
const DATE: u16 = ((2024 - 1980) << 9) | (5 << 5) | 17;
const TIME: u16 = (14 << 11) | (30 << 5) | 10;
/// The same instant in seconds since 1970, taken as UTC because nothing on a
/// FAT volume says otherwise.
const WHEN: u32 = 1_715_956_220;

// ------------------------------------------------------------ the FAT builder

/// How a volume is laid out, which is five numbers and one consequence.
///
/// The consequence is the flavour: it is *not* a field anywhere on the disk,
/// it is `(total - first_data) / cluster_sectors` compared against two
/// thresholds. So a builder that wants a FAT16 volume has to arrange for one
/// rather than declare one, which is exactly what makes the fixtures worth
/// having.
struct Layout {
    cluster_sectors: usize,
    reserved: usize,
    fats: usize,
    fat_sectors: usize,
    root_entries: usize,
    clusters: usize,
}

impl Layout {
    fn of(flavour: Flavour) -> Layout {
        match flavour {
            // Under 4085 clusters, which is the only thing that makes a volume
            // FAT12.
            Flavour::Fat12 => Layout {
                cluster_sectors: 1,
                reserved: 1,
                fats: 2,
                fat_sectors: (202 * 3usize).div_ceil(2).div_ceil(SECTOR),
                root_entries: 512,
                clusters: 200,
            },
            // Over 4085 and under 65525.
            Flavour::Fat16 => Layout {
                cluster_sectors: 1,
                reserved: 1,
                fats: 2,
                fat_sectors: (4102 * 2usize).div_ceil(SECTOR),
                root_entries: 512,
                clusters: 4100,
            },
            // Over 65525, and with a root that is a cluster like any other
            // directory rather than a fixed run of sectors.
            Flavour::Fat32 => Layout {
                cluster_sectors: 1,
                reserved: 32,
                fats: 2,
                fat_sectors: (65532 * 4usize).div_ceil(SECTOR),
                root_entries: 0,
                clusters: 65530,
            },
            Flavour::ExFat => unreachable!("exFAT has a builder of its own"),
        }
    }

    fn root_sectors(&self) -> usize {
        (self.root_entries * format::ENTRY_BYTES).div_ceil(SECTOR)
    }

    fn first_data(&self) -> usize {
        self.reserved + self.fats * self.fat_sectors + self.root_sectors()
    }

    fn total_sectors(&self) -> usize {
        self.first_data() + self.clusters * self.cluster_sectors
    }
}

struct Builder {
    flavour: Flavour,
    layout: Layout,
    image: Vec<u8>,
    next: u32,
}

impl Builder {
    fn new(flavour: Flavour) -> Builder {
        let layout = Layout::of(flavour);
        let mut image = vec![0u8; layout.total_sectors() * SECTOR];

        // `BS_jmpBoot`, which the format requires and which is most of what
        // stops a reader firing on a file that merely ends its first sector
        // the right way.
        image[0] = 0xEB;
        image[1] = 0x58;
        image[2] = 0x90;
        image[3..11].copy_from_slice(b"KOSHA   ");
        put16(&mut image, 0x0B, SECTOR as u16);
        image[0x0D] = layout.cluster_sectors as u8;
        put16(&mut image, 0x0E, layout.reserved as u16);
        image[0x10] = layout.fats as u8;
        put16(&mut image, 0x11, layout.root_entries as u16);
        image[0x15] = 0xF8; // fixed disk
        put16(&mut image, 0x18, 63);
        put16(&mut image, 0x1A, 255);

        // **Both spellings of the sector count are exercised across the
        // fixtures**, because both are legal and a reader that only reads one
        // of them is wrong on half of the disks in the world. The sixteen-bit
        // field is used when the count fits in it, which is what a real
        // formatter does.
        let total = layout.total_sectors();
        if total < 0x1_0000 {
            put16(&mut image, 0x13, total as u16);
        } else {
            put32(&mut image, 0x20, total as u32);
        }

        if flavour == Flavour::Fat32 {
            put32(&mut image, 0x24, layout.fat_sectors as u32);
            put16(&mut image, 0x28, 0); // both tables maintained
            put32(&mut image, 0x2C, 2); // the root is the first cluster
            put16(&mut image, 0x30, 1); // an FSInfo sector nothing here reads
            image[0x42] = 0x29;
            put32(&mut image, 0x43, 0x1234_5678);
            image[0x47..0x52].copy_from_slice(&eight_three(b"KOSHA", b""));
            image[0x52..0x5A].copy_from_slice(b"FAT32   ");
        } else {
            put16(&mut image, 0x16, layout.fat_sectors as u16);
            image[0x24] = 0x80;
            image[0x26] = 0x29;
            put32(&mut image, 0x27, 0x1234_5678);
            image[0x2B..0x36].copy_from_slice(&eight_three(b"KOSHA", b""));
            // Deliberately the *wrong* string on the FAT12 volume. It is
            // documented as not being the answer, and a reader that believed
            // it would pick a sixteen-bit table for a twelve-bit one.
            image[0x36..0x3E].copy_from_slice(b"FAT16   ");
        }
        put16(&mut image, 0x1FE, 0xAA55);

        let mut builder = Builder {
            flavour,
            layout,
            image,
            next: 2,
        };
        // The first two table entries are the media descriptor and the dirty
        // flags. Nothing reads them, and a volume without them is one no
        // operating system would have written.
        builder.set_table(0, 0x0FFF_FF00 | 0xF8);
        builder.set_table(1, 0x0FFF_FFFF);
        builder
    }

    fn end_marker(&self) -> u32 {
        match self.flavour {
            Flavour::Fat12 => 0x0FFF,
            Flavour::Fat16 => 0xFFFF,
            _ => 0x0FFF_FFFF,
        }
    }

    /// Write one table entry, into every copy of the table.
    fn set_table(&mut self, cluster: u32, value: u32) {
        for copy in 0..self.layout.fats {
            let base = (self.layout.reserved + copy * self.layout.fat_sectors) * SECTOR;
            match self.flavour {
                Flavour::Fat12 => {
                    // A byte and a half per entry, so every second one starts
                    // in the middle of a byte.
                    let at = base + (cluster as usize) + (cluster as usize) / 2;
                    let packed = u16::from_le_bytes([self.image[at], self.image[at + 1]]);
                    let packed = if cluster & 1 == 1 {
                        (packed & 0x000F) | ((value as u16) << 4)
                    } else {
                        (packed & 0xF000) | (value as u16 & 0x0FFF)
                    };
                    put16(&mut self.image, at, packed);
                }
                Flavour::Fat16 => put16(&mut self.image, base + cluster as usize * 2, value as u16),
                _ => put32(
                    &mut self.image,
                    base + cluster as usize * 4,
                    value & 0x0FFF_FFFF,
                ),
            }
        }
    }

    /// Take `count` clusters and chain them together.
    fn chain(&mut self, count: usize) -> u32 {
        let first = self.next;
        let end = self.end_marker();
        for step in 0..count {
            let cluster = first + step as u32;
            let value = if step + 1 == count { end } else { cluster + 1 };
            self.set_table(cluster, value);
        }
        self.next += count as u32;
        first
    }

    fn cluster_at(&self, cluster: u32) -> usize {
        (self.layout.first_data() + (cluster as usize - 2) * self.layout.cluster_sectors) * SECTOR
    }

    fn cluster_bytes(&self) -> usize {
        self.layout.cluster_sectors * SECTOR
    }

    /// Write directory records into the clusters of a chain.
    fn write_chain(&mut self, first: u32, records: &[u8]) {
        let size = self.cluster_bytes();
        for (step, piece) in records.chunks(size).enumerate() {
            let at = self.cluster_at(first + step as u32);
            self.image[at..at + piece.len()].copy_from_slice(piece);
        }
    }

    fn write_root(&mut self, records: &[u8]) -> u32 {
        match self.flavour {
            Flavour::Fat32 => {
                let root = 2;
                self.write_chain(root, records);
                root
            }
            _ => {
                let at = self.layout.first_data() * SECTOR - self.layout.root_sectors() * SECTOR;
                self.image[at..at + records.len()].copy_from_slice(records);
                0
            }
        }
    }
}

/// An 8.3 name as the eleven bytes a record holds: no separator between the
/// two fields, each padded with spaces.
///
/// Built from the two halves rather than written as an eleven-byte literal,
/// because counting the spaces in `&eight_three(b"NOTE", b"MD")` by eye is exactly the
/// mistake a fixture is supposed not to make.
fn eight_three(base: &[u8], extension: &[u8]) -> [u8; 11] {
    let mut raw = [b' '; 11];
    raw[..base.len()].copy_from_slice(base);
    raw[8..8 + extension.len()].copy_from_slice(extension);
    raw
}

/// One 32-byte record naming a file or a directory.
fn short_record(raw: &[u8; 11], attributes: u8, case: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut record = [0u8; 32];
    record[0..11].copy_from_slice(raw);
    record[0x0B] = attributes;
    record[0x0C] = case;
    put16(&mut record, 0x10, DATE);
    put16(&mut record, 0x14, (cluster >> 16) as u16);
    put16(&mut record, 0x16, TIME);
    put16(&mut record, 0x18, DATE);
    put16(&mut record, 0x1A, cluster as u16);
    put32(&mut record, 0x1C, size);
    record
}

/// The long-name records that go in front of a short one.
///
/// Backwards, thirteen characters at a time, each tagged with a checksum of
/// the short name it belongs to — which is the whole of what ties the two
/// together.
fn long_records(name: &str, raw: &[u8; 11]) -> Vec<u8> {
    const SLOTS: [usize; format::LONG_NAME_CHARS] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let units: Vec<u16> = name.encode_utf16().collect();
    let parts = units.len().div_ceil(format::LONG_NAME_CHARS);
    let sum = format::checksum(raw);

    let mut out = Vec::new();
    for ordinal in (1..=parts).rev() {
        let mut record = [0u8; 32];
        record[0] = ordinal as u8 | if ordinal == parts { 0x40 } else { 0 };
        record[0x0B] = 0x0F;
        record[0x0D] = sum;
        let base = (ordinal - 1) * format::LONG_NAME_CHARS;
        for (index, at) in SLOTS.into_iter().enumerate() {
            // A NUL ends the name and 0xFFFF pads what is left of the record,
            // which is what lets a reader stop without counting.
            let unit = match units.get(base + index) {
                Some(unit) => *unit,
                None if base + index == units.len() => 0x0000,
                None => 0xFFFF,
            };
            put16(&mut record, at, unit);
        }
        out.extend_from_slice(&record);
    }
    out
}

fn dot_records(here: u32, parent: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&short_record(&eight_three(b".", b""), 0x10, 0, here, 0));
    // A `..` naming the root is written as cluster zero, whichever cluster the
    // root really is. Both are skipped by name, which is what stops the walk
    // climbing back up.
    out.extend_from_slice(&short_record(&eight_three(b"..", b""), 0x10, 0, parent, 0));
    out
}

/// A volume with one of everything in it: a plain short name, a long one, a
/// short name the lowercase bits apply to, an empty file, and a subdirectory
/// with both kinds of name inside it.
pub(crate) fn fixture(flavour: Flavour) -> Vec<u8> {
    let mut builder = Builder::new(flavour);
    // FAT32's root takes the first cluster, so everything else is allocated
    // after it.
    if flavour == Flavour::Fat32 {
        builder.chain(1);
    }

    let readme = builder.chain(1);
    let long = builder.chain(10); // 5000 bytes at 512 to a cluster
    let docs = builder.chain(1);
    let note = builder.chain(1);
    let another = builder.chain(1);

    let root_cluster = if flavour == Flavour::Fat32 { 2 } else { 0 };
    let mut inside = dot_records(docs, root_cluster);
    inside.extend_from_slice(&short_record(
        &eight_three(b"NOTE", b"MD"),
        0x20,
        0x18,
        note,
        10,
    ));
    inside.extend_from_slice(&long_records(
        "Another Long Name.bin",
        &eight_three(b"ANOTHE~1", b"BIN"),
    ));
    inside.extend_from_slice(&short_record(
        &eight_three(b"ANOTHE~1", b"BIN"),
        0x20,
        0,
        another,
        300,
    ));
    builder.write_chain(docs, &inside);

    let mut root = Vec::new();
    // A volume label: a record that is not deleted and names nothing. Reporting
    // it as a zero-byte file would be inventing an entry.
    root.extend_from_slice(&short_record(&eight_three(b"KOSHA", b""), 0x08, 0, 0, 0));
    root.extend_from_slice(&short_record(
        &eight_three(b"README", b"TXT"),
        0x20,
        0,
        readme,
        100,
    ));
    root.extend_from_slice(&long_records(
        "A long file name.txt",
        &eight_three(b"ALONGF~1", b"TXT"),
    ));
    root.extend_from_slice(&short_record(
        &eight_three(b"ALONGF~1", b"TXT"),
        0x20,
        0,
        long,
        5000,
    ));
    root.extend_from_slice(&short_record(
        &eight_three(b"DOCS", b""),
        0x10,
        0x08,
        docs,
        0,
    ));
    root.extend_from_slice(&short_record(&eight_three(b"EMPTY", b"BIN"), 0x20, 0, 0, 0));
    builder.write_root(&root);

    builder.image
}

// ---------------------------------------------------------- the exFAT builder

const EXFAT_CLUSTERS: u32 = 512;
const EXFAT_FAT_SECTORS: usize = 8;
const EXFAT_FAT_OFFSET: usize = 24;
const EXFAT_HEAP_OFFSET: usize = 32;

struct ExFat {
    image: Vec<u8>,
    next: u32,
}

impl ExFat {
    fn new() -> ExFat {
        let sectors = EXFAT_HEAP_OFFSET + EXFAT_CLUSTERS as usize;
        let mut image = vec![0u8; sectors * SECTOR];
        image[0] = 0xEB;
        image[1] = 0x76;
        image[2] = 0x90;
        image[3..11].copy_from_slice(b"EXFAT   ");
        // 0x0B to 0x40 stays zero. That is the field exFAT uses to make
        // software which reads a BPB refuse the volume rather than read a
        // sector size of nothing out of it.
        put64(&mut image, 0x48, sectors as u64);
        put32(&mut image, 0x50, EXFAT_FAT_OFFSET as u32);
        put32(&mut image, 0x54, EXFAT_FAT_SECTORS as u32);
        put32(&mut image, 0x58, EXFAT_HEAP_OFFSET as u32);
        put32(&mut image, 0x5C, EXFAT_CLUSTERS);
        put32(&mut image, 0x60, 2); // the root directory
        put32(&mut image, 0x64, 0x1234_5678);
        put16(&mut image, 0x68, 0x0100); // revision 1.0
        image[0x6C] = 9; // 512-byte sectors
        image[0x6D] = 0; // one sector to a cluster
        image[0x6E] = 1; // one table
        image[0x70] = 50;
        put16(&mut image, 0x1FE, 0xAA55);

        let mut built = ExFat { image, next: 2 };
        built.set_table(0, 0xFFFF_FFF8);
        built.set_table(1, 0xFFFF_FFFF);
        built
    }

    fn set_table(&mut self, cluster: u32, value: u32) {
        let at = EXFAT_FAT_OFFSET * SECTOR + cluster as usize * 4;
        put32(&mut self.image, at, value);
    }

    /// Clusters chained through the table, the way a file that grew is.
    fn chain(&mut self, count: usize) -> u32 {
        let first = self.next;
        for step in 0..count {
            let cluster = first + step as u32;
            let value = if step + 1 == count {
                0xFFFF_FFFF
            } else {
                cluster + 1
            };
            self.set_table(cluster, value);
        }
        self.next += count as u32;
        first
    }

    /// Clusters that follow one another with nothing in the table about them.
    ///
    /// The ordinary case on exFAT: a file written in one go gets this, and a
    /// reader that consulted the table for it would read zeros and report an
    /// empty directory.
    fn contiguous(&mut self, count: usize) -> u32 {
        let first = self.next;
        self.next += count as u32;
        first
    }

    fn cluster_at(&self, cluster: u32) -> usize {
        (EXFAT_HEAP_OFFSET + cluster as usize - 2) * SECTOR
    }

    fn write(&mut self, first: u32, records: &[u8]) {
        for (step, piece) in records.chunks(SECTOR).enumerate() {
            let at = self.cluster_at(first + step as u32);
            self.image[at..at + piece.len()].copy_from_slice(piece);
        }
    }
}

/// A file's whole record set: one record saying it is a file, one saying where
/// its bytes are, and the rest its name fifteen characters at a time.
fn exfat_set(name: &str, cluster: u32, length: u64, directory: bool, contiguous: bool) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let name_records = units.len().div_ceil(format::EXFAT_NAME_CHARS);
    let secondaries = 1 + name_records;

    let mut set = vec![0u8; format::ENTRY_BYTES * (1 + secondaries)];
    set[0] = 0x85;
    set[1] = secondaries as u8;
    put16(&mut set, 0x04, if directory { 0x0010 } else { 0x0020 });
    let stamp = (u32::from(DATE) << 16) | u32::from(TIME);
    put32(&mut set, 0x08, stamp);
    put32(&mut set, 0x0C, stamp);
    put32(&mut set, 0x10, stamp);

    let stream = format::ENTRY_BYTES;
    set[stream] = 0xC0;
    // AllocationPossible, and NoFatChain when the run needs no table.
    set[stream + 1] = 0x01 | if contiguous { 0x02 } else { 0x00 };
    set[stream + 3] = units.len() as u8;
    put64(&mut set, stream + 0x08, length); // valid data length
    put32(&mut set, stream + 0x14, cluster);
    put64(&mut set, stream + 0x18, length);

    for part in 0..name_records {
        let at = format::ENTRY_BYTES * (2 + part);
        set[at] = 0xC1;
        for index in 0..format::EXFAT_NAME_CHARS {
            let unit = units
                .get(part * format::EXFAT_NAME_CHARS + index)
                .copied()
                .unwrap_or(0);
            put16(&mut set, at + 0x02 + index * 2, unit);
        }
    }

    // The checksum is over every byte of every record of the set, with the two
    // bytes holding it skipped. Writing it last is the only order that works.
    let mut sum = 0u16;
    for (index, record) in set.chunks(format::ENTRY_BYTES).enumerate() {
        sum = format::set_checksum(sum, record, index == 0);
    }
    put16(&mut set, 0x02, sum);
    set
}

/// The allocation bitmap or the upcase table: a record that stands alone, with
/// no set around it and no name in it.
fn exfat_metadata(kind: u8, cluster: u32, length: u64) -> Vec<u8> {
    let mut record = vec![0u8; format::ENTRY_BYTES];
    record[0] = kind;
    put32(&mut record, 0x14, cluster);
    put64(&mut record, 0x18, length);
    record
}

/// The same tree as the FAT fixture, so the two can be compared directly.
pub(crate) fn exfat_fixture() -> Vec<u8> {
    let mut builder = ExFat::new();
    let root = builder.chain(1);
    assert_eq!(root, 2);

    let bitmap = builder.contiguous(1);
    let upcase = builder.contiguous(1);
    let readme = builder.contiguous(1);
    let long = builder.chain(10);
    let docs = builder.chain(1);
    let note = builder.contiguous(1);
    let another = builder.contiguous(1);

    let mut inside = exfat_set("note.md", note, 10, false, true);
    inside.extend_from_slice(&exfat_set(
        "Another Long Name.bin",
        another,
        300,
        false,
        true,
    ));
    builder.write(docs, &inside);

    let mut records = exfat_metadata(0x81, bitmap, 64);
    records.extend_from_slice(&exfat_metadata(0x82, upcase, 512));
    records.extend_from_slice(&exfat_set("README.TXT", readme, 100, false, true));
    records.extend_from_slice(&exfat_set("A long file name.txt", long, 5000, false, false));
    records.extend_from_slice(&exfat_set("docs", docs, 512, true, false));
    // A file of no bytes has no cluster, which is not damage — it is what an
    // empty file is.
    records.extend_from_slice(&exfat_set("EMPTY.BIN", 0, 0, false, false));
    builder.write(root, &records);

    builder.image
}

// -------------------------------------------------------------------- helpers

fn tree_of(image: &[u8], options: &ScanOptions) -> crate::SizeTree {
    let mut fs = Filesystem::open(image).expect("the builder writes a readable boot sector");
    let start = super::reader::root(fs.geometry());
    super::build::build(
        &mut fs,
        Path::new("/image"),
        start,
        options,
        |_: Progress| {},
        || false,
    )
    .expect("a built image is walkable")
}

fn shape(tree: &crate::SizeTree) -> Vec<String> {
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

/// What every fixture holds, minus the root, whose own size is the one thing
/// the four do not agree on.
fn expected() -> Vec<String> {
    vec![
        // 5000 bytes is ten clusters of 512, and the last of them is mostly
        // empty — which is the whole reason anybody scans a disk.
        "A long file name.txt 5000 5120 file".to_string(),
        "Another Long Name.bin 300 512 file".to_string(),
        "EMPTY.BIN 0 0 file".to_string(),
        "README.TXT 100 512 file".to_string(),
        // Its own cluster of records, plus the two files in it.
        "docs 310 1536 dir".to_string(),
        "note.md 10 512 file".to_string(),
    ]
}

// ------------------------------------------------------------------ the format

#[test]
fn a_boot_sector_that_is_not_one_is_refused() {
    assert_eq!(format::boot(&[0u8; 512]), Err(format::Unreadable::NotFat));
    assert_eq!(format::boot(&[0u8; 8]), Err(format::Unreadable::NotFat));
}

/// NTFS shares this boot sector layout as far as the cluster size, and a
/// reader that took the first plausible answer would read an NTFS volume as a
/// FAT one and produce a tree of rubbish rather than an error.
#[test]
fn an_ntfs_boot_sector_is_not_taken_for_a_fat_one() {
    let mut sector = [0u8; 512];
    sector[0] = 0xEB;
    sector[1] = 0x52;
    sector[2] = 0x90;
    sector[3..11].copy_from_slice(b"NTFS    ");
    put16(&mut sector, 0x0B, 512);
    sector[0x0D] = 8;
    sector[0x15] = 0xF8;
    // NTFS keeps its own sector count eight bytes further on, and leaves both
    // of FAT's where they are.
    put64(&mut sector, 0x28, 1_000_000);
    put16(&mut sector, 0x1FE, 0xAA55);
    assert_eq!(format::boot(&sector), Err(format::Unreadable::NotFat));
}

/// The same check from the other side: a volume that *is* FAT in every other
/// respect, with both sector counts zeroed the way NTFS leaves them.
#[test]
fn a_volume_that_states_no_sectors_is_refused() {
    let mut image = fixture(Flavour::Fat12);
    put16(&mut image, 0x13, 0);
    put32(&mut image, 0x20, 0);
    assert_eq!(format::boot(&image), Err(format::Unreadable::NotFat));
}

/// The width of the table is a consequence of the cluster count and of
/// nothing else — not of the string at 0x36, which says "FAT16" on the FAT12
/// fixture on purpose.
#[test]
fn the_flavour_comes_from_the_cluster_count() {
    for flavour in [Flavour::Fat12, Flavour::Fat16, Flavour::Fat32] {
        let image = fixture(flavour);
        let geometry = format::boot(&image).expect("the fixture is a volume");
        assert_eq!(geometry.flavour, flavour, "{} misread", flavour.name());
    }
    let geometry = format::boot(&exfat_fixture()).expect("the fixture is a volume");
    assert_eq!(geometry.flavour, Flavour::ExFat);
}

/// Twelve-bit entries are stored a byte and a half apart, so every second one
/// starts in the middle of a byte. Getting the halves the wrong way round
/// produces a chain that is plausible and wrong.
#[test]
fn twelve_bit_table_entries_are_read_from_both_halves() {
    // Two entries packed into three bytes: 0x123 then 0x456.
    let packed = [0x23u8, 0x61, 0x45];
    assert_eq!(
        format::table_entry(Flavour::Fat12, 0, &packed[0..2]),
        Some(0x123)
    );
    assert_eq!(
        format::table_entry(Flavour::Fat12, 1, &packed[1..3]),
        Some(0x456)
    );
    assert_eq!(format::table_entry_at(Flavour::Fat12, 0), Some((0, 2)));
    assert_eq!(format::table_entry_at(Flavour::Fat12, 1), Some((1, 2)));
    assert_eq!(format::table_entry_at(Flavour::Fat12, 2), Some((3, 2)));
}

#[test]
fn a_fat_timestamp_is_the_instant_it_names() {
    assert_eq!(format::timestamp(DATE, TIME), WHEN);
    assert_eq!(
        format::exfat_timestamp((u32::from(DATE) << 16) | u32::from(TIME)),
        WHEN
    );
    // A date no calendar has.
    assert_eq!(format::timestamp(0, 0), 0);
    assert_eq!(format::timestamp((44 << 9) | (13 << 5) | 1, 0), 0);
}

/// The one thing exFAT fixed about FAT's clocks. The top bit says the field
/// was filled in; without it, zero means "not recorded" rather than UTC.
#[test]
fn an_exfat_zone_moves_the_instant_and_only_when_it_is_stated() {
    assert_eq!(format::with_zone(WHEN, 0x00), WHEN);
    // Two hours east: the wall clock ran ahead of UTC, so UTC is earlier.
    assert_eq!(format::with_zone(WHEN, 0x80 | 8), WHEN - 7200);
    // One hour west, as a seven-bit two's-complement number.
    assert_eq!(format::with_zone(WHEN, 0x80 | 0x7C), WHEN + 3600);
}

/// A short name is not ASCII and nothing on the disk says what it is. 437 is
/// the answer that is right most often, and it is very nearly never reached.
#[test]
fn a_short_name_outside_ascii_is_read_as_code_page_437() {
    let mut name = Vec::new();
    format::push_short_name(&eight_three(b"\x81BER", b"TXT"), 0, &mut name);
    assert_eq!(String::from_utf8(name).unwrap(), "\u{fc}BER.TXT");

    // The lowercase bits only ever meant ASCII.
    let mut name = Vec::new();
    format::push_short_name(&eight_three(b"NOTE", b"MD"), 0x18, &mut name);
    assert_eq!(String::from_utf8(name).unwrap(), "note.md");

    // A name really beginning with the byte that marks a deleted record is
    // stored as 0x05. Putting it back gives 0xE5, which is not ASCII either —
    // so it goes through 437 like any other high byte and comes out as the
    // sigma a DOS machine would have shown.
    let mut name = Vec::new();
    format::push_short_name(&eight_three(b"\x05AVE", b"TXT"), 0, &mut name);
    assert_eq!(String::from_utf8(name).unwrap(), "\u{3c3}AVE.TXT");

    // And only in the first position: everywhere else 0x05 is itself.
    let mut name = Vec::new();
    format::push_short_name(&eight_three(b"A\x05VE", b"TXT"), 0, &mut name);
    assert_eq!(name[1], 0x05, "only the first character is ever escaped");
}

// -------------------------------------------------------------------- the walk

/// All four, against one expectation. The tree a volume describes cannot
/// depend on how wide its table entries are.
#[test]
fn every_flavour_walks_into_the_same_tree() {
    for (flavour, image) in [
        (Flavour::Fat12, fixture(Flavour::Fat12)),
        (Flavour::Fat16, fixture(Flavour::Fat16)),
        (Flavour::Fat32, fixture(Flavour::Fat32)),
        (Flavour::ExFat, exfat_fixture()),
    ] {
        let tree = tree_of(&image, &ScanOptions::default());
        assert_eq!(
            shape(&tree),
            expected(),
            "{} walked into a different tree",
            flavour.name()
        );
        assert!(
            !tree.stats().partial,
            "{} reported a partial tree",
            flavour.name()
        );
        assert_eq!(tree.stats().files, 5, "{}", flavour.name());
        assert_eq!(tree.stats().dirs, 2, "{}", flavour.name());
        assert_eq!(tree.stats().entries, 7, "{}", flavour.name());
    }
}

/// A FAT12 or FAT16 root is a fixed run of sectors with no cluster number, and
/// it is the only directory in either format addressed that way. Its size is
/// therefore a property of the volume rather than of what is in it.
#[test]
fn a_fixed_root_is_as_large_as_the_boot_sector_says() {
    for flavour in [Flavour::Fat12, Flavour::Fat16] {
        let image = fixture(flavour);
        let geometry = format::boot(&image).unwrap();
        let format::Root::Fixed { bytes, .. } = geometry.root else {
            panic!("{} should have a fixed root", flavour.name());
        };
        assert_eq!(bytes, 512 * format::ENTRY_BYTES as u64);

        let tree = tree_of(&image, &ScanOptions::default());
        let root = tree.node(tree.root());
        // The root's own records, plus everything under it.
        assert_eq!(root.allocated, bytes + 5120 + 512 + 1536);
    }
    // FAT32 gave the root a cluster like any other directory, and exFAT kept
    // that. One cluster of records here rather than sixteen kilobytes of them.
    for image in [fixture(Flavour::Fat32), exfat_fixture()] {
        let tree = tree_of(&image, &ScanOptions::default());
        assert_eq!(tree.node(tree.root()).allocated, 512 + 5120 + 512 + 1536);
    }
}

/// A record that is not deleted and names nothing. Reporting it as a zero-byte
/// file in the root would be inventing an entry.
#[test]
fn a_volume_label_is_not_a_file() {
    let tree = tree_of(&fixture(Flavour::Fat12), &ScanOptions::default());
    assert!(
        !shape(&tree).iter().any(|row| row.starts_with("KOSHA ")),
        "the volume label was reported as a file"
    );
}

/// The allocation bitmap and the upcase table are records that stand alone,
/// with no set around them. They are not files and they have no names.
#[test]
fn exfat_metadata_records_are_not_files() {
    let tree = tree_of(&exfat_fixture(), &ScanOptions::default());
    assert_eq!(
        tree.len(),
        7,
        "the bitmap or the upcase table became a file"
    );
}

/// A run the table says nothing about is the ordinary case on exFAT, and a
/// reader that consulted the table for it would find zeros.
#[test]
fn a_contiguous_exfat_run_is_read_without_the_table() {
    let mut image = exfat_fixture();
    // Wipe the whole table. Everything contiguous still reads; the one chained
    // directory does not, and the tree says so rather than shrinking quietly.
    let start = EXFAT_FAT_OFFSET * SECTOR;
    image[start..start + EXFAT_FAT_SECTORS * SECTOR].fill(0);

    let mut fs = Filesystem::open(image.as_slice()).expect("the boot sector is untouched");
    // The root is chained, so with no table it is one cluster and stops. Every
    // entry in it is still found, because they all fit in that one cluster.
    let start = super::reader::root(fs.geometry());
    let tree = super::build::build(
        &mut fs,
        Path::new("/image"),
        start,
        &ScanOptions::default(),
        |_: Progress| {},
        || false,
    )
    .expect("the volume still opens");
    let names = shape(&tree);
    assert!(
        names.iter().any(|row| row.starts_with("README.TXT ")),
        "a contiguous file needed the table: {names:?}"
    );
}

/// The checksum is what ties a run of long-name pieces to the record that
/// follows them. Software that predated long names would rename the short
/// record without touching them, and the pieces would then belong to a name
/// that is no longer there.
#[test]
fn a_long_name_whose_checksum_does_not_match_falls_back_to_the_short_one() {
    let mut image = fixture(Flavour::Fat12);
    let sum = format::checksum(&eight_three(b"ALONGF~1", b"TXT"));
    let mut flipped = 0usize;
    for at in (0..image.len()).step_by(format::ENTRY_BYTES) {
        if image[at + 0x0B] == 0x0F && image[at + 0x0D] == sum {
            image[at + 0x0D] ^= 0xFF;
            flipped += 1;
        }
    }
    assert_eq!(flipped, 2, "the long name should be in two pieces");

    let tree = tree_of(&image, &ScanOptions::default());
    let names = shape(&tree);
    assert!(
        names.iter().any(|row| row.starts_with("ALONGF~1.TXT ")),
        "the short name should have been used: {names:?}"
    );
    assert!(
        !names.iter().any(|row| row.starts_with("A long file")),
        "a long name that does not belong to this record was used: {names:?}"
    );
}

/// exFAT is the only format here whose directory entries are checked at all.
/// A set that does not add up is one a filesystem was in the middle of
/// writing, and the entry it half-describes is not one to report.
#[test]
fn an_exfat_set_that_does_not_add_up_is_refused_rather_than_read() {
    let mut image = exfat_fixture();
    let root = EXFAT_HEAP_OFFSET * SECTOR;
    // The first record set in the root, past the bitmap and the upcase table,
    // is README.TXT. Change its length without fixing the checksum, which is
    // exactly what a torn write looks like.
    let stream = root + format::ENTRY_BYTES * 3;
    assert_eq!(image[stream], 0xC0);
    put64(&mut image, stream + 0x18, 999_999);

    let tree = tree_of(&image, &ScanOptions::default());
    let names = shape(&tree);
    assert!(
        !names.iter().any(|row| row.starts_with("README.TXT ")),
        "a set that does not add up was read anyway: {names:?}"
    );
    assert!(
        tree.stats().partial,
        "a refused entry has to be said, not swallowed"
    );
    assert!(
        names.iter().any(|row| row.starts_with("docs ")),
        "the rest of the directory still reads: {names:?}"
    );
}

/// A path somebody typed has to match the way the filesystem itself matches:
/// neither format has ever had case-sensitive names.
#[test]
fn a_path_inside_an_image_resolves_whatever_case_it_is_typed_in() {
    for image in [
        fixture(Flavour::Fat12),
        fixture(Flavour::Fat32),
        exfat_fixture(),
    ] {
        let mut fs = Filesystem::open(image.as_slice()).unwrap();
        for typed in ["docs", "DOCS", "Docs"] {
            let place = super::build::resolve(&mut fs, Path::new(typed))
                .unwrap()
                .unwrap_or_else(|| panic!("{typed} should resolve"));
            let tree = super::build::build(
                &mut fs,
                Path::new("/image/docs"),
                place,
                &ScanOptions::default(),
                |_: Progress| {},
                || false,
            )
            .unwrap();
            assert_eq!(tree.len(), 3, "{typed}");
        }
        assert!(
            super::build::resolve(&mut fs, Path::new("nowhere"))
                .unwrap()
                .is_none()
        );
        // A file is not a place a scan can start, and saying so is better than
        // measuring the directory that holds it.
        assert!(
            super::build::resolve(&mut fs, Path::new("README.TXT"))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn a_scan_that_is_cancelled_says_so() {
    let image = fixture(Flavour::Fat12);
    let mut fs = Filesystem::open(image.as_slice()).unwrap();
    let start = super::reader::root(fs.geometry());
    let tree = super::build::build(
        &mut fs,
        Path::new("/image"),
        start,
        &ScanOptions::default(),
        |_: Progress| {},
        || true,
    )
    .unwrap();
    assert!(tree.stats().cancelled);
    assert!(tree.stats().partial);
}

#[test]
fn a_depth_limit_stops_the_walk_where_it_says() {
    let image = fixture(Flavour::Fat12);
    let tree = tree_of(
        &image,
        &ScanOptions {
            max_depth: Some(1),
            ..ScanOptions::default()
        },
    );
    let names = shape(&tree);
    assert!(names.iter().any(|row| row.starts_with("docs ")));
    assert!(
        !names.iter().any(|row| row.starts_with("note.md ")),
        "the walk went past the depth it was given: {names:?}"
    );
}

// ------------------------------------------------------------ damaged input

/// Every bit of every structure the reader trusts, flipped one at a time.
///
/// A volume is read out of an image somebody else wrote, and an image being
/// copied or a card pulled out of a slot is a half-updated structure rather
/// than an attack. Every one of these must produce a tree or an error, and
/// never a panic, a hang, or an allocation the machine cannot serve.
///
/// FAT12 is the fixture swept because it is the smallest complete one, and
/// because its table is the one with an entry straddling every second byte —
/// the arithmetic most likely to be wrong.
#[test]
fn every_flipped_bit_produces_a_tree_or_an_error() {
    let mut image = fixture(Flavour::Fat12);
    let layout = Layout::of(Flavour::Fat12);
    let mut swept = 0usize;

    let table = layout.reserved * SECTOR;
    let root = (layout.first_data() - layout.root_sectors()) * SECTOR;
    let heap = layout.first_data() * SECTOR;
    let regions = [
        0..SECTOR,                            // the boot sector
        table..table + SECTOR,                // the table
        root..root + SECTOR,                  // the records in the root
        heap + 3 * SECTOR..heap + 4 * SECTOR, // the subdirectory's cluster
    ];
    for region in regions {
        for at in region {
            for bit in 0..8 {
                let original = image[at];
                image[at] ^= 1 << bit;
                if let Ok(mut fs) = Filesystem::open(image.as_slice()) {
                    let start = super::reader::root(fs.geometry());
                    let _ = super::build::build(
                        &mut fs,
                        Path::new("/image"),
                        start,
                        &ScanOptions::default(),
                        |_: Progress| {},
                        || false,
                    );
                }
                image[at] = original;
                swept += 1;
            }
        }
    }

    // Computed from what was covered rather than written down, and floored so
    // a sweep that quietly covered nothing fails rather than passes.
    assert_eq!(swept, 4 * SECTOR * 8);
    assert!(swept >= 16_000, "only {swept} mutations");
}

/// The same sweep over exFAT, whose record sets are a different parser
/// entirely: a state machine across records rather than one record at a time.
#[test]
fn every_flipped_bit_of_an_exfat_volume_produces_a_tree_or_an_error() {
    let mut image = exfat_fixture();
    let mut swept = 0usize;

    let table = EXFAT_FAT_OFFSET * SECTOR;
    let root = EXFAT_HEAP_OFFSET * SECTOR;
    let regions = [
        0..SECTOR,
        table..table + SECTOR,
        root..root + SECTOR,
        // The subdirectory, which is the eighth cluster.
        root + 6 * SECTOR..root + 7 * SECTOR,
    ];
    for region in regions {
        for at in region {
            for bit in 0..8 {
                let original = image[at];
                image[at] ^= 1 << bit;
                if let Ok(mut fs) = Filesystem::open(image.as_slice()) {
                    let start = super::reader::root(fs.geometry());
                    let _ = super::build::build(
                        &mut fs,
                        Path::new("/image"),
                        start,
                        &ScanOptions::default(),
                        |_: Progress| {},
                        || false,
                    );
                }
                image[at] = original;
                swept += 1;
            }
        }
    }
    assert_eq!(swept, 4 * SECTOR * 8);
}

#[test]
fn every_truncation_produces_a_tree_or_an_error() {
    for image in [fixture(Flavour::Fat12), exfat_fixture()] {
        // Every sector boundary rather than every byte: a read that stops
        // mid-structure is the same case whichever byte it stopped on.
        for keep in (0..image.len()).step_by(SECTOR) {
            if let Ok(mut fs) = Filesystem::open(&image[..keep]) {
                let start = super::reader::root(fs.geometry());
                let _ = super::build::build(
                    &mut fs,
                    Path::new("/image"),
                    start,
                    &ScanOptions::default(),
                    |_: Progress| {},
                    || false,
                );
            }
        }
    }
}

/// A chain that points back at itself. Without a bound this is a walk with no
/// end, and the bound has to be the smaller of "larger than a directory can
/// be" and "longer than the volume has clusters".
#[test]
fn a_chain_that_loops_is_stopped_rather_than_followed() {
    let mut builder = Builder::new(Flavour::Fat12);
    let looping = builder.chain(1);
    // Point the one cluster at itself.
    builder.set_table(looping, looping);
    let mut inside = dot_records(looping, 0);
    inside.extend_from_slice(&short_record(&eight_three(b"ONE", b"TXT"), 0x20, 0, 0, 0));
    builder.write_chain(looping, &inside);

    let root = short_record(&eight_three(b"LOOP", b""), 0x10, 0, looping, 0);
    builder.write_root(&root);
    let image = builder.image;

    let tree = tree_of(&image, &ScanOptions::default());
    assert!(
        tree.stats().partial,
        "a chain with no end has to be refused, and said so"
    );
    // Two hundred clusters is all the volume has, so the walk stops there and
    // the directory's size is a floor rather than a number nobody can trust.
    assert!(tree.len() < 8, "a looping chain produced a tree of entries");
}

/// A record naming a cluster the volume does not have. The name is real and
/// the thing it names is not, so it is counted as unreadable rather than
/// dropped without trace.
#[test]
fn an_entry_pointing_outside_the_volume_is_named_rather_than_dropped() {
    let mut builder = Builder::new(Flavour::Fat12);
    let mut root = short_record(&eight_three(b"GONE", b"TXT"), 0x20, 0, 9_999, 4096).to_vec();
    root.extend_from_slice(&short_record(
        &eight_three(b"ALSOGONE", b"DIR"),
        0x10,
        0,
        9_999,
        0,
    ));
    builder.write_root(&root);
    let image = builder.image;

    let tree = tree_of(&image, &ScanOptions::default());
    let names = shape(&tree);
    assert!(
        names.iter().any(|row| row.starts_with("GONE.TXT ")),
        "the name is real even where the cluster is not: {names:?}"
    );
    assert!(tree.stats().partial);
    assert!(tree.stats().unreadable >= 2);
}

/// A directory that names itself. `.` and `..` are skipped by name, so the
/// only way to arrive somewhere already visited is a record pointing back up
/// the tree — which is a corrupt volume and a walk with no end.
#[test]
fn a_directory_that_names_itself_does_not_loop() {
    let mut builder = Builder::new(Flavour::Fat12);
    let here = builder.chain(1);
    let mut inside = dot_records(here, 0);
    // A third name for the same directory, which is not `.` and not `..`.
    inside.extend_from_slice(&short_record(&eight_three(b"AGAIN", b""), 0x10, 0, here, 0));
    builder.write_chain(here, &inside);
    builder.write_root(&short_record(&eight_three(b"SELF", b""), 0x10, 0, here, 0));
    let image = builder.image;

    let tree = tree_of(&image, &ScanOptions::default());
    assert!(tree.len() < 8, "a self-naming directory produced a loop");
}

// --------------------------------------------------- inside a partitioned disk

/// A volume that does not start at the beginning of the disk.
///
/// **This is where the EFI system partition stops being skipped.** Every GPT
/// disk has one, the specification that defines it says it holds FAT12, FAT16
/// or FAT32, and until this module a scan of a whole-disk image found the root
/// filesystem and stepped over the one at the front — or, on an image that
/// held nothing else, refused the disk entirely.
mod on_a_partitioned_disk {
    use super::*;
    use crate::partition::tests::{EFI_SYSTEM, SECTOR as DISK_SECTOR, gpt, mbr_entry};

    /// Where every real tool puts the first partition: a megabyte in.
    const START: usize = 2048 * DISK_SECTOR;

    fn disk_around(volume: &[u8]) -> Vec<u8> {
        let mut disk = vec![0u8; START + volume.len() + DISK_SECTOR];
        disk[START..START + volume.len()].copy_from_slice(volume);
        disk
    }

    fn scan_image(disk: &[u8], name: &str) -> Result<crate::SizeTree, crate::ScanError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, disk).unwrap();
        crate::Scanner::new(ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        })
        .scan(&path)
    }

    fn names(tree: &crate::SizeTree) -> Vec<String> {
        let mut rows: Vec<String> = (0..tree.len())
            .map(crate::NodeId::from_index)
            .filter(|&id| id != tree.root())
            .map(|id| tree.name_lossy(id).to_string())
            .collect();
        rows.sort();
        rows
    }

    /// The image of a USB stick, which is the ordinary way somebody has one of
    /// these as a file.
    #[test]
    fn a_master_boot_record_leads_to_a_fat_volume() {
        let volume = fixture(Flavour::Fat32);
        let mut disk = disk_around(&volume);
        mbr_entry(
            &mut disk,
            0,
            0,
            0x0C, // FAT32 with LBA addressing
            2048,
            (volume.len() / DISK_SECTOR) as u32,
        );

        let tree = scan_image(&disk, "stick.img").expect("a partitioned disk is an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("FAT32 · partition 1 of 1 (MBR)"),
            "a scan must say which partition it read, and what was in it"
        );
        assert!(names(&tree).contains(&"README.TXT".to_string()));
    }

    /// An EFI system partition, alone on a disk. Before this it was the whole
    /// of the disk and kosha refused all of it.
    #[test]
    fn an_efi_system_partition_is_read_rather_than_skipped() {
        let volume = fixture(Flavour::Fat16);
        let mut disk = disk_around(&volume);
        let last = ((START + volume.len()) / DISK_SECTOR - 1) as u64;
        gpt(&mut disk, &[(2048, last, EFI_SYSTEM)]);

        let tree = scan_image(&disk, "esp.img").expect("an EFI system partition is a filesystem");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("FAT16 · partition 1 of 1 (GPT)")
        );
        assert!(names(&tree).contains(&"docs".to_string()));
    }

    /// exFAT is what a card out of a camera and a disk formatted for a
    /// television are, and both come with a partition table in front.
    #[test]
    fn an_exfat_volume_in_a_partition_is_read() {
        let volume = exfat_fixture();
        let mut disk = disk_around(&volume);
        mbr_entry(
            &mut disk,
            0,
            0,
            0x07,
            2048,
            (volume.len() / DISK_SECTOR) as u32,
        );

        let tree = scan_image(&disk, "card.img").expect("a partitioned disk is an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("exFAT · partition 1 of 1 (MBR)")
        );
        assert!(names(&tree).contains(&"A long file name.txt".to_string()));
    }

    /// A volume with no table in front of it at all, which is what `mkfs` on a
    /// file leaves and what a `dd` of one partition produces.
    #[test]
    fn a_volume_at_the_front_needs_no_table() {
        for volume in [fixture(Flavour::Fat12), exfat_fixture()] {
            let expected = format::boot(&volume).unwrap().flavour.name();
            let tree = scan_image(&volume, "volume.img").expect("a bare volume is an image");
            assert_eq!(tree.stats.image.as_deref(), Some(expected));
        }
    }
}

/// Both formats spread one entry over several records, so a filesystem
/// interrupted part way through creating one leaves half of it on the disk.
/// The entry is real and is not in the tree, and the difference between those
/// two is the whole of what `partial` means.
#[test]
fn half_an_entry_at_the_end_of_a_directory_is_said_rather_than_ignored() {
    let mut builder = Builder::new(Flavour::Fat12);
    let readme = builder.chain(1);
    let mut root = short_record(&eight_three(b"README", b"TXT"), 0x20, 0, readme, 100).to_vec();
    // The long-name records of an entry whose short record was never written.
    root.extend_from_slice(&long_records(
        "Interrupted half way.txt",
        &eight_three(b"INTERR~1", b"TXT"),
    ));
    builder.write_root(&root);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    let names = shape(&tree);
    assert!(
        names.iter().any(|row| row.starts_with("README.TXT ")),
        "the entry that was written is still read: {names:?}"
    );
    assert!(
        tree.stats().partial,
        "half an entry has to be said, not swallowed"
    );
}

/// The same for exFAT, where a set is up to nineteen records and the
/// half-written case is correspondingly more likely.
#[test]
fn half_an_exfat_record_set_is_said_rather_than_ignored() {
    let mut builder = ExFat::new();
    let root = builder.chain(1);
    let readme = builder.contiguous(1);

    let mut records = exfat_set("README.TXT", readme, 100, false, true);
    // A file record claiming three more records, and nothing after it.
    let orphan = exfat_set("Interrupted.txt", 0, 0, false, false);
    records.extend_from_slice(&orphan[..format::ENTRY_BYTES]);
    builder.write(root, &records);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    assert!(
        shape(&tree)
            .iter()
            .any(|row| row.starts_with("README.TXT ")),
        "the complete set is still read"
    );
    assert!(tree.stats().partial);
}

/// A file cannot be longer than the volume that holds it. exFAT states a
/// length in sixty-four bits, so one flipped bit asks for exabytes — and a
/// total nobody can read past is worse than a total that says it is short.
#[test]
fn a_file_longer_than_the_volume_is_refused_rather_than_added_up() {
    let mut builder = Builder::new(Flavour::Fat12);
    let one = builder.chain(1);
    let mut root = short_record(&eight_three(b"HUGE", b"BIN"), 0x20, 0, one, 4_000_000).to_vec();
    root.extend_from_slice(&short_record(
        &eight_three(b"REAL", b"BIN"),
        0x20,
        0,
        one,
        100,
    ));
    builder.write_root(&root);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    assert!(tree.stats().partial);
    let names = shape(&tree);
    assert!(
        names.contains(&"HUGE.BIN 0 0 file".to_string()),
        "an impossible length is not added up: {names:?}"
    );
    assert!(
        names.contains(&"REAL.BIN 100 512 file".to_string()),
        "the rest of the directory still reads: {names:?}"
    );
}
