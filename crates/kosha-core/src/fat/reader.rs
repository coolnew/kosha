// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Following chains and reading directories, in both formats.
//!
//! The layer between [`format`](super::format), which turns bytes into
//! numbers, and [`build`](super::build), which turns a walk into a tree. What
//! it adds is the reads: which clusters a directory is in, and what its
//! records say.
//!
//! **Only directories are chased through the table.** A file's length is in
//! the record that names it and neither format has sparse files, so how much
//! disk a file occupies is arithmetic — see [`Geometry::allocation`]. On a
//! volume of a hundred thousand files that is a hundred thousand reads of the
//! one structure that would otherwise be read for everything, and the table of
//! a large FAT32 volume is megabytes.
//!
//! The two formats meet at [`Entry`]: a name, a length, a first cluster, and
//! whether it is a directory. Above this line nothing knows which of the two
//! it is walking.

use std::io;

use crate::vhdx::ReadAt;

use super::format::{self, Geometry, Place, Record, Slot};

/// The most bytes of directory records this reader will read as one directory.
///
/// A chain's length comes off the disk, so without a bound a loop in a damaged
/// table is a walk with no end. Sixty-four megabytes is two million records,
/// which is far past any directory a filesystem holds — and a chain cannot in
/// any case be longer than the volume has clusters, which is the other half of
/// the bound below.
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

/// What a chain's next link says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Next {
    Cluster(u32),
    /// The end-of-chain marker, or a bad-cluster mark. Both stop a chain and
    /// neither is damage.
    End,
    /// A link pointing at a cluster the volume does not have, or a table too
    /// short to hold the entry. The chain stops, and what it was reading is
    /// incomplete rather than finished.
    Broken,
}

/// An open FAT or exFAT filesystem.
pub struct Filesystem<R> {
    source: R,
    geometry: Geometry,
    /// One window of the table, kept because following a chain reads entries
    /// that are next to each other far more often than not.
    ///
    /// A window rather than a parsed entry, because FAT12 keeps its entries a
    /// byte and a half apart: one of them straddles every second byte and the
    /// last of a sector can straddle the sector.
    window: Vec<u8>,
    /// Where `window` was read from, or `None` before anything was.
    window_at: Option<u64>,
}

impl<R: ReadAt> Filesystem<R> {
    /// Read the boot sector and take the geometry from it.
    pub fn open(mut source: R) -> io::Result<Filesystem<R>> {
        let mut sector = [0u8; 512];
        source.read_exact_at(0, &mut sector)?;
        let geometry = format::boot(&sector)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))?;
        Ok(Filesystem {
            window: vec![0u8; geometry.sector_size as usize],
            window_at: None,
            source,
            geometry,
        })
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// One byte of the table, through the window.
    ///
    /// Reading a byte at a time looks wasteful and is not: every call but the
    /// first of a window is two comparisons and an index, and going through it
    /// is what lets a twelve-bit entry be read without caring whether it
    /// straddles anything.
    fn table_byte(&mut self, at: u64) -> io::Result<Option<u8>> {
        if at >= self.geometry.fat_bytes {
            return Ok(None);
        }
        let size = u64::from(self.geometry.sector_size);
        let start = (at / size) * size;
        if self.window_at != Some(start) {
            // A table whose last window runs past the end of the region is a
            // short image rather than a wrong number, so the read failing is
            // the answer and the chain stops.
            let offset = self
                .geometry
                .fat_start
                .checked_add(start)
                .ok_or_else(|| malformed("a table outside the volume"))?;
            self.source.read_exact_at(offset, &mut self.window)?;
            self.window_at = Some(start);
        }
        Ok(self.window.get((at - start) as usize).copied())
    }

    /// The cluster after this one.
    fn next_cluster(&mut self, cluster: u32) -> io::Result<Next> {
        let flavour = self.geometry.flavour;
        let Some((at, width)) = format::table_entry_at(flavour, cluster) else {
            return Ok(Next::Broken);
        };
        let mut bytes = [0u8; 4];
        for (index, byte) in bytes.iter_mut().enumerate().take(width) {
            match self.table_byte(at.saturating_add(index as u64))? {
                Some(value) => *byte = value,
                None => return Ok(Next::Broken),
            }
        }
        let Some(entry) = format::table_entry(flavour, cluster, &bytes[..width]) else {
            return Ok(Next::Broken);
        };
        if format::chain_ends(flavour, entry) {
            return Ok(Next::End);
        }
        if !self.geometry.holds(entry) {
            return Ok(Next::Broken);
        }
        Ok(Next::Cluster(entry))
    }

    /// Where a directory's records are, as byte offsets of equal-sized chunks.
    ///
    /// Byte offsets rather than cluster numbers because a FAT12 or FAT16 root
    /// directory has no cluster numbers at all — it is a fixed run of sectors
    /// between the table and the heap. Reducing both cases to offsets is what
    /// keeps the record parser below from having to know that.
    fn layout(&mut self, place: &Place) -> io::Result<(Vec<u64>, usize, bool)> {
        match *place {
            Place::Fixed { start, bytes } => {
                let chunk = u64::from(self.geometry.sector_size);
                let bytes = bytes.min(MAX_DIRECTORY_BYTES);
                let mut offsets = Vec::new();
                let mut at = 0u64;
                while at < bytes {
                    offsets.push(start.saturating_add(at));
                    at += chunk;
                }
                Ok((offsets, chunk as usize, true))
            }
            Place::Chain { first, contiguous } => {
                let cluster_bytes = self.geometry.cluster_bytes();
                if cluster_bytes == 0 {
                    return Ok((Vec::new(), 0, false));
                }
                // Two bounds, and both are needed. A directory is not allowed
                // to be larger than a real one could be; and a chain cannot
                // visit more clusters than the volume has, which is what stops
                // a loop that is shorter than the first bound.
                let limit = MAX_DIRECTORY_BYTES.div_ceil(cluster_bytes).max(1);
                let limit = limit.min(u64::from(self.geometry.cluster_count)) as usize;

                let mut offsets = Vec::new();
                let mut whole = true;
                if let Some(length) = contiguous {
                    // exFAT's "no FAT chain": the clusters follow one another
                    // and the table has nothing to say. The common case on a
                    // card out of a camera, and the reason a large exFAT
                    // volume can be walked with almost no table reads at all.
                    let wanted = length.div_ceil(cluster_bytes);
                    for step in 0..wanted.min(limit as u64) {
                        let Some(cluster) = u32::try_from(u64::from(first) + step)
                            .ok()
                            .filter(|cluster| self.geometry.holds(*cluster))
                        else {
                            whole = false;
                            break;
                        };
                        match self.geometry.cluster_offset(cluster) {
                            Some(at) => offsets.push(at),
                            None => {
                                whole = false;
                                break;
                            }
                        }
                    }
                    if wanted > limit as u64 {
                        whole = false;
                    }
                } else {
                    let mut cluster = first;
                    loop {
                        if !self.geometry.holds(cluster) {
                            whole = false;
                            break;
                        }
                        match self.geometry.cluster_offset(cluster) {
                            Some(at) => offsets.push(at),
                            None => {
                                whole = false;
                                break;
                            }
                        }
                        match self.next_cluster(cluster)? {
                            Next::End => break,
                            Next::Broken => {
                                whole = false;
                                break;
                            }
                            Next::Cluster(next) => {
                                // Checked here rather than before the read, so
                                // that a chain which ends exactly at the bound
                                // is a whole directory rather than a truncated
                                // one. What this catches is a chain with
                                // another link in it — a directory larger than
                                // kosha will read, or one that loops, which
                                // are the same thing from here: what was read
                                // is a floor.
                                if offsets.len() >= limit {
                                    whole = false;
                                    break;
                                }
                                cluster = next;
                            }
                        }
                    }
                }
                Ok((offsets, cluster_bytes as usize, whole))
            }
        }
    }
}

/// What one entry of a directory says, in whichever format it was written.
///
/// The seam between the two formats. A FAT record gives a 32-bit size and a
/// name assembled out of up to twenty pieces; an exFAT record set gives a
/// 64-bit one and a name in UTF-16 — and by the time either reaches here it is
/// a name, a length, a cluster and two flags.
pub struct Entry<'a> {
    /// UTF-8, whichever of the three encodings it was stored in.
    pub name: &'a [u8],
    pub cluster: u32,
    /// What the record says the file's length is. Zero for a FAT directory,
    /// which does not record one.
    pub length: u64,
    /// exFAT's "no FAT chain" flag, which is meaningless in FAT and always
    /// false there.
    pub contiguous: bool,
    pub is_directory: bool,
    pub mtime: u32,
}

/// What walking a directory cost and whether all of it was read.
pub struct Walked {
    /// False when a chain broke, a record set was truncated, or the bound
    /// above was reached. The tree records it as a partial directory rather
    /// than reporting a smaller disk.
    pub whole: bool,
    /// The directory's own bytes, which the walk already knows because it
    /// followed the chain. FAT does not record a directory's length anywhere,
    /// so this is the only place it comes from.
    pub allocated: u64,
}

/// Every entry of a directory, whatever shape it is stored in.
///
/// `each` is handed a name and the few numbers behind it, and nothing is kept:
/// the name lives in `name`, which is rewritten for every entry, so a caller
/// that wants to keep one copies it. That is the same bargain the XFS reader
/// makes, and for the same reason — a directory of a hundred thousand entries
/// should cost one allocation, not a hundred thousand.
pub fn entries<R: ReadAt>(
    fs: &mut Filesystem<R>,
    place: &Place,
    buffer: &mut Vec<u8>,
    name: &mut Vec<u8>,
    each: impl FnMut(Entry<'_>),
) -> io::Result<Walked> {
    let (offsets, chunk, mut whole) = fs.layout(place)?;
    let allocated = (offsets.len() as u64).saturating_mul(chunk as u64);
    if chunk == 0 || offsets.is_empty() {
        return Ok(Walked { whole, allocated });
    }
    buffer.resize(chunk, 0);

    let exfat = fs.geometry.flavour.is_exfat();
    let mut fat = LongName::default();
    let mut set = RecordSet::default();
    let mut each = each;

    for offset in offsets {
        if fs.source.read_exact_at(offset, buffer).is_err() {
            // One unreadable cluster of a directory is the rest of the
            // directory still being worth reading. Half a directory is worth
            // more than none, and the tree says it is partial.
            whole = false;
            continue;
        }
        let stop = if exfat {
            read_exfat_chunk(buffer, &mut set, name, &mut whole, &mut each)
        } else {
            read_fat_chunk(buffer, &mut fat, name, &mut whole, &mut each)
        };
        if stop {
            // The first record never written. Nothing after it has ever been
            // used, in either format, so there is no reason to read the rest
            // of a directory that was allocated a megabyte and holds four
            // files. A break rather than a return, because the check below
            // applies to a directory that ended as much as to one that ran
            // out — more so, in fact: half an entry followed by the end of
            // the directory is precisely what an interrupted write leaves.
            break;
        }
    }

    // **A directory whose last records are the beginning of an entry and not
    // the whole of it.** Both formats spread one entry over several records —
    // FAT puts a long name in front of the record it belongs to, exFAT puts a
    // file's length and name after it — so a filesystem interrupted part way
    // through creating one leaves exactly this, and so does a chain that
    // stopped early. The entry is real and is not in the tree, which is what
    // `partial` is for.
    if fat.ok || set.open {
        whole = false;
    }
    Ok(Walked { whole, allocated })
}

/// The pieces of a long name, as they arrive.
///
/// **Backwards, which is not an accident of anybody's.** The last piece is
/// stored first, so that software reading forwards meets the end of the name
/// before it meets the record the name belongs to — and can therefore tell,
/// without seeking, how many records to skip.
struct LongName {
    units: [u16; format::MAX_LONG_PARTS * format::LONG_NAME_CHARS],
    /// How many pieces the first one said there would be.
    parts: usize,
    /// The ordinal the next piece must carry, counting down to zero.
    expected: u8,
    checksum: u8,
    ok: bool,
}

impl Default for LongName {
    fn default() -> LongName {
        LongName {
            units: [0; format::MAX_LONG_PARTS * format::LONG_NAME_CHARS],
            parts: 0,
            expected: 0,
            checksum: 0,
            ok: false,
        }
    }
}

/// Read one chunk of a FAT directory. Returns whether the directory ended.
fn read_fat_chunk(
    chunk: &[u8],
    long: &mut LongName,
    name: &mut Vec<u8>,
    whole: &mut bool,
    each: &mut impl FnMut(Entry<'_>),
) -> bool {
    for record in chunk.as_chunks::<{ format::ENTRY_BYTES }>().0 {
        match format::slot(record) {
            Slot::End => return true,
            Slot::Free => long.ok = false,
            Slot::Long(piece) => {
                if piece.last {
                    long.ok = (1..=format::MAX_LONG_PARTS as u8).contains(&piece.ordinal);
                    long.parts = usize::from(piece.ordinal);
                    long.checksum = piece.checksum;
                    long.expected = piece.ordinal;
                    long.units = [0; format::MAX_LONG_PARTS * format::LONG_NAME_CHARS];
                }
                if long.ok && piece.ordinal == long.expected && piece.checksum == long.checksum {
                    let at = (usize::from(piece.ordinal) - 1) * format::LONG_NAME_CHARS;
                    long.units[at..at + format::LONG_NAME_CHARS].copy_from_slice(&piece.chars);
                    long.expected = piece.ordinal - 1;
                } else {
                    // A run of pieces that does not hold together is a name
                    // that was half-rewritten, or one whose record a tool that
                    // predates long names deleted. The short name is still
                    // there and is still true.
                    long.ok = false;
                }
            }
            Slot::Short(short) => {
                name.clear();
                if long.ok && long.expected == 0 && format::checksum(short.raw) == long.checksum {
                    format::push_utf16(&long.units[..long.parts * format::LONG_NAME_CHARS], name);
                }
                long.ok = false;
                if name.is_empty() {
                    format::push_short_name(short.raw, short.case, name);
                }
                if name.is_empty() {
                    // A record with a name of nothing but spaces. It is not an
                    // entry anybody can act on, and it is not nothing either.
                    *whole = false;
                    continue;
                }
                if name.as_slice() == b"." || name.as_slice() == b".." {
                    continue;
                }
                each(Entry {
                    name,
                    cluster: short.cluster,
                    length: u64::from(short.size),
                    contiguous: false,
                    is_directory: short.is_directory(),
                    mtime: short.mtime,
                });
            }
        }
    }
    false
}

/// A file's records, as they arrive.
///
/// One record says it is a file and how many follow; the next says where the
/// bytes are; the rest are the name, fifteen characters each. None of them
/// means anything alone.
/// The longest name exFAT allows, in UTF-16 units. Seventeen name records of
/// fifteen each, less the five the format does not use.
const MAX_EXFAT_NAME_UNITS: usize = 255;

struct RecordSet {
    open: bool,
    /// Records still to come in this set, counted down from what the first one
    /// said.
    remaining: u8,
    attributes: u16,
    mtime: u32,
    zone: u8,
    stream: bool,
    flags: u8,
    cluster: u32,
    length: u64,
    /// How many UTF-16 units the name has, which the stream record states and
    /// the name records then have to supply exactly.
    units_wanted: usize,
    units: [u16; MAX_EXFAT_NAME_UNITS],
    filled: usize,
    /// What the set's records add up to so far, and what the first of them
    /// said they should.
    checksum: u16,
    wanted_checksum: u16,
    ok: bool,
}

impl Default for RecordSet {
    fn default() -> RecordSet {
        RecordSet {
            open: false,
            remaining: 0,
            attributes: 0,
            mtime: 0,
            zone: 0,
            stream: false,
            flags: 0,
            cluster: 0,
            length: 0,
            units_wanted: 0,
            units: [0; MAX_EXFAT_NAME_UNITS],
            filled: 0,
            checksum: 0,
            wanted_checksum: 0,
            ok: false,
        }
    }
}

impl RecordSet {
    fn reset(&mut self) {
        self.open = false;
        self.stream = false;
        self.filled = 0;
        self.units_wanted = 0;
        self.ok = false;
    }
}

/// Read one chunk of an exFAT directory. Returns whether the directory ended.
fn read_exfat_chunk(
    chunk: &[u8],
    set: &mut RecordSet,
    name: &mut Vec<u8>,
    whole: &mut bool,
    each: &mut impl FnMut(Entry<'_>),
) -> bool {
    for record in chunk.as_chunks::<{ format::ENTRY_BYTES }>().0 {
        match format::record(record) {
            Record::End => return true,
            Record::File {
                secondaries,
                attributes,
                mtime,
                offset,
                checksum,
            } => {
                if set.open {
                    // A set that never finished. Its file is real and was not
                    // counted, which is exactly what `partial` is for.
                    *whole = false;
                }
                set.reset();
                set.open = true;
                set.remaining = secondaries;
                set.attributes = attributes;
                set.mtime = mtime;
                set.zone = offset;
                set.wanted_checksum = checksum;
                set.checksum = format::set_checksum(0, record, true);
                // A stream record and at least one name record, or there is no
                // file here to name.
                set.ok = secondaries >= 2;
                if secondaries == 0 {
                    *whole = false;
                    set.reset();
                }
            }
            other => {
                if !set.open {
                    // A secondary record with no primary in front of it. Either
                    // the directory starts mid-set, which a damaged chain can
                    // produce, or this is a record type that stands alone —
                    // the allocation bitmap and the upcase table both do, and
                    // neither is a file.
                    continue;
                }
                set.checksum = format::set_checksum(set.checksum, record, false);
                match other {
                    Record::Stream {
                        flags,
                        name_units,
                        cluster,
                        length,
                    } if !set.stream => {
                        set.stream = true;
                        set.flags = flags;
                        set.cluster = cluster;
                        set.length = length;
                        set.units_wanted = usize::from(name_units);
                        if set.units_wanted == 0 || set.units_wanted > set.units.len() {
                            set.ok = false;
                        }
                    }
                    Record::Name { chars } if set.stream => {
                        for unit in chars {
                            if set.filled < set.units_wanted {
                                set.units[set.filled] = unit;
                                set.filled += 1;
                            }
                        }
                    }
                    // A record type invented after this reader, or one out of
                    // order. It is still part of the set and still counted, so
                    // that the set ends where the first record said it would.
                    _ => set.ok = false,
                }
                set.remaining = set.remaining.saturating_sub(1);
                if set.remaining == 0 {
                    finish(set, name, whole, each);
                }
            }
        }
    }
    false
}

/// A record set that has run to its end: emit it, or say it could not be read.
fn finish(
    set: &mut RecordSet,
    name: &mut Vec<u8>,
    whole: &mut bool,
    each: &mut impl FnMut(Entry<'_>),
) {
    let complete = set.ok
        && set.stream
        && set.filled == set.units_wanted
        && set.units_wanted > 0
        && set.checksum == set.wanted_checksum;
    if !complete {
        *whole = false;
        set.reset();
        return;
    }
    name.clear();
    format::push_utf16(&set.units[..set.filled], name);
    if name.is_empty() {
        *whole = false;
        set.reset();
        return;
    }
    // exFAT has no `.` or `..` records at all — a directory's parent is where
    // it was reached from and nothing else. So there is nothing to filter here,
    // and a name that reads `.` would be a real entry somebody made.
    each(Entry {
        name,
        cluster: set.cluster,
        length: set.length,
        contiguous: Record::is_contiguous(set.flags),
        is_directory: Record::is_directory(set.attributes),
        mtime: format::with_zone(set.mtime, set.zone),
    });
    set.reset();
}

/// Where the root directory is, in the form a walk wants.
pub fn root(geometry: &Geometry) -> Place {
    match geometry.root {
        format::Root::Fixed { start, bytes } => Place::Fixed { start, bytes },
        format::Root::Cluster(cluster) => Place::Chain {
            first: cluster,
            contiguous: None,
        },
    }
}

/// Where a child's bytes are, from the entry that named it.
///
/// No flavour is needed: only the exFAT parser ever sets `contiguous`, because
/// only exFAT has a flag for it, so a FAT entry always produces a chain.
pub fn place_of(entry: &Entry<'_>) -> Place {
    Place::Chain {
        first: entry.cluster,
        contiguous: entry.contiguous.then_some(entry.length),
    }
}

fn malformed(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}
