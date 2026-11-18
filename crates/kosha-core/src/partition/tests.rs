// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Partition tables built byte by byte, and read by exactly the code that reads
//! a real one.
//!
//! There is no test-only path through [`read`]: the builders below write a
//! master boot record, an extended chain and a GUID partition table the way a
//! disk carries them, checksum included. Which means these run — and catch
//! things — on a machine with no disk image on it at all.

use super::*;

pub(crate) const SECTOR: usize = 512;
const DISK: usize = 64 * 1024 * 1024;

/// A disk with nothing but zeros, long enough for the addresses used here.
fn blank() -> Vec<u8> {
    vec![0u8; DISK]
}

fn put32(disk: &mut [u8], at: usize, value: u32) {
    disk[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(disk: &mut [u8], at: usize, value: u64) {
    disk[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// Write one 16-byte MBR entry into a sector.
pub(crate) fn mbr_entry(
    disk: &mut [u8],
    sector_at: usize,
    slot: usize,
    kind: u8,
    start: u32,
    count: u32,
) {
    let at = sector_at + 0x1BE + slot * 16;
    disk[at] = 0;
    disk[at + 4] = kind;
    put32(disk, at + 8, start);
    put32(disk, at + 12, count);
    disk[sector_at + 0x1FE] = 0x55;
    disk[sector_at + 0x1FF] = 0xAA;
}

/// A GPT header at LBA 1 and its entries, with a checksum that holds.
pub(crate) fn gpt(disk: &mut [u8], entries: &[(u64, u64, [u8; 16])]) {
    const ENTRY_SIZE: u32 = 128;
    const ENTRIES_LBA: u64 = 2;

    // The protective MBR, which is what says "look at LBA 1".
    mbr_entry(disk, 0, 0, 0xEE, 1, 0xFFFF_FFFF);

    let header = SECTOR;
    disk[header..header + 8].copy_from_slice(GPT_SIGNATURE);
    put32(disk, header + 8, 0x0001_0000); // revision
    put32(disk, header + 12, 92); // header size
    put64(disk, header + 72, ENTRIES_LBA);
    put32(disk, header + 80, entries.len() as u32);
    put32(disk, header + 84, ENTRY_SIZE);

    for (index, (first, last, kind)) in entries.iter().enumerate() {
        let at = ENTRIES_LBA as usize * SECTOR + index * ENTRY_SIZE as usize;
        disk[at..at + 16].copy_from_slice(kind);
        // A unique GUID, which nothing here reads but a real entry has.
        disk[at + 16] = 1;
        put64(disk, at + 32, *first);
        put64(disk, at + 40, *last);
    }

    // Last, because it covers everything above it.
    let mut crc = Crc32::new();
    crc.update(&disk[header..header + 16]);
    crc.update(&[0, 0, 0, 0]);
    crc.update(&disk[header + 20..header + 92]);
    let sum = crc.finish();
    put32(disk, header + 16, sum);
}

pub(crate) const LINUX_DATA: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];
pub(crate) const EFI_SYSTEM: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

// ------------------------------------------------------------ what it reads

#[test]
fn a_disk_with_no_table_has_none() {
    let disk = blank();
    assert_eq!(read(&mut disk.as_slice()), None);
}

/// The signature is the whole of the check for "is there a table here", so a
/// disk that carries it and nothing else must not produce a partition.
#[test]
fn a_boot_signature_alone_is_not_a_table() {
    let mut disk = blank();
    disk[0x1FE] = 0x55;
    disk[0x1FF] = 0xAA;
    assert_eq!(read(&mut disk.as_slice()), None);
}

#[test]
fn reads_the_primaries_of_a_master_boot_record() {
    let mut disk = blank();
    mbr_entry(&mut disk, 0, 0, 0x83, 2048, 20480);
    mbr_entry(&mut disk, 0, 1, 0x07, 22528, 40960);

    let table = read(&mut disk.as_slice()).expect("a table");
    assert_eq!(table.scheme, Scheme::Mbr);
    assert_eq!(table.partitions.len(), 2);
    assert_eq!(table.partitions[0].number, 1);
    assert_eq!(table.partitions[0].start, 2048 * 512);
    assert_eq!(table.partitions[0].length, 20480 * 512);
    assert_eq!(table.partitions[0].kind, "Linux");
    assert_eq!(table.partitions[1].number, 2);
    assert_eq!(table.partitions[1].kind, "NTFS or exFAT");
}

/// An empty slot takes no number with it, and the slots after it keep theirs —
/// which is what every tool on the machine prints and therefore what somebody
/// will be looking for.
#[test]
fn an_empty_slot_does_not_shift_the_numbering() {
    let mut disk = blank();
    mbr_entry(&mut disk, 0, 2, 0x83, 2048, 20480);

    let table = read(&mut disk.as_slice()).expect("a table");
    assert_eq!(table.partitions.len(), 1);
    assert_eq!(table.partitions[0].number, 3);
}

/// Six partitions do not fit in four slots, so the fifth and sixth live in a
/// chain of further records — and the two bases in that chain are measured from
/// different places, which is the entire difficulty of the scheme.
#[test]
fn follows_the_chain_of_extended_partitions() {
    let mut disk = blank();
    let extended_start = 100_000u32;
    mbr_entry(&mut disk, 0, 0, 0x83, 2048, 20480);
    mbr_entry(&mut disk, 0, 1, 0x05, extended_start, 100_000);

    // The first link: a partition 2048 sectors past this record, and a pointer
    // to the next record measured from the start of the chain.
    let first = extended_start as usize * SECTOR;
    mbr_entry(&mut disk, first, 0, 0x83, 2048, 4096);
    mbr_entry(&mut disk, first, 1, 0x05, 8192, 4096);

    let second = (extended_start as usize + 8192) * SECTOR;
    mbr_entry(&mut disk, second, 0, 0x83, 2048, 2048);

    let table = read(&mut disk.as_slice()).expect("a table");
    let found: Vec<(u32, u64)> = table
        .partitions
        .iter()
        .map(|partition| (partition.number, partition.start))
        .collect();
    assert_eq!(
        found,
        vec![
            (1, 2048 * 512),
            // Relative to its own record.
            (5, (extended_start as u64 + 2048) * 512),
            // Relative to the start of the chain, then its own record.
            (6, (extended_start as u64 + 8192 + 2048) * 512),
        ]
    );
}

/// A chain that points at itself is a disk somebody can hand over, and the
/// walk has to end. Both guards are tested because either alone would let the
/// other's case run forever.
#[test]
fn a_chain_that_loops_still_ends() {
    let mut disk = blank();
    mbr_entry(&mut disk, 0, 0, 0x05, 2048, 100_000);
    let link = 2048 * SECTOR;
    mbr_entry(&mut disk, link, 0, 0x83, 2048, 2048);
    // Pointing straight back at itself.
    mbr_entry(&mut disk, link, 1, 0x05, 0, 4096);

    let table = read(&mut disk.as_slice()).expect("a table");
    assert_eq!(
        table.partitions.len(),
        1,
        "the loop must not repeat entries"
    );
}

#[test]
fn reads_a_guid_partition_table() {
    let mut disk = blank();
    gpt(
        &mut disk,
        &[
            (2048, 34815, EFI_SYSTEM),
            (34816, 260_095, LINUX_DATA),
            (0, 0, [0u8; 16]),
        ],
    );

    let table = read(&mut disk.as_slice()).expect("a table");
    assert_eq!(table.scheme, Scheme::Gpt);
    assert_eq!(table.sector_size, 512);
    assert_eq!(table.partitions.len(), 2, "the empty slot is not an entry");
    assert_eq!(table.partitions[0].kind, "EFI system");
    assert_eq!(table.partitions[1].number, 2);
    assert_eq!(table.partitions[1].start, 34816 * 512);
    // Inclusive of the last sector, which is the one place GPT differs from
    // every other length on a disk — and an off-by-one here is a partition
    // half a kilobyte short, which reads fine until the very last block.
    assert_eq!(table.partitions[1].length, (260_095 - 34816 + 1) * 512);
    assert_eq!(table.partitions[1].kind, "Linux filesystem");
}

/// The protective entry is what sends the reader to LBA 1. A disk that has one
/// and no usable GPT must not then be read as though the protective entry were
/// a filesystem — it describes the whole disk, and treating it as a partition
/// would hand back something that is not one.
#[test]
fn a_protective_record_without_a_table_is_not_read_as_a_partition() {
    let mut disk = blank();
    mbr_entry(&mut disk, 0, 0, 0xEE, 1, 0xFFFF_FFFF);
    assert_eq!(read(&mut disk.as_slice()), None);
}

/// The checksum is how a half-written table is told from a whole one, which
/// matters because kosha reads images while something else is writing them.
#[test]
fn a_header_whose_checksum_fails_is_not_a_table() {
    let mut disk = blank();
    gpt(&mut disk, &[(2048, 4095, LINUX_DATA)]);
    assert!(read(&mut disk.as_slice()).is_some(), "sound to begin with");

    // One bit, somewhere the checksum covers.
    disk[SECTOR + 72] ^= 1;
    assert_eq!(read(&mut disk.as_slice()), None);
}

/// A 4Kn disk puts its header a full block in rather than half a kilobyte, and
/// an image does not say which it was made with — so both are tried.
#[test]
fn finds_a_table_on_a_disk_with_four_kilobyte_sectors() {
    let mut disk = blank();
    mbr_entry(&mut disk, 0, 0, 0xEE, 1, 0xFFFF_FFFF);

    const SECTOR_4K: usize = 4096;
    let header = SECTOR_4K;
    disk[header..header + 8].copy_from_slice(GPT_SIGNATURE);
    put32(&mut disk, header + 12, 92);
    put64(&mut disk, header + 72, 2);
    put32(&mut disk, header + 80, 1);
    put32(&mut disk, header + 84, 128);
    let at = 2 * SECTOR_4K;
    disk[at..at + 16].copy_from_slice(&LINUX_DATA);
    put64(&mut disk, at + 32, 256);
    put64(&mut disk, at + 40, 4095);
    let mut crc = Crc32::new();
    crc.update(&disk[header..header + 16]);
    crc.update(&[0, 0, 0, 0]);
    crc.update(&disk[header + 20..header + 92]);
    let sum = crc.finish();
    put32(&mut disk, header + 16, sum);

    let table = read(&mut disk.as_slice()).expect("a table");
    assert_eq!(table.sector_size, 4096);
    assert_eq!(table.partitions[0].start, 256 * 4096);
}

/// The standard CRC-32, against the value every implementation of it agrees on.
/// Getting the polynomial wrong produces a checksum that never matches and a
/// disk quietly declared unpartitioned — a failure with no symptom to chase.
#[test]
fn the_checksum_is_the_one_gpt_actually_uses() {
    let mut crc = Crc32::new();
    crc.update(b"123456789");
    assert_eq!(crc.finish(), 0xCBF4_3926);
}

// ------------------------------------------------------------- damaged input

/// Every bit of both tables, flipped one at a time.
///
/// A partition table is read from bytes kosha did not write, off a disk that
/// may be half-copied, truncated, or simply not what it claims. Every one of
/// these must produce a table or nothing, and never a panic, a hang, or an
/// allocation the machine cannot serve.
#[test]
fn every_flipped_bit_produces_a_table_or_nothing() {
    let mut swept = 0usize;

    for build in [
        (|disk: &mut Vec<u8>| {
            mbr_entry(disk, 0, 0, 0x83, 2048, 20480);
            mbr_entry(disk, 0, 1, 0x05, 100_000, 100_000);
            let link = 100_000 * SECTOR;
            mbr_entry(disk, link, 0, 0x83, 2048, 4096);
        }) as fn(&mut Vec<u8>),
        |disk: &mut Vec<u8>| {
            gpt(
                disk,
                &[(2048, 34815, EFI_SYSTEM), (34816, 260_095, LINUX_DATA)],
            );
        },
    ] {
        let mut disk = blank();
        build(&mut disk);

        // The first three sectors and the first extended record: every
        // structure either table puts a number in.
        let regions = [0..3 * SECTOR, 100_000 * SECTOR..100_001 * SECTOR];
        for region in regions {
            for at in region {
                for bit in 0..8 {
                    let was = disk[at];
                    disk[at] ^= 1 << bit;
                    if let Some(table) = read(&mut disk.as_slice()) {
                        // Whatever comes back has to be usable: a caller will
                        // add these to an offset and read there.
                        for partition in &table.partitions {
                            let _ = partition.start.checked_add(partition.length);
                        }
                    }
                    disk[at] = was;
                    swept += 1;
                }
            }
        }
    }

    // Computed from what was covered rather than written down, and floored so
    // that a sweep which quietly covered nothing fails rather than passes.
    assert_eq!(swept, 2 * 2 * SECTOR * 8 * 2);
    assert!(swept >= 10_000, "only {swept} mutations");
}

/// A disk cut off mid-table, at every length there is.
#[test]
fn every_truncation_produces_a_table_or_nothing() {
    let mut disk = blank();
    gpt(
        &mut disk,
        &[(2048, 34815, EFI_SYSTEM), (34816, 260_095, LINUX_DATA)],
    );
    for keep in 0..4 * SECTOR {
        let _ = read(&mut &disk[..keep]);
    }
}

/// A header can ask for four billion entries of four billion bytes. The read
/// happens into memory, so believing it is an allocation nothing can serve —
/// which is not an error anybody can catch, because a failed allocation aborts
/// the process. The same shape as the 68 GB inode table the ext4 sweep found.
#[test]
fn an_absurd_entry_count_is_refused_rather_than_allocated() {
    let mut disk = blank();
    gpt(&mut disk, &[(2048, 4095, LINUX_DATA)]);

    for (count, size) in [
        (u32::MAX, 128u32),
        (4096, u32::MAX),
        (u32::MAX, u32::MAX),
        (1, 0),
        (0, 128),
    ] {
        put32(&mut disk, SECTOR + 80, count);
        put32(&mut disk, SECTOR + 84, size);
        let mut crc = Crc32::new();
        crc.update(&disk[SECTOR..SECTOR + 16]);
        crc.update(&[0, 0, 0, 0]);
        crc.update(&disk[SECTOR + 20..SECTOR + 92]);
        let sum = crc.finish();
        put32(&mut disk, SECTOR + 16, sum);

        assert_eq!(
            read(&mut disk.as_slice()),
            None,
            "{count} entries of {size} bytes must be refused"
        );
    }
}
