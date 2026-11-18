// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Opening a UDF volume, and reading directories out of it.
//!
//! The layer between [`format`](super::format), which turns bytes into numbers,
//! and [`build`](super::build), which turns a walk into a tree. Three things
//! happen here and in no other UDF file.
//!
//! **Finding the volume.** Nothing says where a UDF volume's descriptors are;
//! an *anchor* at logical block 256 says, and finding it means guessing the
//! block size and being told whether the guess was right. A descriptor carries
//! its own address and its own CRC, so a wrong guess is refused rather than
//! believed — which is why this can search where the ISO 9660 reader is told.
//!
//! **Turning a logical block into an offset.** A file's blocks are numbered
//! inside a *partition*, the partition starts somewhere on the medium, and
//! between the two there may be an indirection: a table of replacements for
//! blocks that went bad, or — on a Blu-ray disc — a whole partition whose
//! contents are the contents of a file. [`Volume::place`] is where all of that
//! ends up, and it is the one function in this module that everything else
//! depends on being right.
//!
//! **Reading a file's extents.** A UDF file is a list of extents rather than a
//! run, so unlike an ISO 9660 disc a directory may be scattered, and a
//! directory with more extents than fit in its entry continues in an extent of
//! its own. Only directories are read: a file's length and its recorded blocks
//! are both in its entry, so nothing here ever reads a file's bytes.

use std::io;

use crate::vhdx::{Placement, ReadAt};

use super::format::{self, Allocation, Descriptors, File, Map, Recorded, Unreadable};

/// The most blocks of a volume descriptor sequence to walk.
///
/// A volume has six or seven descriptors and the sequence ends with a
/// terminator. mkudffs reserves sixteen blocks for it and Windows thirty-two;
/// this is a bound rather than an expectation, for a sequence whose terminator
/// was never written.
const MAX_DESCRIPTORS: usize = 64;

/// The most partition maps to keep.
///
/// A logical volume has one. Two is a sparable or metadata partition beside the
/// physical one it is built on. Everything past that is a volume doing something
/// no formatter does.
const MAX_MAPS: usize = 8;

/// The most extents to follow for one directory.
///
/// A directory is written in one extent by every formatter there is; a volume
/// that has been written to for years may have split one. The bound is what
/// stops a damaged chain of allocation extent descriptors from being walked for
/// ever.
const MAX_EXTENTS: usize = 4096;

/// How many allocation extent descriptors may follow one another.
const MAX_CONTINUATIONS: usize = 64;

/// How many indirect entries may be followed before giving up.
///
/// Strategy 4096 writes one. A chain of them is a damaged volume asking a reader
/// to walk in a circle.
const MAX_INDIRECT: usize = 4;

/// The most bytes of one directory to read.
///
/// A directory's length comes off the volume as a `u64`. Sixty-four megabytes is
/// several hundred thousand entries, which is past what any volume holds in one
/// directory — the same bound the ISO 9660 reader uses, for the same reason.
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

/// Where one logical partition's blocks really are.
enum Mapped {
    /// The identity: block *n* of this partition is block `start + n` of the
    /// medium. Every disc image, every hard disk and every stick.
    Physical { start: u32, blocks: u32 },
    /// The partition's contents are a *file's* contents, so a block in it is
    /// found by walking that file's extents. What a Blu-ray disc has, and what
    /// Windows writes when asked for UDF 2.50.
    Metadata { runs: Vec<Run>, blocks: u32 },
    /// Every block number goes through a table, which is how a disc that cannot
    /// be erased pretends to be one that can: rewriting a file appends the new
    /// bytes and writes a new table, and the old entry is simply no longer
    /// pointed at. A CD-R or DVD-R written a packet at a time.
    Virtual {
        /// One entry per virtual block, each a block of the physical partition
        /// below — or 0xFFFFFFFF for one that was never written.
        table: Vec<u8>,
        blocks: u32,
        start: u32,
        host_blocks: u32,
    },
}

/// One extent of something, resolved to the medium.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Run {
    /// Which logical block of the thing this run starts at.
    at: u32,
    /// Where those blocks are on the medium.
    start: u32,
    blocks: u32,
}

/// One extent of a file, before it has been resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Piece {
    partition: u16,
    block: u32,
    bytes: u32,
    recorded: bool,
}

/// An open UDF filesystem.
pub struct Filesystem<R> {
    source: R,
    volume: Volume,
    /// Which UDF revision the volume says it is, for the note. Reported rather
    /// than used: which of the two file entry layouts an entry has comes from
    /// that entry's own tag, not from here.
    revision: u16,
    /// Whether the volume names a partition whose bad blocks were moved, which
    /// is a fact about how much of what follows is exact.
    sparable: bool,
    metadata: bool,
    virtualised: bool,
}

/// Everything needed to turn a logical block into a byte offset.
///
/// Separate from the filesystem so that the arithmetic can be handed around and
/// tested without a source to read from — it is the part that is easy to get
/// wrong and impossible to notice, because a block computed in the wrong units
/// lands on a descriptor that simply fails to check out.
pub struct Volume {
    pub block_size: u32,
    /// How many blocks the medium has, which is what bounds every offset.
    pub blocks: u64,
    /// One entry per partition reference number, in the order the logical volume
    /// listed them: a reference number is an index into this list rather than a
    /// partition number.
    partitions: Vec<Mapped>,
    /// Where the tree starts.
    pub root: Allocation,
}

impl Volume {
    /// Where a logical block of a partition is, in bytes from the front of the
    /// volume.
    ///
    /// `None` for a block outside the partition, a partition reference number
    /// the volume never listed, or an offset past the end of the medium — all
    /// three of which are what a damaged allocation descriptor looks like.
    pub fn place(&self, partition: u16, block: u32) -> Option<u64> {
        let mapped = self.partitions.get(usize::from(partition))?;
        let on_medium = match mapped {
            Mapped::Physical { start, blocks } => {
                if block >= *blocks {
                    return None;
                }
                start.checked_add(block)?
            }
            Mapped::Metadata { runs, blocks } => {
                if block >= *blocks {
                    return None;
                }
                let run = runs
                    .iter()
                    .find(|run| block >= run.at && block - run.at < run.blocks)?;
                run.start.checked_add(block - run.at)?
            }
            Mapped::Virtual {
                table,
                blocks,
                start,
                host_blocks,
            } => {
                if block >= *blocks {
                    return None;
                }
                let physical = format::vat_entry(table, block as usize)?;
                if physical >= *host_blocks {
                    return None;
                }
                start.checked_add(physical)?
            }
        };
        if u64::from(on_medium) >= self.blocks {
            return None;
        }
        u64::from(on_medium).checked_mul(u64::from(self.block_size))
    }

    /// How many bytes the medium is, which is what refuses a file longer than
    /// the volume that holds it.
    pub fn bytes(&self) -> u64 {
        self.blocks.saturating_mul(u64::from(self.block_size))
    }
}

/// Which block size this volume has, or `None` if there is no UDF volume at
/// `start`.
///
/// **The recognition test, and the one the reader itself uses.** An anchor is
/// 512 bytes carrying tag 2, a checksum over its own sixteen, a CRC over the
/// other 496, and the number 256 — which it has to, because the check is that it
/// says where it is. Four block sizes are tried and at most one can answer, so
/// this is also how the block size is discovered.
///
/// `length` is how many bytes there are from `start`. The anchors at the end of
/// the medium are looked for too: UDF puts three on every volume it can, because
/// the media this was written for fail at their edges. For a partition whose
/// real length the caller does not know, a length that is too large simply means
/// those two are looked for where they are not — harmless, because the anchor at
/// block 256 is required on everything that is not write-once.
pub fn block_size_of<R: ReadAt>(source: &mut R, start: u64, length: u64) -> Option<u32> {
    let mut bytes = [0u8; format::ANCHOR_BYTES];
    for size in format::BLOCK_SIZES {
        let blocks = length / u64::from(size);
        // 256 first, then the last block, then 256 back from it — the order UDF
        // lists them in, which is also cheapest first.
        let last = blocks.checked_sub(1);
        let candidates = [
            Some(format::ANCHOR_BLOCK),
            last,
            last.and_then(|last| last.checked_sub(format::ANCHOR_BLOCK)),
        ];
        for block in candidates.into_iter().flatten() {
            let Ok(block32) = u32::try_from(block) else {
                continue;
            };
            let Some(at) = block
                .checked_mul(u64::from(size))
                .and_then(|at| at.checked_add(start))
            else {
                continue;
            };
            if at.saturating_add(format::ANCHOR_BYTES as u64) > start.saturating_add(length) {
                continue;
            }
            if source.read_exact_at(at, &mut bytes).is_ok()
                && format::anchor(&bytes, block32).is_some()
            {
                return Some(size);
            }
        }
    }
    None
}

impl<R: ReadAt> Filesystem<R> {
    /// Find the volume and everything needed to address it.
    pub fn open(mut source: R) -> io::Result<Filesystem<R>> {
        let length = source.size();
        let block_size =
            block_size_of(&mut source, 0, length).ok_or_else(|| refused(Unreadable::NotUdf))?;
        let blocks = length / u64::from(block_size);

        let anchor = read_anchor(&mut source, block_size, blocks)
            .ok_or_else(|| refused(Unreadable::NotUdf))?;

        // The main sequence, and the reserve where the main one holds no logical
        // volume. **Two copies of the descriptors is the whole reason the
        // reserve exists**, and a volume whose main sequence was half written is
        // exactly what it is for.
        let mut found = sequence(&mut source, block_size, blocks, anchor.main);
        if found.logical.is_none() && anchor.reserve.bytes > 0 {
            found = sequence(&mut source, block_size, blocks, anchor.reserve);
        }
        let logical = found
            .logical
            .ok_or_else(|| refused(Unreadable::Impossible))?;
        // The block size is stated twice — once by the anchor that was found at
        // a multiple of it, and once here. A volume where they disagree is one
        // where the anchor was found by luck.
        if logical.block_size != block_size {
            return Err(refused(Unreadable::Impossible));
        }

        let mut partitions: Vec<Mapped> = Vec::new();
        let mut sparable = false;
        let mut metadata = false;
        let mut virtualised = false;
        let mut unmapped = false;
        let mut pending: Vec<Map> = Vec::new();
        format::maps(
            &found.logical_bytes,
            logical.maps,
            logical.map_table_bytes,
            |map| {
                pending.push(map);
                pending.len() < MAX_MAPS
            },
        );

        for map in &pending {
            match *map {
                Map::Physical { partition } | Map::Sparable { partition } => {
                    if matches!(map, Map::Sparable { .. }) {
                        sparable = true;
                    }
                    match found.partition(partition) {
                        Some(found) => partitions.push(Mapped::Physical {
                            start: found.start,
                            blocks: found.blocks,
                        }),
                        None => unmapped = true,
                    }
                }
                Map::Metadata { partition, file } => {
                    metadata = true;
                    match found.partition(partition) {
                        Some(host) => {
                            match read_metadata(&mut source, block_size, blocks, host, file) {
                                Some(mapped) => partitions.push(mapped),
                                None => unmapped = true,
                            }
                        }
                        None => unmapped = true,
                    }
                }
                Map::Virtual { partition } => match found.partition(partition) {
                    Some(host) => {
                        virtualised = true;
                        match read_vat(&mut source, block_size, blocks, host) {
                            Some(mapped) => partitions.push(mapped),
                            // The table was not where the end of a recording
                            // puts it. Refused rather than guessed at: without
                            // it a block number on this volume means nothing.
                            None => unmapped = true,
                        }
                    }
                    None => unmapped = true,
                },
                Map::Unknown => unmapped = true,
            }
        }
        if partitions.is_empty() || unmapped {
            return Err(refused(Unreadable::Unmapped));
        }

        let volume = Volume {
            block_size,
            blocks,
            partitions,
            root: nowhere(),
        };
        let mut filesystem = Filesystem {
            source,
            volume,
            revision: logical.revision,
            sparable,
            metadata,
            virtualised,
        };

        // The file set is the last indirection: a `long_ad` in the logical volume
        // descriptor, pointing into a partition that has only just become
        // addressable.
        let mut bytes = vec![0u8; block_size as usize];
        let at = filesystem
            .volume
            .place(logical.file_set.partition, logical.file_set.block)
            .ok_or_else(|| refused(Unreadable::Impossible))?;
        filesystem.source.read_exact_at(at, &mut bytes)?;
        let root = format::file_set(&bytes).ok_or_else(|| refused(Unreadable::Impossible))?;
        filesystem.volume.root = root;
        Ok(filesystem)
    }

    pub fn volume(&self) -> &Volume {
        &self.volume
    }

    /// Where reading this entry will really land in the file, for sorting a
    /// directory's children into the order the medium likes.
    ///
    /// **A block number is not where a read goes.** Inside a virtual disk two
    /// neighbouring blocks are wherever they were first written, which on an
    /// image that grew over months is nowhere near each other — so sorting by
    /// block number puts the reads in an order that means nothing, and asking
    /// where each one *lands* is what turns a directory's worth of seeks into a
    /// sweep. It is the same question [`ReadAt::placement`] exists to answer for
    /// the ext4 reader, asked one entry at a time because this reader has one
    /// entry to read rather than a table to sweep.
    ///
    /// An entry that cannot be placed sorts last: it is about to fail, and
    /// failing after the reads that will succeed keeps them in order.
    pub fn lands_at(&self, icb: Allocation) -> u64 {
        match self
            .volume
            .place(icb.partition, icb.block)
            .map(|at| self.source.placement(at))
        {
            Some(Placement::At(offset)) => offset,
            // Never written, so there is nothing to seek to and nothing to
            // wait for — it reads as zeros wherever it is asked for.
            Some(Placement::Unwritten) => 0,
            None => u64::MAX,
        }
    }

    /// What to put on the tree about how the volume was addressed.
    ///
    /// **Which indirections were in the way is a judgement's worth of
    /// information**, the same as which ISO 9660 tree the names came from. A
    /// sparable partition is the one case where kosha's answer is exact only as
    /// long as no block has ever been replaced, and somebody reading a total off
    /// a rewritable disc deserves to know that was true of it.
    pub fn in_a_note(&self) -> String {
        let mut note = format!("revision {}", format::revision(self.revision));
        if self.metadata {
            note.push_str(", metadata partition");
        }
        if self.sparable {
            note.push_str(", sparable partition");
        }
        if self.virtualised {
            note.push_str(", virtual partition");
        }
        note
    }

    /// Read the file entry an ICB points at, into `bytes`.
    ///
    /// The caller keeps the bytes because a directory small enough to be stored
    /// inside its own entry is stored there, so the entry *is* the directory —
    /// and because a scan reads one of these per file and an allocation per file
    /// is a cost with nothing to show for it.
    ///
    /// Following an *indirect* entry is part of this: strategy 4096 writes one
    /// where the file entry would be and puts the entry somewhere else, so a
    /// reader that stopped at the first descriptor would see no file at all.
    ///
    /// The ICB that comes back is the one the entry was really found at, which
    /// is what its `short_ad`s are relative to.
    pub fn file(
        &mut self,
        icb: Allocation,
        bytes: &mut Vec<u8>,
    ) -> io::Result<Option<(File, Allocation)>> {
        let mut icb = icb;
        bytes.clear();
        bytes.resize(self.volume.block_size as usize, 0);
        for _ in 0..MAX_INDIRECT {
            let Some(at) = self.volume.place(icb.partition, icb.block) else {
                return Ok(None);
            };
            if self.source.read_exact_at(at, bytes).is_err() {
                return Ok(None);
            }
            if let Some(file) = format::file(bytes) {
                return Ok(Some((file, icb)));
            }
            match format::indirect(bytes) {
                Some(next) => icb = next,
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    /// Read every byte of a directory into `buffer`, and say where each block of
    /// it came from.
    ///
    /// Returns false where some of it could not be read, which is what makes the
    /// totals under that directory a floor rather than a number.
    fn read_directory(
        &mut self,
        file: &File,
        entry: &[u8],
        icb: Allocation,
        buffer: &mut Vec<u8>,
        runs: &mut Vec<(usize, Allocation)>,
    ) -> bool {
        buffer.clear();
        runs.clear();
        // The directory's own length, which is what says how much of the last
        // block is descriptors and how much is the padding after them.
        let wanted = file.length.min(MAX_DIRECTORY_BYTES) as usize;

        // **The common case is that there is nothing to read.** A directory
        // small enough to fit in the space its allocation descriptors would have
        // taken is stored there instead, which on a freshly written volume is
        // most of them.
        if file.descriptors == Descriptors::Embedded {
            let Some(inside) = entry.get(file.at..file.at + file.bytes) else {
                return false;
            };
            runs.push((0, icb));
            buffer.extend_from_slice(inside);
            buffer.truncate(wanted);
            return true;
        }

        let mut pieces: Vec<Piece> = Vec::new();
        let mut whole = self.extents(file, entry, icb, &mut pieces);
        if file.length > MAX_DIRECTORY_BYTES {
            whole = false;
        }

        let block_size = self.volume.block_size as usize;
        for piece in &pieces {
            // **What is left to fill, and nothing is read or zeroed past it.**
            // An extent's length is thirty bits off the volume, so one of them
            // can claim a gigabyte — and a directory is as long as its entry
            // says it is, not as long as its extents could hold. Checking only
            // *after* an extent would let one unrecorded extent allocate that
            // gigabyte first, out of a file entry of two kilobytes.
            let room = wanted.saturating_sub(buffer.len());
            if room == 0 {
                // Extents left over after the directory's own length has been
                // filled. Every entry has been read; what is left describes
                // nothing, and a directory whose two numbers disagree that far
                // is one to say something about rather than to trust.
                whole = false;
                break;
            }
            runs.push((
                buffer.len(),
                Allocation {
                    bytes: piece.bytes,
                    block: piece.block,
                    partition: piece.partition,
                    recorded: Recorded::Yes,
                },
            ));
            // An extent that is allocated and never written is a hole. A
            // directory has no business having one, and zeros read as padding
            // rather than as a descriptor, so this needs no special case beyond
            // not reading the medium for it.
            if !piece.recorded {
                buffer.resize(buffer.len() + (piece.bytes as usize).min(room), 0);
                continue;
            }
            // A block at a time, so that an extent whose last block is missing
            // — a copy that ran out of room, the ordinary way an image is
            // damaged — costs that block rather than the whole directory.
            let blocks = (piece.bytes as usize).min(room).div_ceil(block_size);
            for step in 0..blocks {
                let Ok(step) = u32::try_from(step) else {
                    whole = false;
                    break;
                };
                let Some(at) = piece
                    .block
                    .checked_add(step)
                    .and_then(|block| self.volume.place(piece.partition, block))
                else {
                    whole = false;
                    break;
                };
                let was = buffer.len();
                buffer.resize(was + block_size, 0);
                if self.source.read_exact_at(at, &mut buffer[was..]).is_err() {
                    buffer.truncate(was);
                    whole = false;
                    break;
                }
            }
        }
        // An extent is a whole number of blocks and a directory need not be, so
        // what was read runs past the end of it. The tail is padding on every
        // volume there is, and reading it as descriptors would be reading
        // whatever the formatter left there.
        if buffer.len() > wanted {
            buffer.truncate(wanted);
        }
        whole
    }

    /// Collect a file's extents, following the chain where there is one.
    ///
    /// The descriptors live in the file entry until there are too many for it,
    /// at which point the last of them says *the rest are over there* and points
    /// at an allocation extent descriptor holding more of the same.
    fn extents(
        &mut self,
        file: &File,
        entry: &[u8],
        icb: Allocation,
        out: &mut Vec<Piece>,
    ) -> bool {
        let step = match file.descriptors {
            Descriptors::Short => format::SHORT_AD,
            Descriptors::Long => format::LONG_AD,
            Descriptors::Extended => format::EXT_AD,
            Descriptors::Embedded => return true,
        };
        let mut area = entry.get(file.at..file.at + file.bytes).map(<[u8]>::to_vec);
        let mut hops = 0usize;
        let mut spare = vec![0u8; self.volume.block_size as usize];

        while let Some(bytes) = area.take() {
            let mut at = 0usize;
            while at + step <= bytes.len() {
                if out.len() >= MAX_EXTENTS {
                    return false;
                }
                let found = match file.descriptors {
                    Descriptors::Short => format::short(&bytes, at, icb.partition),
                    Descriptors::Long => format::long(&bytes, at),
                    Descriptors::Extended => format::extended(&bytes, at),
                    Descriptors::Embedded => None,
                };
                let Some(found) = found else {
                    return false;
                };
                at += step;
                // A length of nothing ends the list early, which is how a file
                // entry with room for more extents than it needs says so.
                if found.bytes == 0 {
                    return true;
                }
                match found.recorded {
                    Recorded::Continued => {
                        if hops >= MAX_CONTINUATIONS {
                            return false;
                        }
                        hops += 1;
                        let Some(next) = self.read_continuation(found, &mut spare) else {
                            return false;
                        };
                        area = Some(next);
                        break;
                    }
                    Recorded::Yes | Recorded::No => out.push(Piece {
                        partition: found.partition,
                        block: found.block,
                        bytes: found.bytes,
                        recorded: found.recorded == Recorded::Yes,
                    }),
                }
            }
            if area.is_none() {
                return true;
            }
        }
        true
    }

    /// Read an allocation extent descriptor and return the descriptors in it.
    fn read_continuation(&mut self, at: Allocation, spare: &mut [u8]) -> Option<Vec<u8>> {
        let offset = self.volume.place(at.partition, at.block)?;
        self.source.read_exact_at(offset, spare).ok()?;
        let (from, count) = format::allocation_extent(spare, at.block)?;
        Some(spare[from..from + count].to_vec())
    }
}

/// Read the anchor, trying every place UDF allows one.
fn read_anchor<R: ReadAt>(source: &mut R, block_size: u32, blocks: u64) -> Option<format::Anchor> {
    let mut bytes = [0u8; format::ANCHOR_BYTES];
    let last = blocks.checked_sub(1);
    let candidates = [
        Some(format::ANCHOR_BLOCK),
        last,
        last.and_then(|last| last.checked_sub(format::ANCHOR_BLOCK)),
    ];
    for block in candidates.into_iter().flatten() {
        let Ok(block32) = u32::try_from(block) else {
            continue;
        };
        let Some(at) = block.checked_mul(u64::from(block_size)) else {
            continue;
        };
        if source.read_exact_at(at, &mut bytes).is_ok()
            && let Some(anchor) = format::anchor(&bytes, block32)
        {
            return Some(anchor);
        }
    }
    None
}

/// What one volume descriptor sequence held.
struct Sequence {
    partitions: Vec<format::Partition>,
    logical: Option<format::LogicalVolume>,
    /// The logical volume descriptor's own bytes, because its partition maps are
    /// variable-length and are read from it a second time.
    logical_bytes: Vec<u8>,
}

impl Sequence {
    fn partition(&self, number: u16) -> Option<&format::Partition> {
        self.partitions.iter().find(|one| one.number == number)
    }
}

/// Walk a volume descriptor sequence, keeping the partitions and the logical
/// volume.
///
/// **A descriptor may be superseded by a later one for the same thing**, which
/// is how a volume is changed without rewriting the sequence: the one with the
/// higher sequence number wins. Rare on a disc and ordinary on a stick that has
/// been relabelled.
fn sequence<R: ReadAt>(
    source: &mut R,
    block_size: u32,
    blocks: u64,
    extent: format::Extent,
) -> Sequence {
    let mut found = Sequence {
        partitions: Vec::new(),
        logical: None,
        logical_bytes: Vec::new(),
    };
    let mut bytes = vec![0u8; block_size as usize];
    let count = (extent.bytes as usize)
        .div_ceil(block_size as usize)
        .min(MAX_DESCRIPTORS);

    for step in 0..count {
        let Some(block) = u64::from(extent.block).checked_add(step as u64) else {
            break;
        };
        if block >= blocks {
            break;
        }
        let Some(at) = block.checked_mul(u64::from(block_size)) else {
            break;
        };
        if source.read_exact_at(at, &mut bytes).is_err() {
            break;
        }
        // A block of the sequence that is not a descriptor ends it, whether or
        // not a terminator was written.
        let Some(tag) = format::tag(&bytes, None) else {
            break;
        };
        match tag.identifier {
            format::TAG_TERMINATING => break,
            format::TAG_PARTITION => {
                if let Some(partition) = format::partition(&bytes) {
                    match found
                        .partitions
                        .iter_mut()
                        .find(|one| one.number == partition.number)
                    {
                        Some(existing) if existing.sequence <= partition.sequence => {
                            *existing = partition;
                        }
                        Some(_) => {}
                        None => found.partitions.push(partition),
                    }
                }
            }
            format::TAG_LOGICAL_VOLUME => {
                if let Some(logical) = format::logical_volume(&bytes)
                    && found
                        .logical
                        .is_none_or(|existing| existing.sequence <= logical.sequence)
                {
                    found.logical = Some(logical);
                    found.logical_bytes = bytes.clone();
                }
            }
            _ => {}
        }
    }
    found
}

/// How far back from the end of an image to look for a virtual allocation
/// table.
///
/// **The table is at the end of what was *recorded*, not at the end of the
/// medium**, and a file has no way of saying where recording stopped. An image
/// of such a disc ends where the recording did, so the table is in its last
/// block; an image padded out to the size of the medium has zeros there and the
/// table somewhere above it, with nothing to say where. The Linux driver looks
/// three blocks back. This looks further, and then gives up rather than
/// searching a disc's worth of zeros one block at a time.
const MAX_VAT_SEARCH: u64 = 64;

/// The most of a virtual allocation table to read.
///
/// One entry per block of the partition, four bytes each, so this is a medium
/// of two million blocks — four gigabytes at 2048 bytes to a block, which is
/// past anything this arrangement was designed for.
const MAX_VAT_BYTES: u64 = 8 * 1024 * 1024;

/// Find the virtual allocation table and read it.
///
/// **What makes it findable is that it is a file.** It has a file entry like
/// any other, at a place fixed by where the recording stopped rather than by
/// anything written down — so this reads blocks backwards from the end until
/// one of them is a file entry that is a table. A descriptor that checks its
/// own CRC, carries its own address and then says what it is, which is enough
/// to be sure rather than hopeful.
fn read_vat<R: ReadAt>(
    source: &mut R,
    block_size: u32,
    blocks: u64,
    host: &format::Partition,
) -> Option<Mapped> {
    // A volume of one partition, so the table's own entry and its extents are
    // read with the same arithmetic as everything else.
    let inner = Volume {
        block_size,
        blocks,
        partitions: vec![Mapped::Physical {
            start: host.start,
            blocks: host.blocks,
        }],
        root: nowhere(),
    };

    let mut bytes = vec![0u8; block_size as usize];
    let last = blocks.checked_sub(1)?;
    let floor = last
        .saturating_sub(MAX_VAT_SEARCH)
        .max(u64::from(host.start));

    let mut at = last;
    while at >= floor {
        let offset = at.checked_mul(u64::from(block_size))?;
        if source.read_exact_at(offset, &mut bytes).is_ok()
            && let Ok(inside) = u32::try_from(at - u64::from(host.start))
            && let Some(table) = vat_at(source, &inner, &bytes, inside)
        {
            return Some(Mapped::Virtual {
                blocks: (table.len() / 4) as u32,
                table,
                start: host.start,
                host_blocks: host.blocks,
            });
        }
        at = at.checked_sub(1)?;
    }
    None
}

/// The entries of the table this block holds, if it holds one.
fn vat_at<R: ReadAt>(source: &mut R, inner: &Volume, bytes: &[u8], at: u32) -> Option<Vec<u8>> {
    // Only a file entry that really is at this block. Reading backwards over
    // whatever a disc ends with will eventually find bytes that parse; the
    // address in the tag is what says these are the ones being looked for.
    format::tag(bytes, Some(at))?;
    let entry = format::file(bytes)?;
    let data = read_bytes(source, inner, bytes, &entry, MAX_VAT_BYTES)?;
    let (from, count) = format::vat(&data, entry.kind)?;
    let to = from.checked_add(count.checked_mul(4)?)?;
    data.get(from..to).map(<[u8]>::to_vec)
}

/// Every byte of a small file, however its entry stores them.
///
/// For the two files a volume's own *structure* is made of — a virtual
/// allocation table and a metadata file — and for nothing in the tree: a scan
/// never reads a file's contents, and neither of these is a file anybody named.
fn read_bytes<R: ReadAt>(
    source: &mut R,
    volume: &Volume,
    entry_bytes: &[u8],
    entry: &format::File,
    most: u64,
) -> Option<Vec<u8>> {
    if entry.length > most {
        return None;
    }
    let wanted = entry.length as usize;
    if entry.descriptors == Descriptors::Embedded {
        return entry_bytes
            .get(entry.at..entry.at + entry.bytes)?
            .get(..wanted)
            .map(<[u8]>::to_vec);
    }

    let step = match entry.descriptors {
        Descriptors::Short => format::SHORT_AD,
        Descriptors::Long => format::LONG_AD,
        Descriptors::Extended => format::EXT_AD,
        Descriptors::Embedded => return None,
    };
    let area = entry_bytes.get(entry.at..entry.at + entry.bytes)?;
    let block_size = volume.block_size as usize;
    let mut out: Vec<u8> = Vec::with_capacity(wanted);
    let mut at = 0usize;
    while at + step <= area.len() && out.len() < wanted {
        let found = match entry.descriptors {
            Descriptors::Short => format::short(area, at, 0),
            Descriptors::Long => format::long(area, at),
            Descriptors::Extended => format::extended(area, at),
            Descriptors::Embedded => None,
        }?;
        at += step;
        if found.bytes == 0 {
            break;
        }
        // A chain of allocation extent descriptors is not followed. A table
        // long enough to need one describes a medium past what this reads, and
        // half a table is worse than none: it would answer for part of a volume
        // and lose the rest without saying so.
        if found.recorded == Recorded::Continued {
            return None;
        }
        // Only as far as the file says it is. An extent may be longer than the
        // file in it — the last one of every file usually is — and a damaged one
        // may be very much longer.
        let room = wanted.saturating_sub(out.len());
        for step in 0..found.bytes.min(room as u32).div_ceil(volume.block_size) {
            let offset = found
                .block
                .checked_add(step)
                .and_then(|block| volume.place(found.partition, block))?;
            let was = out.len();
            out.resize(was + block_size, 0);
            source.read_exact_at(offset, &mut out[was..]).ok()?;
        }
    }
    out.truncate(wanted);
    (out.len() == wanted).then_some(out)
}

/// A pointer at nothing, for a volume being built that has no root yet.
fn nowhere() -> Allocation {
    Allocation {
        bytes: 0,
        block: 0,
        partition: 0,
        recorded: Recorded::Yes,
    }
}

/// Resolve a metadata partition by reading the file its blocks are in.
///
/// **One more indirection, and the reason a Blu-ray disc reads at all.** UDF
/// 2.50 moved every directory and file entry into a single file so that they sit
/// together on a medium where a seek is expensive. That file lives in an
/// ordinary partition, and a block of the metadata partition is a block of it.
fn read_metadata<R: ReadAt>(
    source: &mut R,
    block_size: u32,
    blocks: u64,
    host: &format::Partition,
    file: u32,
) -> Option<Mapped> {
    // A volume of one partition, so that the metadata file's own entry can be
    // read with the same arithmetic as everything else.
    let inner = Volume {
        block_size,
        blocks,
        partitions: vec![Mapped::Physical {
            start: host.start,
            blocks: host.blocks,
        }],
        root: nowhere(),
    };

    let mut bytes = vec![0u8; block_size as usize];
    let at = inner.place(0, file)?;
    source.read_exact_at(at, &mut bytes).ok()?;
    let entry = format::file(&bytes)?;
    // The metadata file is a file whose bytes are blocks. Embedded descriptors
    // would mean a partition of at most a block, which is not a volume.
    let step = match entry.descriptors {
        Descriptors::Short => format::SHORT_AD,
        Descriptors::Long => format::LONG_AD,
        Descriptors::Extended => format::EXT_AD,
        Descriptors::Embedded => return None,
    };

    let area = bytes.get(entry.at..entry.at + entry.bytes)?.to_vec();
    let mut runs: Vec<Run> = Vec::new();
    let mut logical = 0u32;
    let mut at = 0usize;
    while at + step <= area.len() && runs.len() < MAX_EXTENTS {
        let found = match entry.descriptors {
            Descriptors::Short => format::short(&area, at, 0),
            Descriptors::Long => format::long(&area, at),
            Descriptors::Extended => format::extended(&area, at),
            Descriptors::Embedded => None,
        }?;
        at += step;
        if found.bytes == 0 {
            break;
        }
        // A chain of allocation extent descriptors is not followed here. A
        // metadata file long enough to need one is a volume past what this
        // reads, and stopping with part of a partition mapped would answer
        // questions about half a tree without saying so.
        if found.recorded == Recorded::Continued {
            return None;
        }
        let count = found.bytes.div_ceil(block_size);
        let start = host.start.checked_add(found.block)?;
        runs.push(Run {
            at: logical,
            start,
            blocks: count,
        });
        logical = logical.checked_add(count)?;
    }
    if runs.is_empty() {
        return None;
    }
    Some(Mapped::Metadata {
        runs,
        blocks: logical,
    })
}

fn refused(reason: Unreadable) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// What one entry of a directory says.
///
/// **Less than an ISO 9660 record says, and that is the format's one real
/// cost.** A directory record on a disc carries the file's length; a file
/// identifier descriptor carries only where its entry is, so a scan of a UDF
/// volume reads one block per file. [`build`](super::build) is where that is
/// paid for in the order the medium prefers.
pub struct Entry<'a> {
    /// UTF-8, out of OSTA Compressed Unicode.
    pub name: &'a [u8],
    /// Where the file's entry is.
    pub icb: Allocation,
    /// From the identifier rather than from the file entry, so that a walk knows
    /// what it is about to read before reading it.
    pub is_directory: bool,
}

/// Every entry of one directory.
///
/// `each` is handed a name and where to find what it names; nothing is kept, and
/// `name` is rewritten for every entry.
pub fn entries<R: ReadAt>(
    fs: &mut Filesystem<R>,
    file: &File,
    entry: &[u8],
    icb: Allocation,
    buffer: &mut Vec<u8>,
    runs: &mut Vec<(usize, Allocation)>,
    name: &mut Vec<u8>,
    mut each: impl FnMut(Entry<'_>),
) -> bool {
    let mut whole = fs.read_directory(file, entry, icb, buffer, runs);
    let block_size = fs.volume.block_size as usize;
    if block_size == 0 {
        return false;
    }

    let mut at = 0usize;
    while at < buffer.len() {
        // Which logical block this descriptor begins in, which its own tag
        // repeats. **The check that a walk is still aligned**: a directory is a
        // run of descriptors and their lengths are the only thing saying where
        // one ends, so a single misread length would turn the rest of the
        // directory into bytes that happen to parse. There is no other way to
        // notice.
        let expected = block_of(runs, at, block_size);
        let Some(found) = format::identifier(&buffer[at..], expected) else {
            // Nothing parses here, so this is the end of the directory.
            //
            // **And it is the end, rather than a gap to step over.** An ISO 9660
            // directory is padded at every block boundary, because the standard
            // forbids a record from straddling one; a UDF directory is one byte
            // stream and a descriptor may begin in one block and end in the next,
            // which is why the Linux driver carries code to reassemble one. So
            // there is no padding to skip, and treating a run of zeros as padding
            // would be a way of reading past damage without saying so.
            //
            // Everything before this point is still true. A tail of zeros is the
            // ordinary end of a directory whose last block is not full; anything
            // else is a length that ran off the end, and the caller is told which.
            whole = whole && buffer[at..].iter().all(|byte| *byte == 0);
            break;
        };
        let length = found.length;

        // The parent entry is every directory's first and has no name. Following
        // it is a walk back up the tree.
        if found.is_parent() || found.is_deleted() {
            at += length;
            continue;
        }

        name.clear();
        if !format::push_name(found.name, name) || name.is_empty() {
            // A name in an encoding UDF does not define, or none at all. The
            // entry is real and cannot be shown, which is a thing to count
            // rather than to drop silently.
            whole = false;
            at += length;
            continue;
        }

        each(Entry {
            name,
            icb: found.icb,
            is_directory: found.is_directory(),
        });
        at += length;
    }
    whole
}

/// Which logical block the byte at `at` in the buffer came from.
///
/// The runs are in order and there are a handful of them, so this is a walk
/// rather than a search.
fn block_of(runs: &[(usize, Allocation)], at: usize, block_size: usize) -> Option<u32> {
    let (from, run) = runs.iter().rev().find(|(from, _)| *from <= at)?;
    let step = u32::try_from((at - from) / block_size).ok()?;
    run.block.checked_add(step)
}
