// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! VHDX, the container a WSL distribution's filesystem lives in.
//!
//! A WSL2 distribution is one file on a Windows drive — `ext4.vhdx` — holding a
//! virtual disk that Windows can see nothing inside. Every Windows tool,
//! kosha's own volume scan included, reports one enormous opaque file. Getting
//! at what is in it means two formats: this one, which says where a virtual
//! byte really lives, and [`ext4`](crate::ext4), which is the filesystem on top.
//!
//! **Parsed rather than mounted, which is the whole decision.** Windows will
//! attach a VHDX and hand back a raw device, but `AttachVirtualDisk` needs
//! administrator, changes the state of the machine, and fails outright on the
//! image of a *running* distribution. Reading the file is none of those things:
//! it needs no privilege at all, mutates nothing, and works on a distribution
//! that is running right now — measured, all three images on this machine open
//! for reading while two of them are booted.
//!
//! ## What the format is
//!
//! A dynamic VHDX is a sparse map. The virtual disk is cut into fixed-size
//! blocks — 1 MiB on every WSL image measured — and a *block allocation table*
//! says, for each one, either where in the file it lives or that it was never
//! written and reads as zeros. A 256 TiB virtual disk in a 28 GB file is
//! entirely ordinary.
//!
//! ```text
//! 0x00000  file type identifier   "vhdxfile"
//! 0x10000  header 1               "head", a sequence number, a log GUID
//! 0x20000  header 2               the same, and the higher sequence wins
//! 0x30000  region table 1         "regi", where the BAT and metadata are
//! 0x40000  region table 2         a copy of it
//! 0x100000 onwards                the regions themselves, and the blocks
//! ```
//!
//! Everything here is bytes in and numbers out, with no Windows call anywhere
//! in the file, so the format is under test on whichever machine it is being
//! edited on — the same arrangement as the `ntfs` modules.

use std::io;

/// Where the two headers and the two region tables sit. Fixed by the format.
const HEADER_1: u64 = 0x10000;
const HEADER_2: u64 = 0x20000;
const REGION_TABLE_1: u64 = 0x30000;
const REGION_TABLE_2: u64 = 0x40000;

/// A header is checksummed over 4 KiB; a region table over its whole 64 KiB.
const HEADER_BYTES: usize = 4096;
const REGION_TABLE_BYTES: usize = 64 * 1024;

/// The three signatures worth checking.
const FILE_SIGNATURE: &[u8] = b"vhdxfile";
const HEADER_SIGNATURE: &[u8] = b"head";
const REGION_SIGNATURE: &[u8] = b"regi";
const METADATA_SIGNATURE: &[u8] = b"metadata";

/// Region and metadata items are addressed by GUID, stored the way Windows
/// writes one: the first three fields little-endian, the last eight bytes in
/// order. Kept as the raw sixteen bytes so there is nothing to get wrong twice.
type Guid = [u8; 16];

/// `2DC27766-F623-4200-9D64-115E9BFD4A08` — the block allocation table.
const BAT_REGION: Guid = [
    0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42, 0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08,
];
/// `8B7CA206-4790-4B9A-B8FE-575F050F886E` — the metadata region.
const METADATA_REGION: Guid = [
    0x06, 0xA2, 0x7C, 0x8B, 0x90, 0x47, 0x9A, 0x4B, 0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E,
];
/// `CAA16737-FA36-4D43-B3B6-33F0AA44E76B` — block size, and whether there is a
/// parent image.
const FILE_PARAMETERS: Guid = [
    0x37, 0x67, 0xA1, 0xCA, 0x36, 0xFA, 0x43, 0x4D, 0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B,
];
/// `2FA54224-CD1B-4876-B211-5DBED83BF4B8` — how big the disk claims to be.
const VIRTUAL_DISK_SIZE: Guid = [
    0x24, 0x42, 0xA5, 0x2F, 0x1B, 0xCD, 0x76, 0x48, 0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8,
];
/// `8141BF1D-A96F-4709-BA47-F233A8FAAB5F` — the sector size the BAT's chunk
/// ratio is defined in terms of.
const LOGICAL_SECTOR_SIZE: Guid = [
    0x1D, 0xBF, 0x41, 0x81, 0x6F, 0xA9, 0x09, 0x47, 0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F,
];

/// A payload block that is really in the file. Every other state reads as
/// zeros, for our purposes: never written, discarded, or explicitly zero.
const PAYLOAD_BLOCK_FULLY_PRESENT: u64 = 6;

/// Anything that reads or writes at an offset.
///
/// A trait rather than a `File` so that the whole of this module and all of
/// [`ext4`](crate::ext4) can be driven from a `Vec<u8>` in a test — which is
/// how a synthetic filesystem gets built and walked on a machine that has
/// neither WSL nor a virtual disk.
pub trait ReadAt {
    /// Fill `buffer` from `offset`, or fail. Short reads are an error, not a
    /// partial success: every caller here is reading a structure.
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;

    /// How much there is to read.
    ///
    /// Needed because the reader above this one works in fixed-size chunks, and
    /// the last chunk of anything is short. Without it every read near the end
    /// would fail rather than returning what is actually there.
    fn size(&self) -> u64;

    /// Where a read at `offset` will really land.
    ///
    /// **The difference between ten seconds and two.** A virtual disk is a map,
    /// and two blocks next to each other in the virtual disk are put wherever
    /// in the file they were first written — which is nowhere near each other
    /// on an image that grew over months. A caller that has thousands of blocks
    /// to read can ask where each one is and then ask for them in *that* order,
    /// turning fifteen hundred full-stroke seeks across a 28 GB file into a
    /// sweep across it.
    ///
    /// The default is the identity, which is right for anything that is not a
    /// map: a file, or a slice.
    fn placement(&self, offset: u64) -> Placement {
        Placement::At(offset)
    }

    /// Read from a place [`placement`](Self::placement) gave, without
    /// translating it again.
    fn read_placed(&mut self, at: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.read_exact_at(at, buffer)
    }
}

impl ReadAt for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let at = usize::try_from(offset).map_err(|_| short())?;
        let end = at.checked_add(buffer.len()).ok_or_else(short)?;
        let from = self.get(at..end).ok_or_else(short)?;
        buffer.copy_from_slice(from);
        Ok(())
    }
}

fn short() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "read past the end of the image",
    )
}

fn malformed(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

/// What a virtual byte maps to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// It is in the file, here.
    At(u64),
    /// It was never written, and reads as zeros. Most of a sparse disk.
    Unwritten,
}

/// Where every block of a virtual disk lives.
///
/// Built once and then consulted per read. The table is a few megabytes for a
/// 256 TiB disk, because it is sized for what has been allocated rather than
/// for the address space.
pub struct BlockMap {
    /// The raw BAT, still as little-endian `u64` entries.
    entries: Vec<u64>,
    /// Bytes of virtual disk one entry covers.
    block_size: u64,
    /// How many payload entries sit between two sector-bitmap entries. The BAT
    /// interleaves them, and a virtual block number is not a BAT index.
    chunk_ratio: u64,
    /// What the disk says its size is.
    virtual_size: u64,
    /// The image was not closed cleanly and has log entries that a writer would
    /// replay. We cannot replay them and do not try; the caller decides whether
    /// to say so.
    pub dirty: bool,
}

impl BlockMap {
    /// How big the virtual disk claims to be.
    pub fn virtual_size(&self) -> u64 {
        self.virtual_size
    }

    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    /// Where virtual byte `offset` lives, and how many bytes of the block
    /// follow it.
    ///
    /// The second half matters: a read that would cross a block boundary has to
    /// be split, because two adjacent virtual blocks are almost never adjacent
    /// in the file.
    pub fn locate(&self, offset: u64) -> (Placement, u64) {
        let block = offset / self.block_size;
        let within = offset % self.block_size;
        let left = self.block_size - within;

        // The BAT interleaves one sector-bitmap entry after every
        // `chunk_ratio` payload entries, so the index runs ahead of the block
        // number. Getting this wrong reads plausible data from the wrong place.
        let index = block + block / self.chunk_ratio;
        let Some(&entry) = self.entries.get(index as usize) else {
            // Past the end of the table. The table is sized for what has been
            // allocated, not for the whole address space, so this is the
            // ordinary state of most of a sparse disk rather than an error.
            return (Placement::Unwritten, left);
        };

        if entry & 0x7 != PAYLOAD_BLOCK_FULLY_PRESENT {
            return (Placement::Unwritten, left);
        }
        // The file offset is in the top 44 bits, counted in megabytes.
        let at = (entry >> 20) << 20;
        (Placement::At(at + within), left)
    }
}

/// Whether these bytes begin a VHDX file.
///
/// Exposed because "is this a disk image?" is also asked from outside a scan —
/// after one has failed, where the answer decides what somebody is told to do
/// next. The *extension* is not the question: a name is not evidence, and a
/// scan that has already refused the file is no place to start guessing.
pub fn has_signature(start: &[u8]) -> bool {
    start.starts_with(FILE_SIGNATURE)
}

/// Read a virtual disk's structure and build its block map.
pub fn open(source: &mut impl ReadAt) -> io::Result<BlockMap> {
    let mut signature = [0u8; 8];
    source.read_exact_at(0, &mut signature)?;
    if !has_signature(&signature) {
        return Err(malformed("this file does not begin with a VHDX signature"));
    }

    let header = read_header(source)?;
    let regions = read_region_table(source)?;

    let (metadata_at, metadata_len) = regions
        .iter()
        .find(|region| region.id == METADATA_REGION)
        .map(|region| (region.offset, region.length))
        .ok_or_else(|| malformed("the image has no metadata region"))?;
    let metadata = read_metadata(source, metadata_at, metadata_len)?;

    if metadata.has_parent {
        // A differencing disk reads through to a parent image, and its BAT
        // carries partially-present blocks that mean "some sectors here, the
        // rest from the parent". WSL does not make them, and pretending to read
        // one would silently return a mixture of two disks.
        return Err(malformed(
            "this image is a differencing disk, which kosha does not follow",
        ));
    }

    let bat = regions
        .iter()
        .find(|region| region.id == BAT_REGION)
        .ok_or_else(|| malformed("the image has no block allocation table"))?;

    // `chunk_ratio` is fixed by the format: 2^23 sectors' worth of virtual
    // disk per sector-bitmap block.
    let chunk_ratio = (1u64 << 23)
        .checked_mul(metadata.logical_sector_size)
        .and_then(|bytes| bytes.checked_div(metadata.block_size))
        .filter(|ratio| *ratio > 0)
        .ok_or_else(|| malformed("the image's block and sector sizes do not fit together"))?;

    // Capped at what the region actually holds, which is the honest bound: a
    // 256 TiB disk's table would be two gigabytes, and the image only ever
    // stores as much of it as has been allocated.
    let count = usize::try_from(bat.length / 8).map_err(|_| malformed("the table is too large"))?;
    let mut raw = vec![0u8; count * 8];
    source.read_exact_at(bat.offset, &mut raw)?;
    let entries = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|eight| u64::from_le_bytes(*eight))
        .collect();

    Ok(BlockMap {
        entries,
        block_size: metadata.block_size,
        chunk_ratio,
        virtual_size: metadata.virtual_size,
        dirty: header.dirty,
    })
}

// ------------------------------------------------------------------- header

struct Header {
    sequence: u64,
    dirty: bool,
}

/// The current header, which is whichever of the two survived its checksum with
/// the higher sequence number.
///
/// Both are written in turn so that a torn write can never lose both, and the
/// checksum is how a half-written one is recognised. That matters here more
/// than it would elsewhere: kosha reads images of *running* distributions, so
/// the file really can be being written while it is being read.
fn read_header(source: &mut impl ReadAt) -> io::Result<Header> {
    let mut best: Option<Header> = None;
    for at in [HEADER_1, HEADER_2] {
        let mut bytes = vec![0u8; HEADER_BYTES];
        if source.read_exact_at(at, &mut bytes).is_err() {
            continue;
        }
        let Some(header) = parse_header(&bytes) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|kept| header.sequence > kept.sequence)
        {
            best = Some(header);
        }
    }
    best.ok_or_else(|| malformed("neither VHDX header is readable"))
}

fn parse_header(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < HEADER_BYTES || &bytes[0..4] != HEADER_SIGNATURE {
        return None;
    }
    if !checksum_holds(bytes, 4) {
        return None;
    }
    Some(Header {
        sequence: u64_at(bytes, 0x08)?,
        // A non-zero log GUID means there are journal entries a writer would
        // replay before using the image. We cannot replay them, so what we read
        // is the disk as it was before the last few writes.
        dirty: bytes.get(0x30..0x40)?.iter().any(|byte| *byte != 0),
    })
}

// ------------------------------------------------------------- region table

struct Region {
    id: Guid,
    offset: u64,
    length: u64,
}

fn read_region_table(source: &mut impl ReadAt) -> io::Result<Vec<Region>> {
    for at in [REGION_TABLE_1, REGION_TABLE_2] {
        let mut bytes = vec![0u8; REGION_TABLE_BYTES];
        if source.read_exact_at(at, &mut bytes).is_err() {
            continue;
        }
        if let Some(regions) = parse_region_table(&bytes) {
            return Ok(regions);
        }
    }
    Err(malformed("neither VHDX region table is readable"))
}

fn parse_region_table(bytes: &[u8]) -> Option<Vec<Region>> {
    if bytes.len() < REGION_TABLE_BYTES || &bytes[0..4] != REGION_SIGNATURE {
        return None;
    }
    if !checksum_holds(bytes, 4) {
        return None;
    }
    let count = u32_at(bytes, 8)?;
    // The format caps it at 2047, and the table is one 64 KiB block.
    if count > 2047 {
        return None;
    }

    let mut regions = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let at = 16 + index * 32;
        let id: Guid = bytes.get(at..at + 16)?.try_into().ok()?;
        regions.push(Region {
            id,
            offset: u64_at(bytes, at + 16)?,
            length: u64::from(u32_at(bytes, at + 24)?),
        });
    }
    Some(regions)
}

// ----------------------------------------------------------------- metadata

struct Metadata {
    block_size: u64,
    virtual_size: u64,
    logical_sector_size: u64,
    has_parent: bool,
}

fn read_metadata(source: &mut impl ReadAt, at: u64, length: u64) -> io::Result<Metadata> {
    // A megabyte on every image measured, and the cap is here so that a corrupt
    // region length cannot ask for the whole address space.
    let length = usize::try_from(length.min(16 * 1024 * 1024)).map_err(|_| short())?;
    let mut bytes = vec![0u8; length];
    source.read_exact_at(at, &mut bytes)?;
    parse_metadata(&bytes).ok_or_else(|| malformed("the image's metadata region is malformed"))
}

fn parse_metadata(bytes: &[u8]) -> Option<Metadata> {
    if bytes.len() < 32 || &bytes[0..8] != METADATA_SIGNATURE {
        return None;
    }
    let count = u16_at(bytes, 0x0A)?;

    let mut block_size = None;
    let mut virtual_size = None;
    let mut logical_sector_size = None;
    let mut has_parent = false;

    for index in 0..count as usize {
        let at = 32 + index * 32;
        let id: Guid = bytes.get(at..at + 16)?.try_into().ok()?;
        // Offsets in a metadata entry are from the start of the region, which
        // is why the whole region is read rather than just the table.
        let value_at = u32_at(bytes, at + 16)? as usize;
        let value_len = u32_at(bytes, at + 20)? as usize;
        let value = bytes.get(value_at..value_at.checked_add(value_len)?)?;

        match id {
            FILE_PARAMETERS if value.len() >= 8 => {
                block_size = Some(u64::from(u32_at(value, 0)?));
                // Bit 1 of the flags: this image is a differencing disk.
                has_parent = u32_at(value, 4)? & 0x2 != 0;
            }
            VIRTUAL_DISK_SIZE if value.len() >= 8 => virtual_size = u64_at(value, 0),
            LOGICAL_SECTOR_SIZE if value.len() >= 4 => {
                logical_sector_size = Some(u64::from(u32_at(value, 0)?));
            }
            _ => {}
        }
    }

    let block_size = block_size.filter(|size| size.is_power_of_two() && *size >= 1024 * 1024)?;
    Some(Metadata {
        block_size,
        virtual_size: virtual_size?,
        logical_sector_size: logical_sector_size.filter(|size| *size == 512 || *size == 4096)?,
        has_parent,
    })
}

// ---------------------------------------------------------------- checksums

/// Whether a structure's own CRC-32C agrees with its contents.
///
/// Worth doing rather than skipping, and for the same reason the NTFS reader
/// verifies its update sequence array: this is how a half-written copy is told
/// from a whole one, and kosha reads images that another process is writing to
/// at the time.
fn checksum_holds(bytes: &[u8], checksum_at: usize) -> bool {
    let Some(stored) = u32_at(bytes, checksum_at) else {
        return false;
    };
    let mut crc = Crc32c::new();
    crc.update(&bytes[..checksum_at]);
    // The field takes the value zero during the computation.
    crc.update(&[0, 0, 0, 0]);
    crc.update(&bytes[checksum_at + 4..]);
    crc.finish() == stored
}

/// CRC-32C — the Castagnoli polynomial, which is what VHDX uses and is not the
/// same one as zip or PNG.
///
/// Bitwise rather than table-driven: this runs over 132 KiB once per image, and
/// a 1 KiB lookup table to save a few microseconds would be the only static
/// data in the crate.
struct Crc32c(u32);

impl Crc32c {
    fn new() -> Crc32c {
        Crc32c(0xFFFF_FFFF)
    }

    fn update(&mut self, bytes: &[u8]) {
        /// The reflected form of 0x1EDC6F41.
        const POLYNOMIAL: u32 = 0x82F6_3B78;
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

    fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

// ------------------------------------------------------------------ scalars

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests;
