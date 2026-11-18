// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The MFT strategy, tested without an MFT.
//!
//! Reading a real volume needs Windows, administrator, and a machine somebody
//! is willing to have read at full speed — which is three reasons a test suite
//! cannot depend on one. So the format is built here instead: records assembled
//! byte by byte to the layout the specification fixes, fed to the same parser
//! the disk feeds, and a table assembled by hand and handed to the same builder.
//!
//! That covers everything except `volume.rs`, which opens a handle and reads at
//! offsets and does nothing else. The split exists so that this line is the only
//! untested one.

use std::ffi::OsStr;

use super::build::{self, Root, Table};
use super::record::{self, Name, Names, Summary};
use super::security;
use crate::node::Flags;
use crate::owner::OwnerId;
use crate::scan::{Progress, ScanOptions};

const RECORD_SIZE: usize = 1024;
const SECTOR: usize = 512;

/// Attribute type codes, spelled again here so a test would notice a constant
/// being edited to match a broken parser rather than the other way round.
const STANDARD_INFORMATION: u32 = 0x10;
const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const REPARSE_POINT: u32 = 0xC0;

const NAMESPACE_WIN32: u8 = 1;
const NAMESPACE_DOS: u8 = 2;

/// 2021-01-01T00:00:00Z as a FILETIME, and as the Unix seconds it must become.
const FILETIME_2021: u64 = 132_539_328_000_000_000;
const UNIX_2021: u32 = 1_609_459_200;

// ------------------------------------------------------------- record fixture

/// One MFT record under construction.
struct Record {
    flags: u16,
    base: u32,
    attributes: Vec<Vec<u8>>,
}

impl Record {
    fn file() -> Record {
        Record {
            flags: 0x0001,
            base: 0,
            attributes: Vec::new(),
        }
    }

    fn directory() -> Record {
        Record {
            flags: 0x0003,
            ..Record::file()
        }
    }

    fn deleted() -> Record {
        Record {
            flags: 0x0000,
            ..Record::file()
        }
    }

    fn extension_of(base: u32) -> Record {
        Record {
            base,
            ..Record::file()
        }
    }

    fn with(mut self, attribute: Vec<u8>) -> Record {
        self.attributes.push(attribute);
        self
    }

    /// Lay the record out and write the update sequence array over it, exactly
    /// as NTFS does on the way to the disk.
    fn build(&self) -> Vec<u8> {
        let mut record = vec![0u8; RECORD_SIZE];
        record[0..4].copy_from_slice(b"FILE");
        let sectors = RECORD_SIZE / SECTOR;
        let usa_offset = 0x30usize;
        put16(&mut record, 0x04, usa_offset as u16);
        put16(&mut record, 0x06, (sectors + 1) as u16);
        put16(&mut record, 0x10, 1); // sequence
        put16(&mut record, 0x12, 1); // link count
        let first = 0x38usize;
        put16(&mut record, 0x14, first as u16);
        put16(&mut record, 0x16, self.flags);
        put32(&mut record, 0x1C, RECORD_SIZE as u32);
        put64(&mut record, 0x20, u64::from(self.base));

        let mut at = first;
        for attribute in &self.attributes {
            record[at..at + attribute.len()].copy_from_slice(attribute);
            at += attribute.len();
        }
        put32(&mut record, at, 0xFFFF_FFFF); // the end marker
        put32(&mut record, 0x18, (at + 4) as u32); // used size

        // The counter, then the two bytes it displaces from each sector.
        let usn: u16 = 0x4B4F;
        put16(&mut record, usa_offset, usn);
        for sector in 0..sectors {
            let tail = (sector + 1) * SECTOR - 2;
            let saved = [record[tail], record[tail + 1]];
            record[usa_offset + 2 + sector * 2] = saved[0];
            record[usa_offset + 3 + sector * 2] = saved[1];
            record[tail..tail + 2].copy_from_slice(&usn.to_le_bytes());
        }
        record
    }

    /// Parse it the way the volume reader would.
    fn read(&self) -> (Option<Summary>, Vec<(String, u32)>) {
        let mut bytes = self.build();
        let mut arena = Vec::new();
        let mut refs = Vec::new();
        let summary = record::parse(
            &mut bytes,
            &mut Names {
                arena: &mut arena,
                refs: &mut refs,
            },
        );
        let names = refs
            .iter()
            .map(|name| {
                let slice = &arena[name.off as usize..name.off as usize + name.len as usize];
                (String::from_utf8_lossy(slice).into_owned(), name.parent)
            })
            .collect();
        (summary, names)
    }
}

/// A resident attribute wrapped around a value.
fn resident(kind: u32, flags: u16, value: &[u8]) -> Vec<u8> {
    let length = (0x18 + value.len()).next_multiple_of(8);
    let mut attribute = vec![0u8; length];
    put32(&mut attribute, 0x00, kind);
    put32(&mut attribute, 0x04, length as u32);
    attribute[0x08] = 0; // resident
    put16(&mut attribute, 0x0C, flags);
    put32(&mut attribute, 0x10, value.len() as u32);
    put16(&mut attribute, 0x14, 0x18);
    attribute[0x18..0x18 + value.len()].copy_from_slice(value);
    attribute
}

fn standard_information(mtime: u64, attributes: u32) -> Vec<u8> {
    standard_information_with_security(mtime, attributes, 0)
}

/// The 0x48-byte form, NTFS 3.0 and later, which carries a security id.
fn standard_information_with_security(mtime: u64, attributes: u32, security_id: u32) -> Vec<u8> {
    let mut value = vec![0u8; 0x48];
    put64(&mut value, 0x08, mtime);
    put32(&mut value, 0x20, attributes);
    put32(&mut value, 0x34, security_id);
    resident(STANDARD_INFORMATION, 0, &value)
}

/// The 0x30-byte form, NTFS 1.2, which stops before the security id. A volume
/// upgraded from it keeps short records until each file is next written.
fn standard_information_short(mtime: u64, attributes: u32) -> Vec<u8> {
    let mut value = vec![0u8; 0x30];
    put64(&mut value, 0x08, mtime);
    put32(&mut value, 0x20, attributes);
    resident(STANDARD_INFORMATION, 0, &value)
}

fn file_name(parent: u32, namespace: u8, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut value = vec![0u8; 0x42 + units.len() * 2];
    put64(&mut value, 0x00, u64::from(parent) | (3u64 << 48)); // a sequence number to ignore
    // Sizes NTFS keeps here and does not maintain. Deliberately absurd: if
    // anything ever reads them instead of `$DATA`, these are what shows up.
    put64(&mut value, 0x28, 999_999);
    put64(&mut value, 0x30, 999_999);
    value[0x40] = units.len() as u8;
    value[0x41] = namespace;
    for (i, unit) in units.iter().enumerate() {
        put16(&mut value, 0x42 + i * 2, *unit);
    }
    resident(FILE_NAME, 0, &value)
}

/// A `$DATA` small enough to live inside the record.
fn data_resident(length: usize) -> Vec<u8> {
    resident(DATA, 0, &vec![0xAB; length])
}

/// A `$DATA` whose bytes are out on the volume.
///
/// `total` is the field that only exists when the attribute is compressed or
/// sparse, and that holds what is *really* allocated.
fn data_nonresident(
    logical: u64,
    allocated: u64,
    flags: u16,
    compression_unit: u16,
    total: Option<u64>,
    run_list: &[u8],
) -> Vec<u8> {
    let header = if total.is_some() { 0x48 } else { 0x40 };
    let length = (header + run_list.len()).next_multiple_of(8);
    let mut attribute = vec![0u8; length];
    put32(&mut attribute, 0x00, DATA);
    put32(&mut attribute, 0x04, length as u32);
    attribute[0x08] = 1; // non-resident
    put16(&mut attribute, 0x0C, flags);
    put64(&mut attribute, 0x10, 0); // starting VCN
    put16(&mut attribute, 0x20, header as u16);
    put16(&mut attribute, 0x22, compression_unit);
    put64(&mut attribute, 0x28, allocated);
    put64(&mut attribute, 0x30, logical);
    put64(&mut attribute, 0x38, logical);
    if let Some(total) = total {
        put64(&mut attribute, 0x40, total);
    }
    attribute[header..header + run_list.len()].copy_from_slice(run_list);
    attribute
}

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

// ------------------------------------------------------------- record parsing

#[test]
fn an_ordinary_file_reads_as_itself() {
    let (summary, names) = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "report.pdf"))
        .with(data_nonresident(
            127_387,
            131_072,
            0,
            0,
            None,
            &[0x11, 0x20, 0x40, 0x00],
        ))
        .read();

    let summary = summary.expect("a well-formed record parses");
    assert!(summary.in_use);
    assert!(!summary.is_dir);
    assert!(summary.has_data);
    assert_eq!(summary.logical, 127_387);
    assert_eq!(summary.allocated, 131_072);
    assert_eq!(summary.mtime, UNIX_2021);
    assert_eq!(names, vec![("report.pdf".to_string(), 5)]);
}

#[test]
fn creation_time_is_not_modification_or_inode_change_time() {
    let mut value = vec![0u8; 0x48];
    put64(&mut value, 0x00, FILETIME_2021);
    put64(&mut value, 0x08, FILETIME_2021 + 90 * 10_000_000);
    put64(&mut value, 0x10, FILETIME_2021 + 180 * 10_000_000);
    let (summary, _) = Record::file()
        .with(resident(STANDARD_INFORMATION, 0, &value))
        .read();
    let summary = summary.unwrap();
    assert_eq!(summary.created, Some(u64::from(UNIX_2021)));
    assert_eq!(summary.mtime, UNIX_2021 + 90);
    let (unknown, _) = Record::file()
        .with(standard_information(FILETIME_2021, 0))
        .read();
    assert_eq!(unknown.unwrap().created, None);
}

/// 490 bytes of resident data occupy 496 bytes after 8-byte alignment.
/// A file this small occupies no cluster at all, which is the one case the
/// walk's cluster rounding cannot express and this reads directly.
#[test]
fn a_file_inside_its_own_record_is_rounded_to_eight_bytes_not_to_a_cluster() {
    let (summary, _) = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "resident.txt"))
        .with(data_resident(490))
        .read();

    let summary = summary.unwrap();
    assert_eq!(summary.logical, 490);
    assert_eq!(
        summary.allocated, 496,
        "a resident file's allocation is its length rounded to 8, not to 4096"
    );
}

/// The whole reason the MFT is worth reading rather than enumerating: a
/// compressed file's real allocation is in the record, and reading the wrong
/// field of the same header reports the saving as though it had not happened.
#[test]
fn a_compressed_file_reports_what_it_occupies_rather_than_what_it_would_have() {
    const COMPRESSED: u16 = 0x0001;
    let (summary, _) = Record::file()
        .with(standard_information(FILETIME_2021, 0x800))
        .with(file_name(5, NAMESPACE_WIN32, "compressed.bin"))
        .with(data_nonresident(
            1_800_000,
            1_802_240,
            COMPRESSED,
            4,
            Some(225_280),
            &[0x11, 0x37, 0x40, 0x00],
        ))
        .read();

    let summary = summary.unwrap();
    assert_eq!(summary.logical, 1_800_000);
    assert_eq!(
        summary.allocated, 225_280,
        "read AllocatedLength instead of TotalAllocated: the saving vanished"
    );
}

/// A sparse file uses the same field, and it is the field that makes
/// "already saved by sparse files" computable on Windows at all.
#[test]
fn a_sparse_file_reports_only_the_clusters_it_has() {
    const SPARSE: u16 = 0x8000;
    let (summary, _) = Record::file()
        .with(standard_information(FILETIME_2021, 0x200))
        .with(file_name(5, NAMESPACE_WIN32, "sparse.img"))
        .with(data_nonresident(
            10_737_418_240,
            10_737_418_240,
            SPARSE,
            4,
            Some(65_536),
            &[0x01, 0x40, 0x00],
        ))
        .read();

    let summary = summary.unwrap();
    assert_eq!(summary.logical, 10_737_418_240);
    assert_eq!(summary.allocated, 65_536);
}

/// Parse a run of records into a table, the way the volume reader does: record
/// `n` at index `n`.
fn table_of(records: &[Record]) -> Table {
    let mut table = Table {
        records: vec![Summary::default(); records.len()],
        names: Vec::new(),
        owners: Vec::new(),
        name_bytes: Vec::new(),
        security_stream: Default::default(),
    };
    for (number, record) in records.iter().enumerate() {
        let mut bytes = record.build();
        let mut names = Names {
            arena: &mut table.name_bytes,
            refs: &mut table.names,
        };
        if let Some(summary) = record::parse(&mut bytes, &mut names) {
            table.records[number] = summary;
        }
        table.owners.resize(table.names.len(), number as u32);
    }
    table
}

/// Every name the table gives this record, in order.
fn names_of(table: &Table, record: u32) -> Vec<String> {
    table
        .names
        .iter()
        .zip(&table.owners)
        .filter(|(_, owner)| **owner == record)
        .map(|(name, _)| {
            String::from_utf8_lossy(
                &table.name_bytes[name.off as usize..name.off as usize + name.len as usize],
            )
            .into_owned()
        })
        .collect()
}

/// An 8.3 name is a second name for the same file. Listing it would put a
/// phantom `PROGRA~1` beside every long name on the volume.
#[test]
fn a_short_name_does_not_become_a_second_file() {
    let mut table = table_of(&[
        Record::file(),
        Record::directory()
            .with(standard_information(FILETIME_2021, 0x10))
            .with(file_name(5, NAMESPACE_DOS, "PROGRA~1"))
            .with(file_name(5, NAMESPACE_WIN32, "Program Files")),
    ]);

    build::consolidate(&mut table);
    assert_eq!(names_of(&table, 1), vec!["Program Files".to_string()]);
}

/// The case the per-record rule got wrong, found by diffing the two strategies
/// over `C:\Program Files`: the long name is in the base record and the 8.3
/// name is in an extension record. Looked at on its own, the extension record
/// holds a file with no long name and every reason to keep its short one — so
/// 36 phantom `NEWSAP~2.PNG` files appeared beside their real names. The
/// question is about the file, and the file is not whole until its extensions
/// have been folded in.
#[test]
fn a_short_name_in_another_record_is_still_the_same_file() {
    let mut table = table_of(&[
        Record::file(),
        Record::file()
            .with(standard_information(FILETIME_2021, 0x20))
            .with(file_name(5, NAMESPACE_WIN32, "NewsAppTile.scale-100.png"))
            .with(data_resident(400)),
        Record::extension_of(1).with(file_name(5, NAMESPACE_DOS, "NEWSAP~2.PNG")),
    ]);
    assert_eq!(
        names_of(&table, 2),
        vec!["NEWSAP~2.PNG".to_string()],
        "the fixture has to put the short name in the extension record"
    );

    build::consolidate(&mut table);
    assert_eq!(
        names_of(&table, 1),
        vec!["NewsAppTile.scale-100.png".to_string()]
    );
    assert!(names_of(&table, 2).is_empty());
}

/// And the other half of that rule: a file whose *only* name is an 8.3 one is
/// still a file, and dropping it would lose it entirely.
#[test]
fn a_file_with_nothing_but_a_short_name_is_still_listed() {
    let mut table = table_of(&[
        Record::file(),
        Record::file()
            .with(standard_information(FILETIME_2021, 0x20))
            .with(file_name(5, NAMESPACE_DOS, "OLD~1.TXT")),
    ]);

    build::consolidate(&mut table);
    assert_eq!(names_of(&table, 1), vec!["OLD~1.TXT".to_string()]);
}

/// A heavily hardlinked file spills its extra names into extension records, and
/// the names there belong to the *file*. Left pointing at the extension they
/// name a record with no `$STANDARD_INFORMATION` — an epoch timestamp and no
/// attributes — and split one file across several record numbers, which is the
/// identity hardlink accounting is built on. 23,822 records on this machine's
/// `C:`.
#[test]
fn a_name_in_an_extension_record_belongs_to_the_file() {
    let mut table = table_of(&[
        Record::file(),
        Record::file()
            .with(standard_information(FILETIME_2021, 0x20))
            .with(file_name(5, NAMESPACE_WIN32, "wab32res.dll"))
            .with(data_nonresident(
                974_848,
                974_848,
                0,
                0,
                None,
                &[0x11, 0xEE, 0x40, 0x00],
            )),
        Record::extension_of(1).with(file_name(9, NAMESPACE_WIN32, "wab32res.dll")),
    ]);

    build::consolidate(&mut table);
    assert_eq!(
        names_of(&table, 1),
        vec!["wab32res.dll".to_string(), "wab32res.dll".to_string()],
        "both names have to hang off the one record that knows the file's dates"
    );
    assert_eq!(table.records[1].mtime, UNIX_2021);
    assert!(names_of(&table, 2).is_empty());
}

/// CompactOS, and what most installers now use: the unnamed `$DATA` is an empty
/// sparse placeholder and the bytes are in a stream called `WofCompressedData`.
/// Windows presents such a file as an ordinary one — no reparse attribute, no
/// compression attribute, and the named stream's allocation as its size on disk
/// — so reading only the unnamed stream reports Chrome's 550 KB resource files
/// as occupying nothing at all.
#[test]
fn a_file_compressed_by_the_overlay_filter_reports_its_compressed_stream() {
    let mut volume = Volume::new();
    volume.add(
        Summary {
            in_use: true,
            has_data: true,
            logical: 556_921,
            allocated: 0,
            named_data_allocated: 188_416,
            attributes: 0x2420, // ARCHIVE | REPARSE_POINT | NOT_CONTENT_INDEXED
            reparse_tag: 0x8000_0017, // IO_REPARSE_TAG_WOF
            ..Summary::default()
        },
        &[(build::ROOT_RECORD, "af.pak")],
    );

    let tree = volume.build(&ScanOptions::default());
    let file = tree.children(tree.root()).next().unwrap();
    assert_eq!(tree.node(file).allocated, 188_416);
    assert_eq!(tree.node(file).logical, 556_921);
    assert!(
        tree.node(file).flags.is_empty(),
        "Windows shows this as an ordinary file and so must we: {:?}",
        tree.node(file).flags
    );
}

/// Two names in two directories is what a hardlink looks like from here — no
/// open, no file index, no second syscall. The walk pays a `CreateFile` per
/// file for this answer.
#[test]
fn a_hardlinked_file_carries_every_name_it_has() {
    let (summary, names) = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "libfoo.rlib"))
        .with(file_name(9, NAMESPACE_WIN32, "libfoo.rlib"))
        .read();

    assert!(summary.unwrap().in_use);
    assert_eq!(
        names,
        vec![
            ("libfoo.rlib".to_string(), 5),
            ("libfoo.rlib".to_string(), 9),
        ]
    );
}

/// The top sixteen bits of a file reference count how often the record has been
/// reused. Taking them for part of the number puts every parent at an address
/// nothing lives at.
#[test]
fn the_sequence_number_is_not_part_of_the_parent_record_number() {
    let (_, names) = Record::file()
        .with(file_name(5, NAMESPACE_WIN32, "a.txt"))
        .read();
    assert_eq!(names[0].1, 5);
}

#[test]
fn a_deleted_record_is_read_and_contributes_nothing() {
    let (summary, names) = Record::deleted()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "gone.txt"))
        .with(data_resident(400))
        .read();

    let summary = summary.expect("a deleted record is still a record");
    assert!(!summary.in_use);
    assert_eq!(summary.logical, 0);
    assert!(
        names.is_empty(),
        "a deleted file's name would be a phantom entry in its old directory"
    );
}

/// NTFS writes a counter over the last two bytes of every sector so it can tell
/// a half-written record from a whole one. We check the same thing: a record
/// that fails its own integrity check is not one to build a tree out of.
#[test]
fn a_record_that_fails_its_own_integrity_check_is_refused() {
    let file = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "torn.txt"));

    let mut bytes = file.build();
    // A torn write: the second sector was never updated, so its tail still
    // holds whatever was there before.
    bytes[2 * SECTOR - 2] ^= 0xFF;

    let mut arena = Vec::new();
    let mut refs = Vec::new();
    assert!(
        record::parse(
            &mut bytes,
            &mut Names {
                arena: &mut arena,
                refs: &mut refs
            }
        )
        .is_none()
    );
}

#[test]
fn anything_that_is_not_a_record_is_skipped_rather_than_read() {
    let mut arena = Vec::new();
    let mut refs = Vec::new();
    let mut names = Names {
        arena: &mut arena,
        refs: &mut refs,
    };

    // Records past `$MFT`'s initialised length are simply zeros, and there are
    // a great many of them on a fresh volume.
    assert!(record::parse(&mut vec![0u8; RECORD_SIZE], &mut names).is_none());
    assert!(record::parse(&mut b"BAAD".to_vec(), &mut names).is_none());
    assert!(record::parse(&mut Vec::new(), &mut names).is_none());
}

/// The lengths in a record are the disk's word rather than ours, and a length
/// of zero is the one that does not fail — it loops.
#[test]
fn a_corrupt_attribute_length_stops_the_walk_rather_than_spinning() {
    let file = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "ok.txt"));
    let mut bytes = file.build();

    // Undo the fixups so the edit below survives, then re-apply none: the
    // parser checks the counter first, so edit after its own read instead by
    // pointing the first attribute's length at zero.
    let first = 0x38usize;
    let saved = [bytes[SECTOR - 2], bytes[SECTOR - 1]];
    put32(&mut bytes, first + 4, 0);
    bytes[SECTOR - 2] = saved[0];
    bytes[SECTOR - 1] = saved[1];

    let mut arena = Vec::new();
    let mut refs = Vec::new();
    // The point is that this returns at all.
    let _ = record::parse(
        &mut bytes,
        &mut Names {
            arena: &mut arena,
            refs: &mut refs,
        },
    );
}

/// A mounted volume is a reparse point, and the MFT holds the same
/// `REPARSE_DATA_BUFFER` the walk pays an open and an ioctl to fetch.
#[test]
fn a_mounted_volume_is_recognised_from_the_record_alone() {
    use crate::platform::reparse::{MOUNT_POINT_TAG, tests::buffer};

    let volume = Record::directory()
        .with(standard_information(FILETIME_2021, 0x410))
        .with(file_name(5, NAMESPACE_WIN32, "backup"))
        .with(resident(
            REPARSE_POINT,
            0,
            &buffer(MOUNT_POINT_TAG, r"\??\Volume{7f2d3c14-0000-0000-0000-1}\"),
        ))
        .read()
        .0
        .unwrap();
    assert!(volume.is_mounted_volume);

    let junction = Record::directory()
        .with(standard_information(FILETIME_2021, 0x410))
        .with(file_name(5, NAMESPACE_WIN32, "shortcut"))
        .with(resident(
            REPARSE_POINT,
            0,
            &buffer(MOUNT_POINT_TAG, r"\??\C:\elsewhere"),
        ))
        .read()
        .0
        .unwrap();
    assert!(!junction.is_mounted_volume);
}

/// The bootstrap: `$MFT`'s own extent map, which everything else is read
/// through.
#[test]
fn the_mft_run_list_is_found_in_record_zero() {
    let run_list = [0x31, 0x42, 0x00, 0x14, 0x00, 0x00];
    let mut bytes = Record::file()
        .with(standard_information(FILETIME_2021, 0x06))
        .with(file_name(5, NAMESPACE_WIN32, "$MFT"))
        .with(data_nonresident(270_336, 270_336, 0, 0, None, &run_list))
        .build();

    let stream = record::data_stream(&mut bytes, &[]).expect("record 0 describes where $MFT is");
    let runs = super::runs::decode(stream.runs).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].clusters, 0x42);
    assert_eq!(runs[0].start, Some(0x1400));
}

/// The trap the on-disk attribute field sets, caught on a real volume:
/// `C:\Program Files-Zipz.exe` holds `0x00040020` in its record while
/// `GetFileAttributes` reports `0x20`. NTFS keeps its own bookkeeping in the
/// high bits and Win32 masks it off, and `0x40000` happens to be
/// `FILE_ATTRIBUTE_RECALL_ON_OPEN` — so taking the field at face value marked
/// every ordinary file on the volume as a cloud placeholder whose bytes are not
/// on this disk.
#[test]
fn the_high_bits_of_the_attribute_field_are_not_win32_attributes() {
    let (summary, _) = Record::file()
        .with(standard_information(FILETIME_2021, 0x0004_0020))
        .with(file_name(5, NAMESPACE_WIN32, "7z.exe"))
        .with(data_nonresident(
            576_000,
            577_536,
            0,
            0,
            None,
            &[0x11, 0x8D, 0x40, 0x00],
        ))
        .read();
    assert_eq!(
        summary.unwrap().attributes,
        0x0020,
        "an NTFS-internal bit was read as a Win32 file attribute"
    );
}

/// And the consequence, end to end: an ordinary file with that bit set must not
/// come out flagged.
#[test]
fn an_ordinary_file_is_not_mistaken_for_a_cloud_placeholder() {
    let mut volume = Volume::new();
    volume.add(
        Summary {
            in_use: true,
            has_data: true,
            logical: 576_000,
            allocated: 577_536,
            attributes: 0x0020,
            ..Summary::default()
        },
        &[(build::ROOT_RECORD, "7z.exe")],
    );

    let tree = volume.build(&ScanOptions::default());
    let file = tree.children(tree.root()).next().unwrap();
    assert!(
        tree.node(file).flags.is_empty(),
        "{:?}",
        tree.node(file).flags
    );
}

/// A real placeholder is recognised from its reparse tag, which is where the
/// filter driver keeps the answer. The `RECALL_ON_*` bits the walk reads are
/// synthesised at runtime and are never in a record at all.
#[test]
fn a_cloud_placeholder_is_recognised_by_its_reparse_tag() {
    let mut volume = Volume::new();
    volume.add(
        Summary {
            in_use: true,
            has_data: true,
            logical: 4_000_000_000,
            allocated: 0,
            attributes: 0x0420, // ARCHIVE | REPARSE_POINT, and no OFFLINE bit
            reparse_tag: 0x9000_301A, // IO_REPARSE_TAG_CLOUD_3
            ..Summary::default()
        },
        &[(build::ROOT_RECORD, "holiday.mov")],
    );

    let tree = volume.build(&ScanOptions::default());
    let file = tree.children(tree.root()).next().unwrap();
    let flags = tree.node(file).flags;
    assert!(
        flags.contains(Flags::OFFLINE),
        "its bytes are not on this disk"
    );
    assert!(
        !flags.contains(Flags::SYMLINK),
        "a placeholder is a file to report, not a link to skip past"
    );
}

/// A file too fragmented to fit its run list in one record spills into an
/// extension record, and the fragment carrying the sizes can be in either. The
/// base names itself in the extension's header, which is how the two are put
/// back together without parsing `$ATTRIBUTE_LIST`.
#[test]
fn a_files_sizes_are_found_even_when_they_are_in_an_extension_record() {
    let base = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "fragmented.vhdx"))
        .read()
        .0
        .unwrap();
    assert!(
        !base.has_data,
        "the fixture has to be a base record with nowhere to put $DATA"
    );

    let extension = Record::extension_of(7)
        .with(data_nonresident(
            40_000_000_000,
            40_001_536_000,
            0,
            0,
            None,
            &[0x11, 0x40, 0x20, 0x00],
        ))
        .read()
        .0
        .unwrap();
    assert_eq!(extension.base, 7);
    assert!(extension.has_data);

    let mut table = Table {
        records: vec![Summary::default(); 8],
        names: Vec::new(),
        owners: Vec::new(),
        name_bytes: Vec::new(),
        security_stream: Default::default(),
    };
    table.records[7] = base;
    table.records.push(extension);

    assert_eq!(build::consolidate(&mut table), 1);
    assert_eq!(table.records[7].logical, 40_000_000_000);
    assert_eq!(table.records[7].allocated, 40_001_536_000);
}

// -------------------------------------------------------------- tree building

/// A table built by hand, in the shape the volume reader produces one.
struct Volume {
    table: Table,
    /// What `$Secure` would have said, for the tests that ask about owners.
    security: security::Security,
}

impl Volume {
    /// Records 0 to 4 are NTFS' own; the root directory is always record 5.
    fn new() -> Volume {
        let mut table = Table {
            records: vec![Summary::default(); 6],
            names: Vec::new(),
            owners: Vec::new(),
            name_bytes: Vec::new(),
            security_stream: Default::default(),
        };
        table.records[build::ROOT_RECORD as usize] = Summary {
            in_use: true,
            is_dir: true,
            ..Summary::default()
        };
        Volume {
            table,
            security: security::Security::default(),
        }
    }

    /// Give a security id an owner, as `$Secure` does on a real volume.
    fn owned_by(&mut self, security_id: u32, sid: &str) -> &mut Volume {
        self.security.insert(security_id, sid.to_string());
        self
    }

    fn add(&mut self, summary: Summary, names: &[(u32, &str)]) -> u32 {
        let record = self.table.records.len() as u32;
        self.table.records.push(summary);
        for (parent, name) in names {
            let off = self.table.name_bytes.len() as u32;
            self.table.name_bytes.extend_from_slice(name.as_bytes());
            self.table.names.push(Name {
                parent: *parent,
                off,
                len: name.len() as u16,
                namespace: NAMESPACE_WIN32,
            });
            self.table.owners.push(record);
        }
        record
    }

    fn dir(&mut self, parent: u32, name: &str) -> u32 {
        self.add(
            Summary {
                in_use: true,
                is_dir: true,
                mtime: UNIX_2021,
                ..Summary::default()
            },
            &[(parent, name)],
        )
    }

    fn file(&mut self, parent: u32, name: &str, size: u64) -> u32 {
        self.add(
            Summary {
                in_use: true,
                has_data: true,
                logical: size,
                allocated: size.next_multiple_of(4096),
                mtime: UNIX_2021,
                ..Summary::default()
            },
            &[(parent, name)],
        )
    }

    /// A file carrying a security id, which is how a record says who owns it.
    fn file_owned(&mut self, parent: u32, name: &str, size: u64, security_id: u32) -> u32 {
        self.add(
            Summary {
                in_use: true,
                has_data: true,
                logical: size,
                allocated: size.next_multiple_of(4096),
                mtime: UNIX_2021,
                security_id,
                ..Summary::default()
            },
            &[(parent, name)],
        )
    }

    fn build(&self, options: &ScanOptions) -> crate::SizeTree {
        build::build(
            &self.table,
            Root {
                record: build::ROOT_RECORD,
                path: std::path::Path::new("C:\\"),
            },
            options,
            &self.security,
            |_: Progress| {},
            || false,
        )
    }
}

/// Follow a chain of names down from the root.
///
/// Not [`SizeTree::find`](crate::SizeTree::find), which takes a path and
/// therefore takes a platform's idea of what separates one. These tables
/// describe an NTFS volume and are built on every platform, so a test written
/// with backslashes in it passes on Windows and silently stops testing anything
/// anywhere else.
fn at(tree: &crate::SizeTree, names: &[&str]) -> crate::NodeId {
    let mut id = tree.root();
    for name in names {
        id = tree
            .children(id)
            .find(|&child| tree.name_lossy(child) == *name)
            .unwrap_or_else(|| panic!("{name} is not below {}", tree.name_lossy(id)));
    }
    id
}

/// The invariants everything downstream is written against. Breaking any of
/// them does not fail loudly — it produces a tree that renders, sorts and
/// exports, and is wrong.
fn assert_arena_invariants(tree: &crate::SizeTree) {
    let nodes = tree.nodes();
    assert_eq!(nodes[0].parent, 0, "the root is its own parent");
    for (index, node) in nodes.iter().enumerate().skip(1) {
        assert!(
            (node.parent as usize) < index,
            "node {index} is before its parent, so aggregation reads it too late"
        );
    }
    for (index, node) in nodes.iter().enumerate() {
        let range = node.child_range();
        assert!(range.end <= nodes.len(), "children run off the end");
        for child in range.clone() {
            assert_eq!(
                nodes[child].parent as usize, index,
                "a child in the run does not point back at the parent"
            );
        }
        let names: Vec<&OsStr> = range
            .map(|i| tree.name(crate::NodeId::from_index(i)))
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "a directory's children are out of order");
    }
}

#[test]
fn a_table_becomes_a_tree_with_the_arena_the_walk_would_have_built() {
    let mut volume = Volume::new();
    let windows = volume.dir(build::ROOT_RECORD, "Windows");
    let users = volume.dir(build::ROOT_RECORD, "Users");
    volume.file(windows, "notepad.exe", 200_000);
    volume.file(users, "notes.txt", 4_000);
    volume.file(build::ROOT_RECORD, "pagefile.sys", 8_000_000_000);

    let tree = volume.build(&ScanOptions::default());
    assert_arena_invariants(&tree);

    assert_eq!(tree.stats().strategy, crate::Strategy::MasterFileTable);
    assert_eq!(tree.stats().dirs, 3, "the root and two directories");
    assert_eq!(tree.stats().files, 3);
    assert_eq!(tree.stats().entries, 6);
    assert_eq!(
        tree.total_allocated(),
        200_704 + 4_096 + 8_000_000_000,
        "sizes did not roll up to the root"
    );

    // Sorted by name, as the walk sorts every directory.
    let top: Vec<String> = tree
        .children(tree.root())
        .map(|id| tree.name_lossy(id).into_owned())
        .collect();
    assert_eq!(top, vec!["Users", "Windows", "pagefile.sys"]);
}

/// Record 5's own `$FILE_NAME` is `.`, and its parent is itself. Without the
/// guard the root is its own child and the tree never ends.
#[test]
fn the_root_is_not_a_child_of_itself() {
    let mut volume = Volume::new();
    let off = volume.table.name_bytes.len() as u32;
    volume.table.name_bytes.extend_from_slice(b".");
    volume.table.names.push(Name {
        parent: build::ROOT_RECORD,
        off,
        len: 1,
        namespace: NAMESPACE_WIN32,
    });
    volume.table.owners.push(build::ROOT_RECORD);
    volume.file(build::ROOT_RECORD, "a.txt", 10);

    let tree = volume.build(&ScanOptions::default());
    assert_arena_invariants(&tree);
    assert_eq!(
        tree.stats().entries,
        2,
        "the root counted itself as a child"
    );
}

/// One file record, two names, and the bytes belong to the file rather than to
/// either name. This is free here; the walk opens every file to learn it.
#[test]
fn a_hardlinked_file_is_counted_once_and_shown_twice() {
    let mut volume = Volume::new();
    let target = volume.dir(build::ROOT_RECORD, "target");
    let deps = volume.dir(target, "deps");
    volume.add(
        Summary {
            in_use: true,
            has_data: true,
            logical: 1_000_000,
            allocated: 1_003_520,
            mtime: UNIX_2021,
            ..Summary::default()
        },
        &[(target, "libfoo.rlib"), (deps, "libfoo.rlib")],
    );

    // The default. Every record carries its own link count, so an MFT scan is
    // not paying an open per file the way the walk beside it would be — which
    // is what the front page has always claimed this strategy gives you.
    let deduped = volume.build(&ScanOptions::default());
    assert_eq!(deduped.total_allocated(), 1_003_520);
    assert_eq!(deduped.stats().hardlinks_deduped, 1);
    assert_eq!(
        deduped.stats().entries,
        5,
        "both names stay in the tree; only the bytes are counted once"
    );

    let counted_twice = volume.build(&ScanOptions {
        dedup_hardlinks: Some(false),
        ..ScanOptions::default()
    });
    assert_eq!(counted_twice.total_allocated(), 2 * 1_003_520);
}

/// A volume mounted inside this one has its files in a different MFT, so the
/// strategy cannot reach them however the cross-filesystem setting is left. It
/// says so rather than showing an empty folder.
#[test]
fn a_mounted_volume_is_named_as_a_boundary_rather_than_an_empty_folder() {
    let mut volume = Volume::new();
    volume.add(
        Summary {
            in_use: true,
            is_dir: true,
            attributes: 0x0400, // FILE_ATTRIBUTE_REPARSE_POINT
            is_mounted_volume: true,
            ..Summary::default()
        },
        &[(build::ROOT_RECORD, "backup")],
    );

    let tree = volume.build(&ScanOptions {
        cross_filesystems: true,
        ..ScanOptions::default()
    });
    let mount = tree.children(tree.root()).next().unwrap();
    let flags = tree.node(mount).flags;
    assert!(flags.contains(Flags::OTHER_DEVICE));
    assert!(
        !flags.contains(Flags::SYMLINK),
        "somebody's mounted 2 TB drive is a filesystem, not a link"
    );
    assert_eq!(tree.stats().skipped_other_device, 1);
}

/// A directory cycle cannot happen on a healthy volume and is an infinite tree
/// if it does.
#[test]
fn a_cycle_in_the_parent_references_terminates() {
    let mut volume = Volume::new();
    let a = volume.dir(build::ROOT_RECORD, "a");
    let b = volume.dir(a, "b");
    // `a` is now also inside `b`.
    let off = volume.table.name_bytes.len() as u32;
    volume.table.name_bytes.extend_from_slice(b"a");
    volume.table.names.push(Name {
        parent: b,
        off,
        len: 1,
        namespace: NAMESPACE_WIN32,
    });
    volume.table.owners.push(a);

    let tree = volume.build(&ScanOptions::default());
    assert_arena_invariants(&tree);
    assert!(tree.len() < 10, "the cycle was walked more than once");
}

#[test]
fn the_depth_ceiling_is_honoured() {
    let mut volume = Volume::new();
    let mut parent = build::ROOT_RECORD;
    for level in 0..6 {
        parent = volume.dir(parent, &format!("level{level}"));
    }

    let tree = volume.build(&ScanOptions {
        max_depth: Some(3),
        ..ScanOptions::default()
    });
    assert_eq!(tree.stats().max_depth, 3);
}

/// A name pointing at a record that is no longer in use is a directory index
/// that has not caught up with a deletion. Its bytes are already gone.
#[test]
fn a_name_for_a_deleted_record_is_not_a_file() {
    let mut volume = Volume::new();
    let record = volume.file(build::ROOT_RECORD, "gone.txt", 5_000);
    volume.table.records[record as usize].in_use = false;

    let tree = volume.build(&ScanOptions::default());
    assert_eq!(tree.stats().entries, 1);
    assert_eq!(tree.total_allocated(), 0);
}

/// A subdirectory scan reads the whole MFT and keeps the part below the record
/// it was pointed at, which is what makes `kosha C:\Users\me` work at all under
/// this strategy.
#[test]
fn a_scan_can_start_below_the_volume_root() {
    let mut volume = Volume::new();
    let users = volume.dir(build::ROOT_RECORD, "Users");
    let me = volume.dir(users, "me");
    volume.file(me, "notes.txt", 1_000);
    volume.file(build::ROOT_RECORD, "elsewhere.bin", 9_000_000);

    // Spelled for the platform running the test. `file_name` is what gives the
    // root its label, and on Linux a backslash is an ordinary character — so the
    // Windows spelling makes the whole string one component and the root comes
    // out labelled `C:\Users\me`.
    let below = if cfg!(windows) {
        std::path::Path::new(r"C:\Users\me")
    } else {
        std::path::Path::new("/Users/me")
    };
    let tree = build::build(
        &volume.table,
        Root {
            record: me,
            path: below,
        },
        &ScanOptions::default(),
        &volume.security,
        |_| {},
        || false,
    );
    assert_arena_invariants(&tree);
    assert_eq!(tree.stats().entries, 2);
    assert_eq!(tree.total_allocated(), 4_096);
    assert_eq!(tree.name_lossy(tree.root()), "me");
}

/// A cancel at a level boundary leaves a valid prefix, exactly as the walk
/// does — the tree renders, the totals are a floor, and it says so.
#[test]
fn a_cancelled_build_hands_back_a_partial_tree() {
    let mut volume = Volume::new();
    let dir = volume.dir(build::ROOT_RECORD, "deep");
    volume.file(dir, "a.txt", 1_000);

    let tree = build::build(
        &volume.table,
        Root {
            record: build::ROOT_RECORD,
            path: std::path::Path::new("C:\\"),
        },
        &ScanOptions::default(),
        &volume.security,
        |_| {},
        || true,
    );
    assert!(tree.stats().cancelled);
    assert!(tree.is_partial());
    assert_arena_invariants(&tree);
}

// ------------------------------------------------------------------- owners

/// The security id is in the long form of `$STANDARD_INFORMATION` and not in
/// the short one, and reading it out of a short attribute would pick up
/// whatever attribute happens to follow.
#[test]
fn the_security_id_is_read_only_from_the_long_standard_information() {
    let long = Record::file()
        .with(standard_information_with_security(
            FILETIME_2021,
            0x20,
            0x1234,
        ))
        .with(file_name(5, NAMESPACE_WIN32, "a.txt"))
        .read()
        .0
        .unwrap();
    assert_eq!(long.security_id, 0x1234);

    // The 0x30-byte form, which a volume upgraded from NTFS 1.2 still writes.
    let short = Record::file()
        .with(standard_information_short(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "a.txt"))
        .read()
        .0
        .unwrap();
    assert_eq!(
        short.security_id, 0,
        "a security id was invented for a record that has no field for one"
    );
}

/// The join the whole feature turns on: a record carries an id, `$Secure`
/// carries the owner, and the tree ends up knowing who owns each entry.
#[test]
fn records_are_attributed_through_the_security_table() {
    let mut volume = Volume::new();
    volume.owned_by(256, "S-1-5-21-1-2-3-1001");
    volume.owned_by(257, "S-1-5-18");

    let home = volume.dir(build::ROOT_RECORD, "Users");
    volume.file_owned(home, "mine.bin", 40_000, 256);
    volume.file_owned(home, "theirs.bin", 8_000, 257);
    volume.file_owned(build::ROOT_RECORD, "also-mine.bin", 4_000, 256);

    let tree = volume.build(&ScanOptions {
        collect_owners: true,
        ..ScanOptions::default()
    });

    let owners = tree.owners();
    assert!(owners.were_collected());
    assert_eq!(
        owners.people().len(),
        2,
        "two distinct security ids should give two owners"
    );

    let of = |names: &[&str]| owners.of(at(&tree, names)).map(|owner| owner.id.clone());
    assert_eq!(
        of(&["Users", "mine.bin"]),
        Some(OwnerId::Sid("S-1-5-21-1-2-3-1001".into()))
    );
    assert_eq!(
        of(&["Users", "theirs.bin"]),
        Some(OwnerId::Sid("S-1-5-18".into()))
    );
    assert_eq!(
        of(&["also-mine.bin"]),
        Some(OwnerId::Sid("S-1-5-21-1-2-3-1001".into()))
    );
}

/// A record whose security id is not in `$Secure` — or which has none at all,
/// which is what a record written by NTFS 1.2 looks like. Neither is "owned by
/// nobody": both are owners that could not be read, which is a row of its own.
#[test]
fn a_security_id_with_no_descriptor_is_unreadable_rather_than_unowned() {
    let mut volume = Volume::new();
    volume.owned_by(256, "S-1-5-21-1-2-3-1001");
    volume.file_owned(build::ROOT_RECORD, "known.bin", 4_000, 256);
    volume.file_owned(build::ROOT_RECORD, "missing.bin", 4_000, 999);
    volume.file(build::ROOT_RECORD, "none.bin", 4_000);

    let tree = volume.build(&ScanOptions {
        collect_owners: true,
        ..ScanOptions::default()
    });
    let owners = tree.owners();
    assert_eq!(owners.people().len(), 1);

    let of = |name: &str| owners.of(at(&tree, &[name])).is_some();
    assert!(of("known.bin"));
    assert!(!of("missing.bin"));
    assert!(!of("none.bin"));

    // And the bytes are still all there: the unreadable row carries them.
    let total: u64 = crate::owner::shares(&tree)
        .iter()
        .map(|s| s.allocated)
        .sum();
    assert_eq!(total, tree.total_allocated());
}

/// The table is four bytes an entry, so a scan that did not ask must not pay
/// for it — and must say it did not ask, rather than reporting every file as
/// having an unreadable owner.
#[test]
fn a_scan_that_did_not_ask_for_owners_collects_none() {
    let mut volume = Volume::new();
    volume.owned_by(256, "S-1-5-18");
    volume.file_owned(build::ROOT_RECORD, "a.bin", 4_000, 256);

    let tree = volume.build(&ScanOptions::default());
    assert!(!tree.owners().were_collected());
    assert!(tree.owners().people().is_empty());
    assert!(crate::owner::shares(&tree).is_empty());
}

// ------------------------------------------------- nothing may panic, ever

/// Every byte of a record, with one bit wrong.
///
/// The tests above break a record in the ways somebody thought of — a bad
/// signature, a deleted flag, a DOS name beside a Win32 one. This breaks it in
/// the ways nobody did: a flipped bit in every attribute header, every length,
/// every offset and every run of the data list, one at a time.
///
/// **These records come off a live volume.** The MFT is read as a raw stream
/// while Windows is writing to it, so a record caught mid-update is ordinary
/// rather than hostile — that is what the update sequence array in every
/// record is for, and why the reader applies it. A panic on one would take
/// down an elevated scan of somebody's system drive.
///
/// The same sweep over an ext4 image found a superblock field that asked for a
/// 68 GB allocation, which is an abort rather than an error. That is the shape
/// of thing this looks for.
#[test]
fn no_single_flipped_bit_can_panic_the_record_parser() {
    // A record with something of every kind in it: resident and non-resident
    // data, a name, timestamps, and a run list to walk.
    let pristine = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "report.pdf"))
        .with(data_nonresident(
            127_387,
            131_072,
            0,
            0,
            None,
            &[0x11, 0x20, 0x40, 0x00],
        ))
        .build();

    let mut tried = 0usize;
    for at in 0..pristine.len() {
        for bit in 0..8u8 {
            // A fresh copy each time: `parse` applies the update sequence
            // fixup in place, so a record cannot be reused even unmutated.
            let mut bytes = pristine.clone();
            bytes[at] ^= 1 << bit;

            let mut arena = Vec::new();
            let mut refs = Vec::new();
            // Either answer is correct. The assertion is reaching the next
            // line at all.
            let _ = record::parse(
                &mut bytes,
                &mut Names {
                    arena: &mut arena,
                    refs: &mut refs,
                },
            );
            tried += 1;
        }
    }
    assert_eq!(tried, pristine.len() * 8);
    assert!(tried > 8_000, "only {tried} mutations");
}

/// A record that stops early, which is what the last one in a truncated $MFT
/// read looks like.
#[test]
fn no_truncated_record_can_panic_the_parser() {
    let pristine = Record::file()
        .with(standard_information(FILETIME_2021, 0x20))
        .with(file_name(5, NAMESPACE_WIN32, "report.pdf"))
        .with(data_resident(64))
        .build();

    for length in 0..pristine.len() {
        let mut bytes = pristine[..length].to_vec();
        let mut arena = Vec::new();
        let mut refs = Vec::new();
        let _ = record::parse(
            &mut bytes,
            &mut Names {
                arena: &mut arena,
                refs: &mut refs,
            },
        );
    }
}

/// A whole NTFS volume, built byte by byte and read the way a disk image is.
///
/// The record tests above hand [`Table`] straight to the builder, which skips
/// the part that reads `$MFT` off a volume — the boot sector, the bootstrap
/// through record zero, the pass over every record. That part used to be
/// unreachable without a real disk and administrator rights. It is not any
/// more, and this is what says so on every platform.
#[cfg(test)]
pub(crate) mod volume_image {
    use super::*;

    pub(crate) const CLUSTER: usize = 4096;
    /// Where `$MFT` is put. Any cluster past the boot sector will do; a real
    /// volume puts it a long way in, and a small one here keeps the image
    /// small.
    const MFT_CLUSTER: u64 = 4;
    const TOTAL_CLUSTERS: u64 = 64;

    /// An NTFS boot sector `boot::parse` will accept.
    ///
    /// Written from the specification rather than copied off a disk, so the
    /// fields are the ones the parser reads and a mistake in either is a test
    /// failure rather than two mistakes agreeing.
    fn boot_sector(into: &mut [u8]) {
        into[0x03..0x0B].copy_from_slice(b"NTFS    ");
        put16(into, 0x0B, SECTOR as u16);
        into[0x0D] = (CLUSTER / SECTOR) as u8;
        // Total sectors, one short of the volume the way NTFS records it.
        put64(into, 0x28, TOTAL_CLUSTERS * (CLUSTER / SECTOR) as u64);
        put64(into, 0x30, MFT_CLUSTER);
        // Clusters per record, as the signed log2 NTFS uses for a record
        // smaller than a cluster: 1024 bytes is 2^-10, written as -10.
        into[0x40] = 0xF6;
    }

    /// A run list of one run: `clusters` starting at `start`.
    ///
    /// The compressed form NTFS uses — a header byte saying how many bytes each
    /// of the two numbers takes, then the numbers, then a zero to end the list.
    fn one_run(start: u64, clusters: u64) -> Vec<u8> {
        let mut list = vec![0x22u8];
        list.extend_from_slice(&(clusters as u16).to_le_bytes());
        list.extend_from_slice(&(start as u16).to_le_bytes());
        list.push(0);
        list
    }

    /// Lay out a volume whose `$MFT` holds `records`.
    ///
    /// Record 0 is written here rather than passed in: it is `$MFT`'s own
    /// record, and its `$DATA` run list is the bootstrap the whole strategy
    /// turns on — the extent map of the MFT is stored in the MFT.
    pub(crate) fn volume(records: Vec<Vec<u8>>) -> Vec<u8> {
        let mft_clusters = (records.len() + 1)
            .saturating_mul(RECORD_SIZE)
            .div_ceil(CLUSTER)
            .max(1) as u64;

        let mut image = vec![0u8; TOTAL_CLUSTERS as usize * CLUSTER];
        boot_sector(&mut image);

        let mft_at = MFT_CLUSTER as usize * CLUSTER;
        let zero = Record::file()
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "$MFT"))
            .with(data_nonresident(
                mft_clusters * CLUSTER as u64,
                mft_clusters * CLUSTER as u64,
                0,
                0,
                None,
                &one_run(MFT_CLUSTER, mft_clusters),
            ))
            .build();
        image[mft_at..mft_at + RECORD_SIZE].copy_from_slice(&zero);

        for (index, record) in records.iter().enumerate() {
            let at = mft_at + (index + 1) * RECORD_SIZE;
            image[at..at + record.len()].copy_from_slice(record);
        }
        image
    }

    /// A run list whose length field is eight bytes wide.
    ///
    /// [`one_run`] writes the two-byte form, which cannot say more than 65,535
    /// clusters. This is the form that says a terabyte of `$MFT` in eleven
    /// bytes, and eleven bytes is the whole point: what a number in a run list
    /// costs the reader that believes it.
    fn one_wide_run(start: u64, clusters: u64) -> Vec<u8> {
        // Low nibble: bytes of length. High nibble: bytes of offset.
        let mut list = vec![0x18u8];
        list.extend_from_slice(&clusters.to_le_bytes());
        list.push(start as u8);
        list.push(0);
        list
    }

    /// The same volume as [`volume`], with `$MFT`'s own run list written by the
    /// caller and, optionally, a total-sector count of the caller's choosing.
    ///
    /// `volume` computes an honest run list for the records it is given. This is
    /// for the numbers a volume can *claim* and not hold, which is every number
    /// in a boot sector and every number in a run list once the image came off
    /// the internet rather than out of `format`.
    fn volume_claiming(runs: &[u8], total_sectors: Option<u64>) -> Vec<u8> {
        let mut image = vec![0u8; TOTAL_CLUSTERS as usize * CLUSTER];
        boot_sector(&mut image);
        if let Some(sectors) = total_sectors {
            put64(&mut image, 0x28, sectors);
        }

        let mft_at = MFT_CLUSTER as usize * CLUSTER;
        let zero = Record::file()
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "$MFT"))
            .with(data_nonresident(
                CLUSTER as u64,
                CLUSTER as u64,
                0,
                0,
                None,
                runs,
            ))
            .build();
        image[mft_at..mft_at + RECORD_SIZE].copy_from_slice(&zero);
        // One ordinary record behind it, so that a reader which gets past the
        // claim has something real to read.
        let root = Record::directory().build();
        let at = mft_at + RECORD_SIZE;
        image[at..at + root.len()].copy_from_slice(&root);
        image
    }

    /// Somewhere to read the image from, which is all `Mft` asks for.
    struct Bytes(Vec<u8>);

    impl super::super::mft::Source for Bytes {
        fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> std::io::Result<()> {
            let at = offset as usize;
            let end = at + buffer.len();
            if end > self.0.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "past the end of the image",
                ));
            }
            buffer.copy_from_slice(&self.0[at..end]);
            Ok(())
        }

        fn len(&self) -> Option<u64> {
            Some(self.0.len() as u64)
        }
    }

    fn table_of(image: Vec<u8>) -> Table {
        let mut mft = super::super::mft::Mft::open(Bytes(image)).expect("an NTFS boot sector");
        let mut table = mft.read_table(|_| {}, || false).expect("a readable $MFT");
        build::consolidate(&mut table);
        table
    }

    /// `$MFT` says where `$MFT` is, in a run list eleven bytes long, and the
    /// reader sizes one table entry per record before it has read a record.
    ///
    /// A cluster count is a number off a disk like any other, and this one is
    /// multiplied rather than indexed with: clusters × bytes-per-cluster ÷
    /// bytes-per-record, capped only at `u32::MAX` records because that is what
    /// a node id can hold. Four billion of them is 192 GB of `Summary` and
    /// another 96 GB of name arena, asked for in one breath, from an image of
    /// 256 KiB — and `Geometry::total_clusters` has said "for bounds-checking a
    /// decoded run list" in its doc comment the whole time without anybody
    /// checking anything against it.
    #[test]
    fn an_mft_that_claims_more_clusters_than_the_volume_holds_is_refused() {
        let image = volume_claiming(&one_wide_run(MFT_CLUSTER, 1_000_000), None);
        let mut mft = super::super::mft::Mft::open(Bytes(image)).expect("an NTFS boot sector");
        let Err(error) = mft.read_table(|_| {}, || false) else {
            panic!("a million clusters of $MFT were read out of a 64-cluster volume");
        };
        assert!(
            error.to_string().contains("more clusters than the volume"),
            "refused, but not for the reason: {error}"
        );
    }

    /// And the volume's own size is a claim too.
    ///
    /// The check above holds the run list against the boot sector, so the way
    /// round it is to make the boot sector agree: 2^40 sectors is a 512 TiB
    /// volume and eight bytes of an image 256 KiB long. Then nothing in the
    /// volume contradicts anything else in it, every later multiplication is
    /// consistent, and the first thing to disagree is the medium — which the
    /// reader used to find out about by reading, one chunk at a time, long after
    /// the table had been reserved from the claim.
    ///
    /// Before the medium was asked how big it is, this test did not fail: it
    /// aborted the process on `memory allocation of 192000000000 bytes failed`,
    /// which the harness reports as the test binary disappearing and every other
    /// test in it going too.
    #[test]
    fn a_volume_that_claims_to_be_larger_than_its_image_is_refused_at_the_boot_sector() {
        let image = volume_claiming(&one_wide_run(MFT_CLUSTER, 1_000_000_000), Some(1 << 40));
        let Err(error) = super::super::mft::Mft::open(Bytes(image)) else {
            panic!("a 512 TiB volume was opened inside a 256 KiB image");
        };
        assert!(
            error.to_string().contains("larger than the medium"),
            "refused, but not for the reason: {error}"
        );
    }

    /// The whole path: boot sector, record zero's run list, every record after
    /// it, and a tree at the end with the right sizes in it.
    #[test]
    fn a_volume_image_reads_the_same_as_a_table_handed_over_directly() {
        let root = Record::directory()
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "."))
            .build();
        let big = Record::file()
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "big.bin"))
            .with(data_nonresident(
                100_000,
                102_400,
                0,
                0,
                None,
                &one_run(20, 25),
            ))
            .build();
        let small = Record::file()
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "small.txt"))
            .with(data_resident(64))
            .build();

        // Records 1..4 of the image: the root at its fixed number, then two
        // files. The root has to land on `build::ROOT_RECORD`, so the slots before it
        // are padding the way a real volume's metadata files are.
        let mut records: Vec<Vec<u8>> = (1..build::ROOT_RECORD)
            .map(|_| Record::deleted().build())
            .collect();
        records.push(root);
        records.push(big);
        records.push(small);

        let table = table_of(volume(records));
        let tree = build::build(
            &table,
            Root {
                record: build::ROOT_RECORD,
                path: std::path::Path::new("/image"),
            },
            &ScanOptions::default(),
            &security::Security::default(),
            |_: Progress| {},
            || false,
        );

        let mut found: Vec<(String, u64)> = (0..tree.len())
            .map(crate::NodeId::from_index)
            .filter(|&id| id != tree.root())
            .map(|id| (tree.name_lossy(id).to_string(), tree.node(id).allocated))
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                // `$MFT` is a file on the volume like any other, and the two
                // clusters it occupies here are two clusters of real disk. A
                // scan that hid it would be under-reporting by however large
                // the table is, which on a real drive is a gigabyte.
                ("$MFT".to_string(), 2 * CLUSTER as u64),
                ("big.bin".to_string(), 102_400),
                ("small.txt".to_string(), 64),
            ],
            "the pass over $MFT lost or mismeasured something"
        );
    }

    // ------------------------------------------------- through the scanner

    /// A few files under the root, at the record numbers a volume uses.
    fn a_small_volume() -> Vec<u8> {
        let mut records: Vec<Vec<u8>> = (1..build::ROOT_RECORD)
            .map(|_| Record::deleted().build())
            .collect();
        records.push(
            Record::directory()
                .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "."))
                .build(),
        );
        records.push(
            Record::file()
                .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "big.bin"))
                .with(data_nonresident(
                    100_000,
                    102_400,
                    0,
                    0,
                    None,
                    &one_run(20, 25),
                ))
                .build(),
        );
        volume(records)
    }

    fn scan_image(image: &[u8], name: &str) -> Result<crate::SizeTree, crate::ScanError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, image).unwrap();
        crate::Scanner::new(ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        })
        .scan(&path)
    }

    /// The whole way through: a file on disk, no container, no partition table,
    /// an NTFS volume inside it — and a tree out, with no device opened and
    /// nothing elevated.
    #[test]
    fn an_ntfs_image_scans_through_the_ordinary_entry_point() {
        let tree = scan_image(&a_small_volume(), "windows.img").expect("an NTFS image is an image");

        assert_eq!(
            tree.stats.strategy,
            crate::Strategy::DiskImage,
            "read from an image, not from a volume: saying `Master File Table` \
             would send somebody looking for a drive"
        );
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("NTFS"),
            "which filesystem was read is not something the strategy's name says"
        );
        assert!(
            (0..tree.len())
                .map(crate::NodeId::from_index)
                .any(|id| tree.name_lossy(id) == "big.bin"),
            "the files inside it are the point"
        );
    }

    /// The realistic shape of a whole-disk image: a partition table in front,
    /// and NTFS a megabyte in. Both halves of the note matter here — on a
    /// dual-boot disk, "partition 1 of 1" alone would not say which operating
    /// system's filesystem was measured.
    #[test]
    fn an_ntfs_partition_is_found_and_named() {
        use crate::partition::tests::{SECTOR, mbr_entry};

        let filesystem = a_small_volume();
        let start = 2048 * SECTOR;
        let mut disk = vec![0u8; start + filesystem.len() + SECTOR];
        disk[start..start + filesystem.len()].copy_from_slice(&filesystem);
        mbr_entry(
            &mut disk,
            0,
            0,
            0x07,
            2048,
            (filesystem.len() / SECTOR) as u32,
        );

        let tree = scan_image(&disk, "sda.img").expect("a partitioned NTFS disk is an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("NTFS · partition 1 of 1 (MBR)")
        );
    }

    /// The bootstrap is the part with no second chance: record zero says where
    /// every other record is, so a run list that does not decode has to be an
    /// error rather than an empty volume.
    #[test]
    fn a_broken_run_list_in_record_zero_is_an_error() {
        let mut image = volume(vec![Record::directory().build()]);
        // The run list sits at the tail of record zero's `$DATA`. Filling the
        // whole record with something that is not a record is the honest way to
        // break it without guessing at an offset.
        let mft_at = MFT_CLUSTER as usize * CLUSTER;
        image[mft_at..mft_at + RECORD_SIZE].fill(0);

        let mut mft = super::super::mft::Mft::open(Bytes(image)).expect("the boot sector is fine");
        assert!(
            mft.read_table(|_| {}, || false).is_err(),
            "record zero is the bootstrap; a broken one cannot read as empty"
        );
    }

    /// Not every file is an NTFS volume, and the answer has to be "no" rather
    /// than a tree of nonsense — this is what tells an image holding ext4 from
    /// one holding NTFS.
    #[test]
    fn something_that_is_not_ntfs_is_refused_at_the_boot_sector() {
        let mut image = volume(vec![Record::directory().build()]);
        image[0x03..0x0B].copy_from_slice(b"MSDOS5.0");
        assert!(super::super::mft::Mft::open(Bytes(image)).is_err());
    }

    /// A cancel during the pass has to stop it. The volume scan has nothing to
    /// hand back until the whole table is read, so this is the one strategy
    /// where a cancel is an error rather than a partial tree.
    #[test]
    fn a_cancel_during_the_pass_stops_it() {
        let image = volume(vec![Record::directory().build()]);
        let mut mft = super::super::mft::Mft::open(Bytes(image)).expect("an NTFS boot sector");
        match mft.read_table(|_| {}, || true) {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::Interrupted),
            Ok(_) => panic!("a cancelled pass must not finish"),
        }
    }
}

// ------------------------------------------------- a volume, byte for byte
//
// **Every other reader here is swept end to end and this one was not.** The
// sweep above stops at `record::parse`, so `mft.rs` — the bootstrap that finds
// $MFT through a run list stored inside $MFT itself — and `build.rs` — the
// counting sort, the parent chain, the hardlink set — were reachable by no
// damaged input at all. They are the code that runs elevated against a live
// system drive while Windows writes to it, which is the worst place in this
// project for a panic.
//
// So: a volume in memory, a boot sector and an MFT, read the way an image is.

/// 512-byte sectors, 4 KiB clusters, 1 KiB records: what `format` writes.
const VOLUME_CLUSTER: u64 = 4096;
const SECTORS_PER_CLUSTER: u8 = (VOLUME_CLUSTER / SECTOR as u64) as u8;
/// Where $MFT starts. Any cluster inside the volume and not zero, which the
/// boot parser refuses.
const MFT_AT: u64 = 4;
/// Eight records of a kilobyte: two clusters.
const MFT_CLUSTERS: u64 = 2;
const MFT_RECORDS: usize = (MFT_CLUSTERS * VOLUME_CLUSTER) as usize / RECORD_SIZE;
const VOLUME_CLUSTERS: u64 = 16;

/// A volume's bytes, so that the reader can be driven through the whole of
/// itself rather than through one record.
fn volume_image() -> Vec<u8> {
    let mft_bytes = MFT_CLUSTERS * VOLUME_CLUSTER;
    // One run of `MFT_CLUSTERS` clusters at `MFT_AT`: a header byte saying one
    // byte of length and one of offset, then the two, then the terminator.
    let runs = [0x11u8, MFT_CLUSTERS as u8, MFT_AT as u8, 0x00];

    let mut records: Vec<Vec<u8>> = Vec::new();
    // Record 0 is $MFT, and its own extent map is the bootstrap the whole
    // strategy turns on.
    records.push(
        Record::file()
            .with(standard_information(FILETIME_2021, 0x06))
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "$MFT"))
            .with(data_nonresident(mft_bytes, mft_bytes, 0, 0, None, &runs))
            .build(),
    );
    // 1 to 4 are NTFS' own and are not interesting here; they have to be
    // *something*, because a hole in the table is a different test.
    for name in ["$MFTMirr", "$LogFile", "$Volume", "$AttrDef"] {
        records.push(
            Record::file()
                .with(standard_information(FILETIME_2021, 0x06))
                .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, name))
                .with(data_resident(8))
                .build(),
        );
    }
    // 5 is the root, which has no name of its own here: the caller supplies it.
    records.push(Record::directory().build());
    // And a file, a directory, and a file inside that directory, so the walk
    // has a level to descend and a parent chain to get wrong.
    records.push(
        Record::file()
            .with(standard_information(FILETIME_2021, 0x20))
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "report.pdf"))
            .with(data_nonresident(
                127_387,
                131_072,
                0,
                0,
                None,
                &[0x11, 0x20, 0x08, 0x00],
            ))
            .build(),
    );
    records.push(
        Record::directory()
            .with(standard_information(FILETIME_2021, 0x10))
            .with(file_name(build::ROOT_RECORD, NAMESPACE_WIN32, "docs"))
            .build(),
    );
    assert!(
        records.len() <= MFT_RECORDS,
        "the MFT is too small for these"
    );

    let mut image = vec![0u8; (VOLUME_CLUSTERS * VOLUME_CLUSTER) as usize];
    image[..SECTOR].copy_from_slice(&boot_sector());
    let mut at = (MFT_AT * VOLUME_CLUSTER) as usize;
    for record in &records {
        image[at..at + record.len()].copy_from_slice(record);
        at += RECORD_SIZE;
    }
    image
}

fn boot_sector() -> Vec<u8> {
    let mut sector = vec![0u8; SECTOR];
    sector[0x03..0x0B].copy_from_slice(b"NTFS    ");
    put16(&mut sector, 0x0B, SECTOR as u16);
    sector[0x0D] = SECTORS_PER_CLUSTER;
    put64(
        &mut sector,
        0x28,
        VOLUME_CLUSTERS * VOLUME_CLUSTER / SECTOR as u64,
    );
    put64(&mut sector, 0x30, MFT_AT);
    // A record size below a cluster is written as the negative of its log2,
    // which is how every volume `format` makes describes its 1 KiB records.
    sector[0x40] = (-10i8) as u8;
    sector
}

/// The volume's bytes, as something `Mft::open` will read from.
struct InMemory(Vec<u8>);

impl super::mft::Source for InMemory {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> std::io::Result<()> {
        let at = offset as usize;
        // Short of the end is what a read past a volume's last sector is, and
        // the reader has to meet it rather than be handed zeros forever.
        if at >= self.0.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "past the end of the volume",
            ));
        }
        let take = buffer.len().min(self.0.len() - at);
        buffer[..take].copy_from_slice(&self.0[at..at + take]);
        buffer[take..].fill(0);
        Ok(())
    }

    fn len(&self) -> Option<u64> {
        Some(self.0.len() as u64)
    }
}

/// The same bytes, from a source that does not know how long it is.
///
/// Which is what a live volume is: [`InMemory`] answers `len`, the way a
/// partition inside an image does, and `Volume` does not. The two are meant to
/// behave differently — a length is the one piece of ground truth about a
/// volume, and with it a boot sector that overstates the disk is refused before
/// anything is multiplied by it — so with no length the rest of the reader has
/// to survive the same damage on its own. Every sweep below runs both ways, and
/// this side is the elevated path that no test could reach before.
struct Unmeasured(InMemory);

impl super::mft::Source for Unmeasured {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> std::io::Result<()> {
        self.0.read_exact_at(offset, buffer)
    }
}

/// Everything the image reader does, from bytes to a tree.
///
/// The same four calls in the same order as `image::read_ntfs`, so that what is
/// sweeping is the path a disk image really takes and not a shorter one written
/// for a test.
fn tree_from_image(bytes: Vec<u8>) -> std::io::Result<crate::SizeTree> {
    tree_from(InMemory(bytes))
}

/// The same, read by something that cannot say how big it is.
fn tree_from_unmeasured(bytes: Vec<u8>) -> std::io::Result<crate::SizeTree> {
    tree_from(Unmeasured(InMemory(bytes)))
}

fn tree_from(source: impl super::mft::Source) -> std::io::Result<crate::SizeTree> {
    let mut mft = super::mft::Mft::open(source)?;
    let mut table = mft.read_table(|_| {}, || false)?;
    build::consolidate(&mut table);
    Ok(build::build(
        &table,
        Root {
            record: build::ROOT_RECORD,
            path: std::path::Path::new("C:\\"),
        },
        &ScanOptions::default(),
        &security::Security::default(),
        |_: Progress| {},
        || false,
    ))
}

/// The fixture is a volume the reader reads, before anything is broken in it.
#[test]
fn a_volume_of_bytes_reads_as_the_tree_it_describes() {
    let tree = tree_from_image(volume_image()).expect("the volume reads");
    let names: Vec<String> = tree
        .children(tree.root())
        .map(|id| tree.name_lossy(id).to_string())
        .collect();
    assert!(names.contains(&"report.pdf".to_string()), "{names:?}");
    assert!(names.contains(&"docs".to_string()), "{names:?}");
    assert!(names.contains(&"$MFT".to_string()), "{names:?}");
    assert_eq!(
        tree.node(at(&tree, &["report.pdf"])).allocated,
        131_072,
        "the non-resident size comes off the attribute header"
    );
    assert_arena_invariants(&tree);
}

/// Every bit of the boot sector and of the MFT, flipped one at a time.
///
/// **Reaching `mft.rs` and `build.rs`, which the record sweep above cannot.**
/// A flipped bit here is a cluster size that does not divide, an MFT that
/// claims to live outside the volume, a run list that decodes to an offset
/// past the end, a parent that points at a record which is not a directory, a
/// link count that disagrees with the names — every one of those is an
/// arithmetic operation on a number that came off a disk, and this reader does
/// its arithmetic against a volume Windows is writing to at the time.
///
/// Either answer is correct: a tree, or an error. The assertion is reaching the
/// next line at all, and the arena invariants holding for whatever tree comes
/// back — a tree that renders and sorts and is wrong is worse than an error.
#[test]
fn no_single_flipped_bit_of_a_volume_can_panic_the_reader() {
    let pristine = volume_image();
    // The boot sector, and the MFT. The rest of the volume is the files' own
    // bytes, which this reader never looks at.
    let regions = [
        (0usize, SECTOR),
        (
            (MFT_AT * VOLUME_CLUSTER) as usize,
            (MFT_CLUSTERS * VOLUME_CLUSTER) as usize,
        ),
    ];

    let mut tried = 0usize;
    let mut trees = 0usize;
    // Twice over: as a partition inside an image, whose length the reader knows,
    // and as a volume whose length it does not. See [`Unmeasured`].
    for measured in [true, false] {
        for (start, length) in regions {
            for at in start..start + length {
                for bit in 0..8u8 {
                    let mut bytes = pristine.clone();
                    bytes[at] ^= 1 << bit;
                    let outcome = if measured {
                        tree_from_image(bytes)
                    } else {
                        tree_from_unmeasured(bytes)
                    };
                    if let Ok(tree) = outcome {
                        assert_arena_invariants(&tree);
                        trees += 1;
                    }
                    tried += 1;
                }
            }
        }
    }
    assert_eq!(
        tried,
        (SECTOR + (MFT_CLUSTERS * VOLUME_CLUSTER) as usize) * 8 * 2
    );
    // Both outcomes have to occur, or the sweep is measuring one branch. Most
    // flips land in a record's slack and change nothing at all.
    assert!(trees > 1000, "only {trees} of {tried} produced a tree");
    assert!(trees < tried, "nothing was refused, so nothing was checked");
}

/// A volume that stops early, which is what a copy that ran out of room is.
#[test]
fn no_truncation_of_a_volume_can_panic_the_reader() {
    let pristine = volume_image();
    let mut cut = 0usize;
    let mut refused = 0usize;
    // Every sector boundary, because a short read is a whole number of them,
    // and one byte either side of each to catch the arithmetic that assumes it.
    for end in (0..pristine.len()).step_by(SECTOR / 4) {
        let mut bytes = pristine.clone();
        bytes.truncate(end);
        cut += 1;
        match tree_from_image(bytes) {
            Ok(tree) => assert_arena_invariants(&tree),
            Err(_) => refused += 1,
        }
    }
    // Every one of them, because a truncated volume is a volume shorter than
    // the sector count its own boot sector states, and that is now a refusal
    // before a cluster of it is read. A download that stopped early says so
    // rather than producing part of a tree.
    assert_eq!(refused, cut, "a volume shorter than it says it is was read");

    // And the same cuts where the length is unknown, which is the live volume:
    // there the reader has nothing to compare against and has to walk into the
    // damage. Both answers are correct; neither may panic.
    let mut trees = 0usize;
    let mut errors = 0usize;
    for end in (0..pristine.len()).step_by(SECTOR / 4) {
        let mut bytes = pristine.clone();
        bytes.truncate(end);
        match tree_from_unmeasured(bytes) {
            Ok(tree) => {
                assert_arena_invariants(&tree);
                trees += 1;
            }
            Err(_) => errors += 1,
        }
    }
    assert!(errors > 0, "a volume of no bytes has to be refused");
    assert!(
        trees > 0,
        "not one truncation got past the boot sector, so the sweep stopped there"
    );
}
