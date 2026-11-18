// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Where the filesystems are on a disk that holds more than one.
//!
//! An image of a whole disk does not begin with a filesystem. It begins with a
//! partition table, and the filesystem starts a megabyte in — which is why
//! `dd if=/dev/sda of=disk.img` produced a file kosha could see nothing inside
//! while `dd if=/dev/sda2` produced one it could read completely. The
//! difference is this module.
//!
//! ## The two schemes
//!
//! ```text
//! MBR   sector 0      0x1BE  four 16-byte entries, 0x55AA at 0x1FE
//! GPT   sector 0      a protective MBR, one entry of type 0xEE
//!       sector 1      "EFI PART", a CRC, and where the entries are
//!       sector 2...   the entries themselves, usually 128 of 128 bytes
//! ```
//!
//! Both are read, because both are what people have: GPT on anything made this
//! decade, MBR on everything older and on most disk images that a virtual
//! machine ever wrote. MBR's extended partitions are followed too — they are a
//! linked list of further tables, and an image with six partitions on it has
//! its sixth one there rather than in the four slots the scheme allows.
//!
//! ## What this does not decide
//!
//! Which partition somebody meant. This hands back what is on the disk, in the
//! order the table lists it, and says nothing about which one holds anything
//! worth reading — that is the caller's question, because only the caller knows
//! what it can read. A partition table is a fact; picking one is a judgement.
//!
//! Bytes in and numbers out, with no platform call anywhere, so it is under
//! test on whichever machine it is being edited on — the same arrangement as
//! [`vhdx`](crate::vhdx) and the `ntfs` modules.

use crate::vhdx::ReadAt;

/// The largest sector a disk addresses in, and the smallest.
///
/// Both are tried, because an image does not say which it was made with and the
/// answer changes where LBA 1 is. 512 is what nearly everything uses, including
/// every 4Kn disk's *logical* sector; 4096 is what a handful of native-4K
/// devices report, and an image taken from one has its GPT header a full block
/// in rather than half a kilobyte.
const SECTOR_SIZES: [u64; 2] = [512, 4096];

/// How many entries of a GPT to believe.
///
/// The specification says 128 and allows more. The bound exists because the
/// count comes off the disk: a flipped bit in a `u32` asks for four billion
/// entries, and the array is read into memory before anything looks at it.
const MAX_GPT_ENTRIES: u32 = 4096;

/// How far to follow a chain of extended partitions.
///
/// Each link is a sector somewhere on the disk saying where the next one is, so
/// a corrupt or malicious image can point one at another in a circle. Visited
/// offsets are remembered as well, which catches the simple loop; this catches
/// the long one that never repeats an address.
const MAX_EXTENDED_LINKS: usize = 256;

/// Which scheme a disk was partitioned with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scheme {
    Mbr,
    Gpt,
}

impl Scheme {
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Mbr => "MBR",
            Scheme::Gpt => "GPT",
        }
    }
}

/// One partition, in bytes rather than in sectors.
///
/// Sectors are the table's unit and nobody else's. Converting here means the
/// rest of the program never has to know what size a sector was, and never has
/// to be given the chance to get it wrong.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Partition {
    /// What a person would call it: `1` for `/dev/sda1`, and the same number
    /// `sfdisk` prints. Not an index into the vector — an extended partition
    /// takes a slot in the table and produces no entry here, and logical
    /// partitions are numbered from five whatever the primaries did.
    pub number: u32,
    pub start: u64,
    pub length: u64,
    /// What the table claims is in it. A hint for a person reading a list, and
    /// never a reason to skip one: the type byte on a disk that has been
    /// repartitioned is very often a lie, and opening the filesystem is the
    /// only way to know.
    pub kind: &'static str,
}

/// What was found on a disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Table {
    pub scheme: Scheme,
    pub sector_size: u64,
    pub partitions: Vec<Partition>,
}

/// Read whatever partition table a disk begins with.
///
/// `None` where there is not one, which is the ordinary case for the image of a
/// single filesystem — `mkfs` on a file, a WSL distribution's `ext4.vhdx`, a
/// `dd` of one partition rather than of the disk holding it.
pub fn read(source: &mut impl ReadAt) -> Option<Table> {
    let mut sector = [0u8; 512];
    source.read_exact_at(0, &mut sector).ok()?;
    if u16::from_le_bytes([sector[510], sector[511]]) != 0xAA55 {
        return None;
    }

    // A GPT disk carries an MBR too, holding one entry of type 0xEE that spans
    // the disk — so that a tool which knows only MBR sees the space as taken
    // rather than as free. Its presence is the signal to look at LBA 1, and a
    // disk whose GPT is unreadable is *not* then read as an MBR: the protective
    // entry describes no filesystem, and treating it as one would hand back a
    // partition that is really the whole disk.
    if is_protective(&sector) {
        for sector_size in SECTOR_SIZES {
            if let Some(table) = read_gpt(source, sector_size) {
                return Some(table);
            }
        }
        return None;
    }

    read_mbr(source, &sector)
}

// ---------------------------------------------------------------------- MBR

/// Type bytes that mean "this entry is a chain, not a filesystem".
const EXTENDED_TYPES: [u8; 3] = [0x05, 0x0F, 0x85];

fn is_protective(sector: &[u8; 512]) -> bool {
    // Exactly one used entry, and it is the protective one. Checking every slot
    // rather than only the first, because a disk converted from MBR to GPT by
    // some tools leaves the protective entry in a later slot.
    (0..4).any(|slot| sector[0x1BE + slot * 16 + 4] == 0xEE)
}

fn read_mbr(source: &mut impl ReadAt, sector: &[u8; 512]) -> Option<Table> {
    // MBR is defined in 512-byte sectors and in practice never uses anything
    // else: the entries are 32-bit LBAs, and a disk large enough to need 4K
    // sectors is a disk that has outgrown the scheme.
    const SECTOR: u64 = 512;
    let mut partitions = Vec::new();
    let mut chains = Vec::new();

    for slot in 0..4 {
        let entry = &sector[0x1BE + slot * 16..0x1BE + slot * 16 + 16];
        let kind = entry[4];
        let start = u64::from(u32::from_le_bytes([
            entry[8], entry[9], entry[10], entry[11],
        ]));
        let count = u64::from(u32::from_le_bytes([
            entry[12], entry[13], entry[14], entry[15],
        ]));
        if kind == 0 || count == 0 {
            continue;
        }
        if EXTENDED_TYPES.contains(&kind) {
            chains.push(start * SECTOR);
            continue;
        }
        // A 32-bit LBA times 512 fits a `u64` with room to spare, so unlike the
        // GPT case above this cannot overflow — which is worth saying, because
        // the two look identical and only one of them needs the check.
        partitions.push(Partition {
            number: slot as u32 + 1,
            start: start * SECTOR,
            length: count * SECTOR,
            kind: mbr_kind(kind),
        });
    }

    // Logical partitions are numbered from five whatever the primaries did,
    // which is the numbering every tool on a Linux system prints.
    let mut number = 5;
    for base in chains {
        follow_extended(source, base, &mut number, &mut partitions);
    }

    (!partitions.is_empty()).then_some(Table {
        scheme: Scheme::Mbr,
        sector_size: SECTOR,
        partitions,
    })
}

/// Walk the linked list of extended boot records.
///
/// Each one holds at most two useful entries: a partition, whose start is
/// relative to *that record*, and a link to the next record, whose start is
/// relative to the *first* extended record. Getting those two bases the same
/// way round is the whole difficulty of the scheme.
fn follow_extended(
    source: &mut impl ReadAt,
    base: u64,
    number: &mut u32,
    partitions: &mut Vec<Partition>,
) {
    const SECTOR: u64 = 512;
    let mut at = base;
    let mut seen = Vec::new();

    for _ in 0..MAX_EXTENDED_LINKS {
        if seen.contains(&at) {
            return;
        }
        seen.push(at);

        let mut sector = [0u8; 512];
        if source.read_exact_at(at, &mut sector).is_err() {
            return;
        }
        if u16::from_le_bytes([sector[510], sector[511]]) != 0xAA55 {
            return;
        }

        let mut next = None;
        for slot in 0..4 {
            let entry = &sector[0x1BE + slot * 16..0x1BE + slot * 16 + 16];
            let kind = entry[4];
            let start = u64::from(u32::from_le_bytes([
                entry[8], entry[9], entry[10], entry[11],
            ]));
            let count = u64::from(u32::from_le_bytes([
                entry[12], entry[13], entry[14], entry[15],
            ]));
            if kind == 0 || count == 0 {
                continue;
            }
            if EXTENDED_TYPES.contains(&kind) {
                next = Some(base + start * SECTOR);
                continue;
            }
            partitions.push(Partition {
                number: *number,
                start: at + start * SECTOR,
                length: count * SECTOR,
                kind: mbr_kind(kind),
            });
            *number += 1;
        }

        match next {
            Some(link) => at = link,
            None => return,
        }
    }
}

/// The handful of type bytes worth naming in a list somebody reads.
///
/// Not a lookup of all two hundred and fifty-six: the value of this is telling
/// a Linux partition from a Windows one at a glance, and an exhaustive table
/// would be a page of trivia to maintain for no further gain.
fn mbr_kind(kind: u8) -> &'static str {
    match kind {
        0x83 => "Linux",
        0x82 => "Linux swap",
        0x8E => "Linux LVM",
        0xFD => "Linux RAID",
        0x07 => "NTFS or exFAT",
        0x0B | 0x0C => "FAT32",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x01 => "FAT12",
        0xAF => "HFS+",
        0xA5 | 0xA6 | 0xA9 => "BSD",
        0xEF => "EFI system",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------- GPT

const GPT_SIGNATURE: &[u8] = b"EFI PART";

fn read_gpt(source: &mut impl ReadAt, sector_size: u64) -> Option<Table> {
    let mut header = vec![0u8; sector_size as usize];
    source.read_exact_at(sector_size, &mut header).ok()?;
    if !header.starts_with(GPT_SIGNATURE) {
        return None;
    }

    // The header says how much of itself is covered by its own checksum, and
    // the rest of the sector is not. Believing the field without bounding it
    // would index past the buffer on a disk that says 0xFFFFFFFF.
    let header_len = u32_at(&header, 12)? as usize;
    if header_len < 92 || header_len > header.len() {
        return None;
    }
    if !gpt_checksum_holds(&header[..header_len]) {
        return None;
    }

    let entries_lba = u64_at(&header, 72)?;
    let count = u32_at(&header, 80)?;
    let entry_size = u32_at(&header, 84)?;
    // An entry is 128 bytes in every GPT anybody has made; the specification
    // allows larger, always a multiple of 8. Below 128 there is nowhere for the
    // two addresses to live, and the bound above stops a flipped bit asking for
    // an allocation nothing can serve.
    if count == 0 || count > MAX_GPT_ENTRIES || !(128..=4096).contains(&entry_size) {
        return None;
    }

    let total = (count as u64).checked_mul(u64::from(entry_size))?;
    let at = entries_lba.checked_mul(sector_size)?;
    // The array has to be inside the disk. Without this a header claiming an
    // absurd address turns into a read that either fails slowly or, on a sparse
    // image, succeeds and returns zeros.
    if at.checked_add(total)? > source.size() {
        return None;
    }

    let mut entries = vec![0u8; total as usize];
    source.read_exact_at(at, &mut entries).ok()?;

    let mut partitions = Vec::new();
    for index in 0..count {
        let entry = &entries[(index as u64 * u64::from(entry_size)) as usize..][..128];
        // An all-zero type GUID is an unused slot, and slots are not
        // necessarily filled in order.
        if entry[..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first = u64_at(entry, 32)?;
        let last = u64_at(entry, 40)?;
        if last < first {
            continue;
        }
        // **Addresses here are in sectors, and a sector address is a `u64`.**
        // Multiplying one by 512 is where a flipped bit stops being a silly
        // number and becomes a panic — found by the sweep below, on the first
        // run of it. A partition that cannot be expressed in bytes is not a
        // partition anybody can read, so it is dropped rather than clamped:
        // clamping would invent an address, and every entry here is about to
        // be added to an offset and read at.
        let Some(start) = first.checked_mul(sector_size) else {
            continue;
        };
        // Inclusive of the last sector, which is the one place this scheme
        // differs from every other length on a disk.
        let Some(length) = (last - first + 1).checked_mul(sector_size) else {
            continue;
        };
        if start.checked_add(length).is_none() {
            continue;
        }
        partitions.push(Partition {
            // GPT numbers by slot, so an image whose second slot is empty has a
            // partition 1 and a partition 3 — which is what `sfdisk` prints and
            // what the kernel names, so it is what a person will be looking for.
            number: index + 1,
            start,
            length,
            kind: gpt_kind(&entry[..16]),
        });
    }

    (!partitions.is_empty()).then_some(Table {
        scheme: Scheme::Gpt,
        sector_size,
        partitions,
    })
}

/// Whether the header's own CRC-32 agrees with it.
///
/// **Not the same CRC as [`vhdx`](crate::vhdx) uses.** That one is Castagnoli;
/// this is the ordinary reflected 0xEDB88320 of zip and PNG, and using either
/// where the other belongs produces a checksum that never matches and a disk
/// that is silently declared unpartitioned.
fn gpt_checksum_holds(header: &[u8]) -> bool {
    let Some(stored) = u32_at(header, 16) else {
        return false;
    };
    let mut crc = Crc32::new();
    crc.update(&header[..16]);
    // The field is taken as zero while its own value is computed.
    crc.update(&[0, 0, 0, 0]);
    crc.update(&header[20..]);
    crc.finish() == stored
}

/// The type GUIDs worth naming, stored the way a GUID is written to disk: the
/// first three fields little-endian and the last eight bytes in order.
fn gpt_kind(guid: &[u8]) -> &'static str {
    const LINUX_DATA: [u8; 16] = [
        0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D,
        0xE4,
    ];
    const EFI_SYSTEM: [u8; 16] = [
        0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9,
        0x3B,
    ];
    const LINUX_SWAP: [u8; 16] = [
        0x6D, 0xFD, 0x57, 0x06, 0xAB, 0xA4, 0xC4, 0x43, 0x84, 0xE5, 0x09, 0x33, 0xC8, 0x4B, 0x4F,
        0x4F,
    ];
    const LINUX_LVM: [u8; 16] = [
        0x79, 0xD3, 0xD6, 0xE6, 0x07, 0xF5, 0xC2, 0x44, 0xA2, 0x3C, 0x23, 0x8F, 0x2A, 0x3D, 0xF9,
        0x28,
    ];
    const WINDOWS_DATA: [u8; 16] = [
        0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99,
        0xC7,
    ];
    const APPLE_HFS: [u8; 16] = [
        0x00, 0x53, 0x46, 0x48, 0x00, 0x00, 0xAA, 0x11, 0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC,
        0xAC,
    ];
    const APPLE_APFS: [u8; 16] = [
        0x7C, 0x34, 0x57, 0x7B, 0x00, 0x00, 0xAA, 0x11, 0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC,
        0xAC,
    ];

    match guid {
        g if g == LINUX_DATA => "Linux filesystem",
        g if g == EFI_SYSTEM => "EFI system",
        g if g == LINUX_SWAP => "Linux swap",
        g if g == LINUX_LVM => "Linux LVM",
        g if g == WINDOWS_DATA => "Windows basic data",
        g if g == APPLE_HFS => "HFS+",
        g if g == APPLE_APFS => "APFS",
        _ => "unknown",
    }
}

// ----------------------------------------------------------------- checksum

/// CRC-32 with the reflected 0xEDB88320 polynomial — zip's, PNG's and GPT's.
///
/// Bitwise rather than table-driven, for the same reason the VHDX one is: this
/// runs over ninety-two bytes once per image, and a kilobyte of lookup table
/// would be the only static data in the crate.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) fn new() -> Crc32 {
        Crc32(0xFFFF_FFFF)
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        const POLYNOMIAL: u32 = 0xEDB8_8320;
        for byte in bytes {
            self.0 ^= u32::from(*byte);
            for _ in 0..8 {
                let carry = self.0 & 1;
                self.0 >>= 1;
                if carry != 0 {
                    self.0 ^= POLYNOMIAL;
                }
            }
        }
    }

    pub(crate) fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

// ------------------------------------------------------------------ scalars

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

// Shared with `ext4::tests`, which wraps a real synthetic filesystem in a
// table built by the helpers here. Writing a GPT needs the same CRC that
// reading one checks, and a second copy of it would be a second chance to get
// the polynomial wrong.
#[cfg(test)]
pub(crate) mod tests;
