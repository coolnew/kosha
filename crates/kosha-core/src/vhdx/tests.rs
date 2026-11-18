// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! A VHDX built byte by byte, so the reader is tested against the format
//! rather than against itself.
//!
//! Everything here runs on every platform. The one thing that cannot be tested
//! without a real image is whether Windows will let the file be opened at all,
//! and that is a question about a handle rather than about a format.

use super::*;

const MIB: u64 = 1024 * 1024;

/// Where a builder puts each piece. The first megabyte is reserved by the
/// format; everything after it is ours to lay out.
const METADATA_AT: u64 = 0x100000;
const METADATA_BYTES: u64 = 64 * 1024;
const BAT_AT: u64 = 0x200000;
const BAT_BYTES: u64 = 64 * 1024;
const FIRST_BLOCK_AT: u64 = 0x300000;

struct Builder {
    block_size: u64,
    sector_size: u64,
    virtual_size: u64,
    /// One entry per virtual block, in order. `None` was never written.
    blocks: Vec<Option<Vec<u8>>>,
    has_parent: bool,
    dirty: bool,
    /// Corrupt the current header's checksum, so the other one has to be used.
    tear_header_2: bool,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            block_size: MIB,
            sector_size: 512,
            virtual_size: 64 * MIB,
            blocks: Vec::new(),
            has_parent: false,
            dirty: false,
            tear_header_2: false,
        }
    }

    fn block(mut self, bytes: &[u8]) -> Builder {
        let mut whole = vec![0u8; self.block_size as usize];
        whole[..bytes.len()].copy_from_slice(bytes);
        self.blocks.push(Some(whole));
        self
    }

    fn hole(mut self) -> Builder {
        self.blocks.push(None);
        self
    }

    fn build(self) -> Vec<u8> {
        let end = FIRST_BLOCK_AT + self.blocks.len() as u64 * self.block_size;
        let mut out = vec![0u8; end as usize];

        out[0..8].copy_from_slice(FILE_SIGNATURE);

        // Two headers, the second the current one. Written in turn on a real
        // image so a torn write can never lose both.
        put_header(&mut out, HEADER_1, 1, false, false);
        put_header(&mut out, HEADER_2, 2, self.dirty, self.tear_header_2);

        for at in [REGION_TABLE_1, REGION_TABLE_2] {
            put_region_table(&mut out, at);
        }
        self.put_metadata(&mut out);
        self.put_bat(&mut out);

        for (index, block) in self.blocks.iter().enumerate() {
            if let Some(bytes) = block {
                let at = (FIRST_BLOCK_AT + index as u64 * self.block_size) as usize;
                out[at..at + bytes.len()].copy_from_slice(bytes);
            }
        }
        out
    }

    fn put_metadata(&self, out: &mut [u8]) {
        let base = METADATA_AT as usize;
        out[base..base + 8].copy_from_slice(METADATA_SIGNATURE);
        put16(out, base + 0x0A, 3); // entry count

        // Values live after the table, addressed from the region's own start.
        let mut value_at = 0x200u32;
        let mut entry = 0usize;
        let mut push = |id: Guid, value: &[u8], out: &mut [u8]| {
            let at = base + 32 + entry * 32;
            out[at..at + 16].copy_from_slice(&id);
            put32(out, at + 16, value_at);
            put32(out, at + 20, value.len() as u32);
            let to = base + value_at as usize;
            out[to..to + value.len()].copy_from_slice(value);
            value_at += value.len() as u32;
            entry += 1;
        };

        let mut parameters = [0u8; 8];
        parameters[0..4].copy_from_slice(&(self.block_size as u32).to_le_bytes());
        parameters[4..8].copy_from_slice(&(u32::from(self.has_parent) << 1).to_le_bytes());
        push(FILE_PARAMETERS, &parameters, out);
        push(VIRTUAL_DISK_SIZE, &self.virtual_size.to_le_bytes(), out);
        push(
            LOGICAL_SECTOR_SIZE,
            &(self.sector_size as u32).to_le_bytes(),
            out,
        );
    }

    fn put_bat(&self, out: &mut [u8]) {
        let chunk_ratio = (1u64 << 23) * self.sector_size / self.block_size;
        for (index, payload) in self.blocks.iter().enumerate() {
            let block = index as u64;
            let bat_index = block + block / chunk_ratio;
            let at = (BAT_AT + bat_index * 8) as usize;
            let entry = match payload {
                Some(_) => {
                    let offset = FIRST_BLOCK_AT + block * self.block_size;
                    (offset & !0xF_FFFF) | PAYLOAD_BLOCK_FULLY_PRESENT
                }
                // State 0: never written. Reads as zeros.
                None => 0,
            };
            put64(out, at, entry);
        }
    }
}

fn put_header(out: &mut [u8], at: u64, sequence: u64, dirty: bool, tear: bool) {
    let base = at as usize;
    out[base..base + 4].copy_from_slice(HEADER_SIGNATURE);
    put64(out, base + 0x08, sequence);
    if dirty {
        // Any non-zero log GUID means there are entries a writer would replay.
        out[base + 0x30] = 0x01;
    }
    put16(out, base + 0x42, 1); // version
    let crc = crc_over(&out[base..base + HEADER_BYTES], 4);
    put32(out, base + 4, if tear { crc ^ 0xFFFF } else { crc });
}

fn put_region_table(out: &mut [u8], at: u64) {
    let base = at as usize;
    out[base..base + 4].copy_from_slice(REGION_SIGNATURE);
    put32(out, base + 8, 2); // entry count

    out[base + 16..base + 32].copy_from_slice(&BAT_REGION);
    put64(out, base + 32, BAT_AT);
    put32(out, base + 40, BAT_BYTES as u32);
    put32(out, base + 44, 1);

    out[base + 48..base + 64].copy_from_slice(&METADATA_REGION);
    put64(out, base + 64, METADATA_AT);
    put32(out, base + 72, METADATA_BYTES as u32);
    put32(out, base + 76, 1);

    let crc = crc_over(&out[base..base + REGION_TABLE_BYTES], 4);
    put32(out, base + 4, crc);
}

fn crc_over(bytes: &[u8], checksum_at: usize) -> u32 {
    let mut crc = Crc32c::new();
    crc.update(&bytes[..checksum_at]);
    crc.update(&[0, 0, 0, 0]);
    crc.update(&bytes[checksum_at + 4..]);
    crc.finish()
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(out: &mut [u8], at: usize, value: u64) {
    out[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

// ---------------------------------------------------------------- the tests

/// The known-answer test for the polynomial. CRC-32C is not the CRC-32 in zip
/// and PNG, and the two differ only in a constant — so the wrong one produces
/// numbers that look exactly as random as the right one.
#[test]
fn the_checksum_is_castagnoli_and_not_the_other_one() {
    let mut crc = Crc32c::new();
    crc.update(b"123456789");
    assert_eq!(crc.finish(), 0xE306_9283, "this is CRC-32, not CRC-32C");

    let mut empty = Crc32c::new();
    empty.update(b"");
    assert_eq!(empty.finish(), 0);
}

#[test]
fn a_block_is_found_where_the_table_says_it_is() {
    let image = Builder::new().block(b"first").block(b"second").build();
    let map = open(&mut image.as_slice()).unwrap();

    assert_eq!(map.block_size(), MIB);
    assert_eq!(map.virtual_size(), 64 * MIB);

    let (first, left) = map.locate(0);
    assert_eq!(first, Placement::At(FIRST_BLOCK_AT));
    assert_eq!(left, MIB, "a read at a block boundary has the whole block");

    let (second, _) = map.locate(MIB);
    assert_eq!(second, Placement::At(FIRST_BLOCK_AT + MIB));
}

/// The number that says how much of a read can be served before it has to be
/// split. Two adjacent virtual blocks are almost never adjacent in the file, so
/// a read that ignored this would silently splice two unrelated megabytes.
#[test]
fn a_read_is_told_how_far_the_block_goes() {
    let image = Builder::new().block(b"first").build();
    let map = open(&mut image.as_slice()).unwrap();

    let (placement, left) = map.locate(MIB - 100);
    assert_eq!(placement, Placement::At(FIRST_BLOCK_AT + MIB - 100));
    assert_eq!(left, 100);
}

/// Most of a sparse disk. A 256 TiB image in a 28 GB file is entirely ordinary,
/// and every byte nobody wrote reads as zero rather than as an error.
#[test]
fn an_unwritten_block_is_a_hole_rather_than_a_failure() {
    let image = Builder::new()
        .block(b"first")
        .hole()
        .block(b"third")
        .build();
    let map = open(&mut image.as_slice()).unwrap();

    assert_eq!(map.locate(0).0, Placement::At(FIRST_BLOCK_AT));
    assert_eq!(map.locate(MIB).0, Placement::Unwritten);
    assert_eq!(
        map.locate(2 * MIB).0,
        Placement::At(FIRST_BLOCK_AT + 2 * MIB)
    );
}

/// The table is sized for what has been allocated rather than for the address
/// space — a 256 TiB disk's would be two gigabytes, and the real one is nine
/// megabytes. Everything past the end is a hole, which is the ordinary state of
/// almost the whole disk and must not be an error.
#[test]
fn past_the_end_of_the_table_is_a_hole_too() {
    let image = Builder::new().block(b"first").build();
    let map = open(&mut image.as_slice()).unwrap();
    assert_eq!(map.locate(40 * MIB).0, Placement::Unwritten);
}

/// The interleave. The BAT carries one sector-bitmap entry after every
/// `chunk_ratio` payload entries, so a virtual block number is not a BAT index
/// — and a reader that assumed it was would be exactly right for the first
/// 4096 blocks and wrong for every block after that.
#[test]
fn the_sector_bitmap_entries_shift_every_later_block() {
    // Built directly: an image big enough to cross a chunk boundary would be
    // four gigabytes, and the arithmetic is the whole of what is being checked.
    let mut entries = vec![0u64; 4100];
    // Block 4096 is the first one that sits after a sector-bitmap entry, so it
    // lives at BAT index 4097.
    entries[4097] = (0x9000_0000 & !0xF_FFFF) | PAYLOAD_BLOCK_FULLY_PRESENT;
    // And what is *at* index 4096 is the bitmap, which is not a payload block.
    entries[4096] = 0x8000_0000 | PAYLOAD_BLOCK_FULLY_PRESENT;

    let map = BlockMap {
        entries,
        block_size: MIB,
        chunk_ratio: 4096,
        virtual_size: 8192 * MIB,
        dirty: false,
    };
    assert_eq!(map.locate(4096 * MIB).0, Placement::At(0x9000_0000));
}

#[test]
fn a_file_that_is_not_a_vhdx_is_refused() {
    let mut not_one = vec![0u8; 0x400000];
    not_one[0..8].copy_from_slice(b"notavhdx");
    assert!(open(&mut not_one.as_slice()).is_err());
    assert!(open(&mut [].as_slice()).is_err());
}

/// The image is being written by another process while we read it — that is the
/// normal case for a running distribution — so a half-written header has to be
/// recognised and the other one used.
#[test]
fn a_torn_header_falls_back_to_the_other_one() {
    let mut builder = Builder::new().block(b"first");
    builder.tear_header_2 = true;
    let image = builder.build();

    // Header 2 has the higher sequence number and a broken checksum, so header
    // 1 is what is left — and the image still opens.
    let map = open(&mut image.as_slice()).expect("a torn header is not a broken image");
    assert_eq!(map.locate(0).0, Placement::At(FIRST_BLOCK_AT));
}

/// Not an error, and not something to stay quiet about: the log holds writes
/// that a writer would replay, and we cannot. What we read is the disk as it
/// was a moment before.
#[test]
fn an_unclean_image_is_readable_and_says_so() {
    let mut builder = Builder::new().block(b"first");
    builder.dirty = true;
    let image = builder.build();

    let map = open(&mut image.as_slice()).unwrap();
    assert!(map.dirty);

    let clean = open(&mut Builder::new().block(b"first").build().as_slice()).unwrap();
    assert!(!clean.dirty);
}

/// A differencing disk reads through to a parent image, and its table carries
/// blocks that mean "some sectors here, the rest from the parent". Reading one
/// as though it were whole would return a silent mixture of two disks.
#[test]
fn a_differencing_disk_is_refused_rather_than_half_read() {
    let mut builder = Builder::new().block(b"first");
    builder.has_parent = true;
    let image = builder.build();
    assert!(open(&mut image.as_slice()).is_err());
}

/// A block size of zero divides by zero, and one that is not a power of two
/// makes every later offset wrong by a little. The format allows neither, so
/// the value is checked rather than trusted.
///
/// Patched into a finished image rather than built through the builder: the
/// builder lays blocks out *using* the block size, so a broken one would break
/// the fixture instead of the parser.
#[test]
fn an_impossible_block_size_is_refused() {
    let whole = Builder::new().block(b"first").build();
    // The first metadata value is the file parameters, and its first four bytes
    // are the block size. See `Builder::put_metadata`.
    let block_size_at = (METADATA_AT + 0x200) as usize;

    for size in [0u32, 512, 1000, 3 * 1024 * 1024, 1024 * 1024 + 1] {
        let mut image = whole.clone();
        put32(&mut image, block_size_at, size);
        assert!(
            open(&mut image.as_slice()).is_err(),
            "a block size of {size} was accepted"
        );
    }

    // And the value the real images use still opens.
    let mut image = whole.clone();
    put32(&mut image, block_size_at, MIB as u32);
    assert!(open(&mut image.as_slice()).is_ok());
}

/// A `&[u8]` is the whole reason the reader takes a trait: it is what lets a
/// synthetic image be walked on a machine with no virtual disk anywhere on it.
#[test]
fn reading_past_the_end_of_a_slice_is_an_error_rather_than_a_panic() {
    let bytes = vec![0u8; 16];
    let mut buffer = [0u8; 32];
    assert!(bytes.as_slice().read_exact_at(0, &mut buffer).is_err());
    assert!(
        bytes
            .as_slice()
            .read_exact_at(u64::MAX, &mut buffer)
            .is_err()
    );
    assert!(bytes.as_slice().read_exact_at(8, &mut buffer[..8]).is_ok());
}

// ------------------------------------------------- nothing may panic, ever

/// Every structure this reader trusts, with one bit wrong.
///
/// The tests above break the format in the ways somebody thought of. This
/// breaks it in the ways nobody did: a bit flipped in a signature, a version,
/// a count, an offset or a length, at every byte of every structure the reader
/// parses. Each one must come back as an image that opens or an image that is
/// refused — never as a panic.
///
/// **A panic here is not a tidy failure.** The window reads a distribution's
/// `ext4.vhdx` while the distribution is running, so these bytes are being
/// rewritten underneath the read: a torn structure is the ordinary case rather
/// than an attack, and an index out of bounds in it would take down a scan that
/// somebody started by clicking a row in the sidebar.
///
/// Mutated in place and restored rather than cloned — the image is four
/// megabytes and this makes fifteen thousand of them.
#[test]
fn no_single_flipped_bit_can_panic_the_reader() {
    let mut image = Builder::new()
        .block(b"first")
        .hole()
        .block(b"third")
        .build();

    // Where the reader actually reads: the signature, both headers, both
    // region tables, the metadata table and the block allocation table. The
    // megabytes of block data between them are never parsed, only returned.
    let regions: [(u64, usize); 7] = [
        (0, 64),
        (HEADER_1, 128),
        (HEADER_2, 128),
        (REGION_TABLE_1, 256),
        (REGION_TABLE_2, 256),
        (METADATA_AT, 512),
        (BAT_AT, 512),
    ];

    let mut tried = 0usize;
    for (start, length) in regions {
        for at in start as usize..(start as usize + length) {
            for bit in 0..8u8 {
                image[at] ^= 1 << bit;
                // The result is deliberately ignored: either answer is
                // correct, and the assertion is that we reach the next line.
                let _ = open(&mut image.as_slice());
                image[at] ^= 1 << bit;
                tried += 1;
            }
        }
    }
    // Computed rather than written down, so the number cannot drift away from
    // the regions above — and floored, so a sweep that quietly covered nothing
    // fails instead of passing.
    let expected: usize = regions.iter().map(|(_, length)| length * 8).sum();
    assert_eq!(tried, expected, "the sweep did not cover what it meant to");
    assert!(tried > 10_000, "only {tried} mutations");
}

/// The same question for a file that stops early, which is what a copy
/// interrupted halfway looks like — and what a sparse image looks like to a
/// reader that trusts a length it found inside the file.
#[test]
fn no_truncation_can_panic_the_reader() {
    let image = Builder::new().block(b"first").hole().build();

    // Every byte of the first structure, then a stride through the rest: the
    // interesting boundaries are dense at the start and sparse afterwards.
    let lengths = (0..1024)
        .chain((1024..image.len()).step_by(4096))
        .chain([image.len() - 1, image.len()]);

    for length in lengths {
        let _ = open(&mut &image[..length]);
    }
}
