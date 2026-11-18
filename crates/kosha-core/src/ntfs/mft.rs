// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reading `$MFT`, from wherever it is.
//!
//! Every file on an NTFS volume has a record in one file, and reading that file
//! once is what lets a whole drive be measured in seconds. This is the part
//! that walks it: the bootstrap through record zero, the pass over every
//! record, and the one read of `$Secure` that turns a security id into an
//! account.
//!
//! **Not "from a volume".** It used to be — this was the top half of
//! [`volume`](super::volume), which opens `\\.\C:` and needs administrator
//! and Windows. None of the logic here needs any of that: it needs somewhere to
//! read aligned bytes from, and a disk image is somewhere. Splitting the two
//! apart is what lets an NTFS image be measured on a machine that cannot mount
//! one — a Mac, a Linux box, or a Windows box belonging to somebody who is not
//! an administrator of it.
//!
//! The alignment rule travels with the reader rather than with the source,
//! because it is the *device* that insists on it and a file that does not:
//! every read here is a whole number of clusters at a whole number of clusters,
//! out of an [`Aligned`] buffer, which satisfies an unbuffered device and costs
//! a file nothing.

use std::io;

use super::boot::Geometry;
use super::build::{SecurityFragment, SecurityStream, Table};
use super::record::{self, Names, Summary};
use super::runs;
use super::security;
use std::alloc;

/// How much `$MFT` to hold at once.
///
/// Large enough that the read is sequential from the disk's point of view and
/// small enough that a volume with a gigabyte of MFT does not need a gigabyte
/// of memory to scan. Every value in this range measures the same; the number
/// matters only in that it is neither one record nor all of them.
const CHUNK: usize = 8 * 1024 * 1024;

/// Somewhere `$MFT` can be read from.
///
/// One method, deliberately. [`crate::vhdx::ReadAt`] is the richer trait for an
/// image — it knows about sparse blocks and about where a read will really land
/// — and none of that means anything to a raw device, which is the other thing
/// that has to satisfy this. What both can do is fill a buffer from an offset.
pub trait Source {
    /// Fill `buffer` from `offset`.
    ///
    /// Both are already a whole number of clusters, and `buffer` begins at a
    /// sector-aligned address. An unbuffered device read requires all three; a
    /// file ignores all three.
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;

    /// How many bytes there are altogether, where that is knowable.
    ///
    /// `None` means *unknown*, never zero, and the volume's own numbers are then
    /// taken as read. A partition inside an image knows its length exactly,
    /// which is the case that matters: an image is a file somebody was sent, and
    /// every number in its boot sector is that person's claim about a disk this
    /// program has no other way to measure.
    fn len(&self) -> Option<u64> {
        None
    }
}

/// An NTFS volume's master file table, and the geometry to make sense of it.
pub struct Mft<S> {
    source: S,
    geometry: Geometry,
}

impl<S: Source> Mft<S> {
    /// Read the boot sector and take the volume's geometry from it.
    ///
    /// Fails where there is no NTFS boot sector at the front, which is how a
    /// caller learns that whatever it handed over is not an NTFS volume — the
    /// same answer for a FAT partition in a disk image as for a drive letter
    /// that turned out to be exFAT.
    pub fn open(mut source: S) -> io::Result<Mft<S>> {
        // The boot sector is the first sector, and until it has been read there
        // is no way to know how big a sector is. 4096 is the largest any volume
        // uses and is a whole number of every smaller size, so one read of it is
        // aligned whatever the answer turns out to be.
        let mut first = Aligned::new(4096, 4096);
        source.read_exact_at(0, first.as_mut())?;
        let geometry = super::boot::parse(first.as_ref()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "this does not begin with an NTFS boot sector",
            )
        })?;

        // A volume cannot be larger than the medium it is on, and this is the
        // only check in the whole reader with ground truth behind it: every
        // other number here is one the volume states about itself. It matters
        // because those numbers are multiplied — total sectors decides how many
        // clusters there are, which bounds how much of them `$MFT` may occupy,
        // which decides how many records there are, which is one table entry
        // each, reserved before a record has been read. A boot sector saying
        // 2^40 sectors is eight bytes, and in a 256 KiB image those eight bytes
        // asked for 192 GB and aborted the process.
        //
        // It is also the answer to a real accident rather than a hostile file: a
        // disk image that stopped downloading is a volume larger than its
        // medium, and saying so is better than failing somewhere inside it.
        if let Some(medium) = source.len()
            && geometry
                .total_clusters
                .checked_mul(geometry.bytes_per_cluster)
                .is_none_or(|bytes| bytes > medium)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "this volume claims to be larger than the medium it is on",
            ));
        }

        Ok(Mft { source, geometry })
    }

    /// Read `bytes` from `offset`, both of which must already be aligned.
    fn read(&mut self, buffer: &mut Aligned, offset: u64, bytes: usize) -> io::Result<()> {
        self.source
            .read_exact_at(offset, &mut buffer.as_mut()[..bytes])
    }

    /// Where `$MFT`'s own data lives, from record zero of `$MFT` itself.
    ///
    /// The bootstrap the whole strategy turns on: the extent map of the MFT is
    /// stored in the MFT, in the one record whose position the boot sector
    /// gives directly. Everything else is reachable only through it.
    fn mft_extents(&mut self) -> io::Result<Vec<runs::Run>> {
        let record_size = self.geometry.bytes_per_record as usize;
        // Records are read a cluster at a time so the offset is always aligned;
        // a 1 KiB record inside a 4 KiB cluster would not be.
        let span = record_size.max(self.geometry.bytes_per_cluster as usize);
        let mut buffer = Aligned::new(span, self.geometry.bytes_per_sector as usize);

        let offset = self
            .geometry
            .offset_of(self.geometry.mft_cluster)
            .ok_or_else(|| invalid("the MFT's position overflows the volume"))?;
        self.read(&mut buffer, offset, span)?;

        let stream = super::record::data_stream(&mut buffer.as_mut()[..record_size], &[])
            .ok_or_else(|| invalid("$MFT record 0 does not describe where $MFT is"))?;
        let runs =
            runs::decode(stream.runs).ok_or_else(|| invalid("$MFT's run list is malformed"))?;
        if runs.is_empty() {
            return Err(invalid("$MFT claims to occupy no clusters"));
        }

        // What `total_clusters` is documented as being for, and was not used
        // for. A run list is a handful of bytes and the numbers inside it are up
        // to eight wide: eleven bytes can say that `$MFT` occupies a terabyte,
        // and nothing downstream doubts it — `read_table` multiplies the claim
        // into one table entry per record before it reads a record. `$MFT` is a
        // file on the volume, so it cannot hold more clusters than the volume
        // has, and no extent of it can end past the last one.
        let total = self.geometry.total_clusters;
        let claimed = runs
            .iter()
            .fold(0u64, |sum, run| sum.saturating_add(run.clusters));
        if claimed > total {
            return Err(invalid("$MFT claims more clusters than the volume holds"));
        }
        for run in &runs {
            // A sparse run occupies no clusters and so lies nowhere.
            if let Some(start) = run.start
                && start.saturating_add(run.clusters) > total
            {
                return Err(invalid("an $MFT extent lies outside the volume"));
            }
        }
        Ok(runs)
    }

    /// Read every record in `$MFT`, in order, into a [`Table`].
    ///
    /// `publish` is handed the number of records read so far, often enough to
    /// keep a progress bar honest and rarely enough not to matter.
    pub fn read_table(
        &mut self,
        mut publish: impl FnMut(u64),
        mut cancelled: impl FnMut() -> bool,
    ) -> io::Result<Table> {
        let geometry = self.geometry;
        let record_size = geometry.bytes_per_record as usize;
        let cluster = geometry.bytes_per_cluster as usize;
        let extents = self.mft_extents()?;

        let clusters: u64 = extents.iter().map(|run| run.clusters).sum();
        let total_records = clusters
            .saturating_mul(geometry.bytes_per_cluster)
            .saturating_div(geometry.bytes_per_record.into());
        // Node ids are `u32` and a record number has to be one. Four billion
        // records is four terabytes of MFT; nothing comes close, and the check
        // is here so that if something ever does it is refused rather than
        // silently wrapped into somebody else's parent pointer.
        if total_records > u32::MAX as u64 {
            return Err(invalid("$MFT holds more records than a scan can address"));
        }

        let mut table = Table {
            records: vec![Summary::default(); total_records as usize],
            names: Vec::with_capacity(total_records as usize),
            owners: Vec::with_capacity(total_records as usize),
            name_bytes: Vec::with_capacity(total_records as usize * 24),
            security_stream: SecurityStream::default(),
        };

        let chunk = CHUNK.next_multiple_of(cluster);
        let mut buffer = Aligned::new(chunk, geometry.bytes_per_sector as usize);
        let mut number: u64 = 0;

        for run in &extents {
            // A sparse run in `$MFT` is a hole: no records live there, and the
            // numbering skips past it. Every `Summary` in the gap stays
            // `default()`, which is not in use, so nothing reaches them.
            let Some(start) = run.start else {
                number += run.clusters * geometry.bytes_per_cluster / record_size as u64;
                continue;
            };

            let mut done: u64 = 0;
            while done < run.clusters {
                if cancelled() {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "the scan was cancelled",
                    ));
                }
                let take = (run.clusters - done).min(chunk as u64 / cluster as u64);
                let bytes = (take * cluster as u64) as usize;
                let offset = geometry
                    .offset_of(start + done)
                    .ok_or_else(|| invalid("an $MFT extent lies outside the volume"))?;
                self.read(&mut buffer, offset, bytes)?;

                for block in buffer.as_mut()[..bytes].chunks_mut(record_size) {
                    // `$Secure`'s `$SDS` run list is the only thing in the
                    // whole pass worth copying out of the buffer — a few
                    // hundred bytes, against recomputing where a particular
                    // record sits inside a fragmented `$MFT` afterwards.
                    //
                    // `$SDS` is not in record 9; it is in whichever records
                    // name record 9 as their base, and there can be several of
                    // them. See [`SecurityStream`]. The base reference is read
                    // out of the raw header *before* the fixups, which is sound
                    // and is the point: it lies at offset 0x20, well inside the
                    // first sector and nowhere near the two bytes at the end of
                    // one that the update sequence array displaces. It is a
                    // filter and nothing more — `data_stream` applies the
                    // fixups properly on the copy, and rejects the record if
                    // they do not hold.
                    //
                    // Taken before `parse` because both apply the fixups, and
                    // whichever ran second would find the record already undone.
                    if is_secure_record(block, number) {
                        let mut copy = block.to_vec();
                        if let Some(stream) = record::data_stream(&mut copy, &security::SDS_STREAM)
                        {
                            if stream.start_vcn == 0 {
                                table.security_stream.logical = stream.logical;
                            }
                            table.security_stream.fragments.push(SecurityFragment {
                                start_vcn: stream.start_vcn,
                                runs: stream.runs.to_vec(),
                            });
                        }
                    }
                    if let Some(slot) = table.records.get_mut(number as usize) {
                        let before = table.names.len();
                        let mut names = Names {
                            arena: &mut table.name_bytes,
                            refs: &mut table.names,
                        };
                        if let Some(summary) = record::parse(block, &mut names) {
                            *slot = summary;
                        }
                        // Which record each new name belongs to. Appended here
                        // rather than carried inside the name, which is already
                        // the width of its own padding.
                        let owner = u32::try_from(number).unwrap_or(u32::MAX);
                        table.owners.resize(table.names.len(), owner);
                        debug_assert!(table.names.len() >= before);
                    }
                    number += 1;
                }

                done += take;
                publish(number);
            }
        }

        Ok(table)
    }

    /// Read `$Secure`'s `$SDS` stream, and turn it into owners by security id.
    ///
    /// One read of a stream that is a few megabytes at the outside, against the
    /// one call per file the ordinary walk pays for the same answer. That ratio
    /// is the entire reason per-user attribution is worth having here.
    ///
    /// The fragments arrive in the order their records went past, which is not
    /// the order of the stream — [`security::order`] is what puts them back and
    /// why a gap between two of them stops the read.
    ///
    /// Unbuffered like everything else in this file, so the stream is read in
    /// whole clusters and cut back to its real length afterwards. A sparse run
    /// reads as the zeros it is, which the parser treats as padding.
    pub fn read_security(&mut self, stream: &SecurityStream) -> io::Result<security::Security> {
        let cluster = self.geometry.bytes_per_cluster as usize;

        let (extents, clusters) = security::order(&stream.fragments);

        // The attribute header is read off the disk, so its length is not a
        // number to trust with an allocation. Capped rather than refused: a
        // truncated table names fewer owners and the ones it does name are
        // still right, where refusing outright would lose every one of them.
        let wanted = stream
            .logical
            .min(security::MAX_BYTES)
            .min(clusters.saturating_mul(cluster as u64)) as usize;
        if wanted == 0 {
            return Ok(security::Security::default());
        }

        let mut sds: Vec<u8> = Vec::with_capacity(wanted);
        let mut buffer = Aligned::new(
            CHUNK.min(wanted.next_multiple_of(cluster)),
            self.geometry.bytes_per_sector as usize,
        );
        let span = buffer.as_ref().len();

        'runs: for run in &extents {
            let mut done: u64 = 0;
            while done < run.clusters {
                if sds.len() >= wanted {
                    break 'runs;
                }
                let take = (run.clusters - done).min(span as u64 / cluster as u64);
                let bytes = (take * cluster as u64) as usize;
                match run.start {
                    Some(start) => {
                        let offset = self
                            .geometry
                            .offset_of(start + done)
                            .ok_or_else(|| invalid("a $SDS extent lies outside the volume"))?;
                        self.read(&mut buffer, offset, bytes)?;
                        sds.extend_from_slice(&buffer.as_ref()[..bytes]);
                    }
                    // A hole. The bytes are zeros, and writing them out keeps
                    // every later entry at the offset the stream says it is at.
                    None => sds.resize(sds.len() + bytes, 0),
                }
                done += take;
            }
        }

        sds.truncate(wanted);
        Ok(security::owners(&sds))
    }
}

/// Whether this block could hold part of `$Secure`.
///
/// Record 9 itself, or any record naming it as a base. Cheap enough to run over
/// a million and a half records: a four-byte magic and an eight-byte read.
fn is_secure_record(block: &[u8], number: u64) -> bool {
    if number == u64::from(security::SECURE_RECORD) {
        return true;
    }
    if block.len() < 0x28 || &block[0..4] != b"FILE" {
        return false;
    }
    /// The low 48 bits are the record number; the top 16 are a sequence number.
    const REFERENCE_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
    let base = u64::from_le_bytes(
        block[0x20..0x28]
            .try_into()
            .expect("a slice of exactly eight bytes"),
    );
    base & REFERENCE_MASK == u64::from(security::SECURE_RECORD)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// ------------------------------------------------------------ aligned buffer

/// A heap buffer whose address is a multiple of the volume's sector size.
///
/// `Vec<u8>` is aligned to one byte, and an unbuffered read into one fails with
/// `ERROR_INVALID_PARAMETER` — which is a confusing way to be told the address
/// was odd. This is the smallest thing that satisfies the requirement.
struct Aligned {
    ptr: *mut u8,
    layout: alloc::Layout,
}

impl Aligned {
    /// `size` is rounded up to a whole number of `align`, which is what makes
    /// the *length* of every read legal as well as its address.
    fn new(size: usize, align: usize) -> Aligned {
        let align = align.max(1).next_power_of_two();
        let size = size.next_multiple_of(align).max(align);
        let layout = alloc::Layout::from_size_align(size, align)
            .expect("a sector-aligned buffer of a few megabytes is a valid layout");
        // SAFETY: `layout` has a non-zero size, which is this function's only
        // obligation.
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            alloc::handle_alloc_error(layout);
        }
        Aligned { ptr, layout }
    }
}

impl AsRef<[u8]> for Aligned {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `ptr` is a live allocation of exactly `layout.size()` bytes,
        // zeroed at allocation and therefore initialised, and this borrow keeps
        // it so for its lifetime.
        unsafe { std::slice::from_raw_parts(self.ptr, self.layout.size()) }
    }
}

impl AsMut<[u8]> for Aligned {
    fn as_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `as_ref`, with unique access borrowed from `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed` with this exact layout and is
        // freed once.
        unsafe { alloc::dealloc(self.ptr, self.layout) };
    }
}

// The pointer is owned exclusively by this struct and never aliased, so the
// buffer is as safe to move between threads as the `Vec` it replaces.
// SAFETY: sole ownership of a plain byte allocation.
unsafe impl Send for Aligned {}
