// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! One MFT record, read as bytes.
//!
//! A record is a fixed-size block — 1024 bytes on every volume anybody has
//! measured — holding a small header and then a list of variable-length
//! *attributes*. Four of them matter here:
//!
//! | | Attribute | What it gives us |
//! | --- | --- | --- |
//! | `0x10` | `$STANDARD_INFORMATION` | modification time, the DOS attribute bits |
//! | `0x30` | `$FILE_NAME` | the name, and the record number of the parent |
//! | `0x80` | `$DATA` | the apparent size and the exact on-disk size |
//! | `0xC0` | `$REPARSE_POINT` | whether a directory is a mounted volume |
//!
//! That `$DATA` row is the reason this module exists rather than
//! `FSCTL_ENUM_USN_DATA`. The USN enumeration hands back a name and a parent
//! for every record in one ioctl and looks like exactly the right tool — but a
//! `USN_RECORD` carries no size at any version, so it can build the tree and
//! not one of the numbers in it. The attribute header here carries both sizes,
//! and carries them *exactly* for sparse and compressed files, which is the one
//! case the ordinary Windows walk has to open the file to get right.
//!
//! Parsed from a byte slice, so the whole format is under test on whichever
//! platform this is being edited on.

use std::ffi::OsString;

/// Attribute type codes.
const STANDARD_INFORMATION: u32 = 0x10;
const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const REPARSE_POINT: u32 = 0xC0;
/// Terminates the attribute list.
const END: u32 = 0xFFFF_FFFF;

/// Record header flags.
const IN_USE: u16 = 0x0001;
const IS_DIRECTORY: u16 = 0x0002;

/// Attribute header flags.
const ATTRIBUTE_COMPRESSED: u16 = 0x0001;
const ATTRIBUTE_SPARSE: u16 = 0x8000;

/// `$FILE_NAME` namespaces. The 8.3 name is a second name for the same file and
/// would otherwise show up as a phantom sibling of every long name on the
/// volume. Acted on in [`build`](super::build), not here.
pub const NAMESPACE_DOS: u8 = 2;

/// The attribute bits NTFS keeps in a record, and the only ones worth reading
/// out of one.
///
/// Everything above the low sixteen is NTFS' own: a directory marker, an index
/// marker, and flags with no Win32 meaning at all. Two of those collide with
/// attributes that *do* have a Win32 meaning — `RECALL_ON_OPEN` at `0x40000`
/// and `RECALL_ON_DATA_ACCESS` at `0x400000` — and both of those are runtime
/// attributes the cloud filter driver synthesises rather than anything on the
/// disk. Reading them from a record is not merely unreliable; it is reading a
/// field that never holds them.
pub const ON_DISK_ATTRIBUTES: u32 = 0x0000_FFFF;

/// A record's own number, and its parent's, are 48 bits of a 64-bit reference;
/// the top 16 are a sequence number that increments as the record is reused.
const REFERENCE_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// What one record says about the file it describes.
///
/// Deliberately not an `EntryMeta`: that type is about a directory entry seen
/// through the Win32 API, and half its fields (`device`, `link_key`,
/// `mounted_volume`) are answers to questions the MFT settles differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Summary {
    /// The base record this one belongs to, when it is an extension record
    /// holding attributes that would not fit. Zero for an ordinary record —
    /// record zero is `$MFT` itself, which is never anybody's extension.
    pub base: u32,
    pub in_use: bool,
    pub is_dir: bool,
    /// The file attribute bits, masked to the ones NTFS actually stores.
    ///
    /// **Not what `GetFileAttributes` returns**, and the difference is a trap.
    /// The field on disk holds NTFS' own bookkeeping in its high bits, which
    /// Win32 masks off on the way out — `C:\Program Files-Zipz.exe` reads
    /// `0x00040020` here and `0x20` through the API. That stray `0x40000` is
    /// `FILE_ATTRIBUTE_RECALL_ON_OPEN` in the Win32 numbering, so taking the
    /// field at face value marked every such file as a cloud placeholder whose
    /// bytes are not on this disk. See [`ON_DISK_ATTRIBUTES`].
    pub attributes: u32,
    /// The reparse tag, when this is a reparse point. Zero otherwise.
    ///
    /// The honest source for "the data is not here": the `RECALL_ON_*`
    /// attribute bits are synthesised at runtime by the cloud filter driver and
    /// are never written to a record, so a placeholder can only be recognised
    /// from its tag.
    pub reparse_tag: u32,
    /// Unix seconds, saturating, from `$STANDARD_INFORMATION`.
    pub mtime: u32,
    pub created: Option<u64>,
    /// This file's entry in the volume's shared table of security descriptors,
    /// from `$STANDARD_INFORMATION`. Zero when the record predates NTFS 3.0 or
    /// carries the shorter form of the attribute, both of which mean there is
    /// nothing to look up.
    ///
    /// The whole reason per-user attribution is free in an MFT scan and costs a
    /// call per file in the ordinary walk: a million records share a few dozen
    /// descriptors, and `$Secure` maps the ids to them in one read. See
    /// [`security`](super::security).
    pub security_id: u32,
    /// Apparent size of the unnamed `$DATA` stream.
    pub logical: u64,
    /// Bytes that stream occupies on disk — exact, including for sparse and
    /// compressed files.
    pub allocated: u64,
    /// Whether an unnamed `$DATA` was found at all. A directory has none, and
    /// so does a file whose data lives in an extension record.
    pub has_data: bool,
    /// On-disk bytes of this record's *named* `$DATA` streams.
    ///
    /// Ignored for ordinary files, whose alternate data streams the directory
    /// walk does not see either. It exists for one case: a file compressed by
    /// the Windows Overlay Filter — CompactOS, and what most installers now
    /// use — keeps an empty sparse unnamed stream and puts the real bytes in a
    /// stream called `WofCompressedData`. Windows reports *that* stream's
    /// allocation as the file's size on disk, so reading only the unnamed one
    /// reports a 550 KB file as occupying nothing.
    pub named_data_allocated: u64,
    /// This is a reparse point whose target is a volume rather than a
    /// directory: a mounted volume, which is a filesystem boundary rather than
    /// a link. See `platform::reparse`.
    pub is_mounted_volume: bool,
}

/// One name a file is known by, and the directory it is known by it in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Name {
    /// Record number of the directory holding this name.
    pub parent: u32,
    /// Offset into the caller's byte arena.
    pub off: u32,
    pub len: u16,
    /// Which namespace this name is in. Kept because the 8.3 name has to be
    /// dropped only when the same *file* has a long one, and the two can be in
    /// different records — so the decision cannot be made here. See
    /// [`build::consolidate`](super::build::consolidate).
    pub namespace: u8,
}

/// Where names go: a byte arena and the references into it.
///
/// Passed in rather than returned so that parsing a million records costs a
/// million *pushes* rather than a million allocations.
pub struct Names<'a> {
    pub arena: &'a mut Vec<u8>,
    pub refs: &'a mut Vec<Name>,
}

/// Read one record in place, appending its names to `names`.
///
/// In place because applying the fixups (below) rewrites the buffer, and
/// copying every record to avoid that would double the cost of the only pass
/// this scan makes.
///
/// `None` means "this is not a record": the block is zeros, or the magic is
/// wrong, or a length in it points outside itself. The MFT has plenty of all
/// three — records past the initialised length are zeros, and a deleted record
/// keeps whatever it last held — so this is the ordinary case rather than an
/// error, and the caller skips the block and carries on.
pub fn parse(record: &mut [u8], names: &mut Names<'_>) -> Option<Summary> {
    let header = fixup(record)?;

    let mut summary = Summary {
        base: header.base,
        in_use: header.in_use,
        is_dir: header.is_dir,
        ..Summary::default()
    };
    if !header.in_use {
        // A deleted record still parses, and its bytes are still on the disk,
        // but they belong to no file: counting them would report space that a
        // `dir` of the same volume cannot see and that nothing can reclaim.
        return Some(summary);
    }

    for attribute in attributes(record, header.first_attribute) {
        match attribute.kind {
            STANDARD_INFORMATION => {
                if let Some(value) = attribute.resident_value()
                    && value.len() >= 0x24
                {
                    summary.mtime = filetime_to_unix(u64_at(value, 0x08));
                    summary.created = u64_at(value, 0)
                        .checked_sub(116_444_736_000_000_000)
                        .map(|ticks| ticks / 10_000_000);
                    summary.attributes = u32_at(value, 0x20) & ON_DISK_ATTRIBUTES;
                    // The attribute comes in two lengths. The short one — 0x30
                    // bytes, NTFS 1.2 — stops before the security id, and a
                    // volume upgraded from it keeps short records until each
                    // file is next written. Reading past the end of the value
                    // would pick up whatever attribute follows, so the length
                    // is the test rather than the volume version.
                    if value.len() >= 0x48 {
                        summary.security_id = u32_at(value, 0x34);
                    }
                }
            }
            FILE_NAME => {
                // Every name, 8.3 included. Which of them survives is decided
                // once the extension records have been folded back onto their
                // bases, because a file's long name and its short name are not
                // always in the same record.
                push_name(&attribute, names);
            }
            DATA => {
                // Only the unnamed stream. A named one is an alternate data
                // stream, which the directory walk does not see either, and
                // counting it on one path and not the other would make the two
                // strategies disagree about a file neither of them is wrong
                // about.
                if let Some((logical, allocated)) = attribute.data_sizes() {
                    if attribute.name_length == 0 {
                        summary.logical = logical;
                        summary.allocated = allocated;
                        summary.has_data = true;
                    } else {
                        summary.named_data_allocated =
                            summary.named_data_allocated.saturating_add(allocated);
                    }
                }
            }
            REPARSE_POINT => {
                // The same `REPARSE_DATA_BUFFER` the ordinary walk pays an open
                // and an ioctl for, sitting here in the record with nothing to
                // ask.
                use crate::platform::reparse;
                if let Some(value) = attribute.resident_value()
                    && value.len() >= 4
                {
                    summary.reparse_tag = u32_at(value, 0);
                    summary.is_mounted_volume =
                        reparse::targets_a_volume(value, reparse::MOUNT_POINT_TAG);
                }
            }
            _ => {}
        }
    }

    Some(summary)
}

/// UTF-16 as the operating system would hand it to us.
///
/// On Windows this is the same WTF-8 encoding `OsStr::as_encoded_bytes` gives
/// for a name that came back from `FindFirstFileW`, so a name read out of the
/// MFT and the same name read out of a directory listing are byte-identical —
/// which is what lets the two strategies produce comparable trees. Elsewhere
/// this is only ever reached by the tests, where lossy is exactly right.
#[cfg(windows)]
fn os_name(units: &[u16]) -> OsString {
    use std::os::windows::ffi::OsStringExt;
    OsString::from_wide(units)
}

#[cfg(not(windows))]
fn os_name(units: &[u16]) -> OsString {
    OsString::from(String::from_utf16_lossy(units))
}

/// A non-resident `$DATA` stream: where its clusters are, and how long it is.
pub struct Stream<'a> {
    /// The encoded run list, for [`runs::decode`](super::runs::decode).
    pub runs: &'a [u8],
    /// The stream's real length in bytes.
    ///
    /// Zero for every fragment but the first: NTFS keeps the sizes only in the
    /// attribute record holding cluster zero, and repeats the fields as zeros
    /// in the continuations.
    pub logical: u64,
    /// The first cluster of the stream this fragment covers.
    ///
    /// A stream too fragmented to describe in one attribute record is continued
    /// in another, and the continuations are in no particular order — record
    /// 83680 of this machine's `C:` holds `$SDS` from cluster 249 while record
    /// 2022 holds it from cluster 0. Without this the two cannot be put back in
    /// order, and bytes read in the wrong order are not a shorter answer but a
    /// wrong one.
    pub start_vcn: u64,
}

/// Find a non-resident `$DATA` stream by name and say where it lives.
///
/// Only two callers, and both are files this strategy has to read rather than
/// merely measure: `$MFT`'s own unnamed stream — the bootstrap the whole
/// strategy turns on, since the extent map of the MFT is stored in the MFT —
/// and `$Secure`'s `$SDS`, which holds every security descriptor on the volume.
/// Every *ordinary* file's numbers come out of the attribute header without
/// following anything, which is what makes the strategy cheap.
///
/// `stream` is the attribute name as UTF-16 units, empty for the unnamed one.
///
/// Takes the record mutably for the same reason [`parse`] does: the fixups have
/// to be undone before any offset in it is believable.
pub fn data_stream<'a>(record: &'a mut [u8], stream: &[u16]) -> Option<Stream<'a>> {
    let header = fixup(record)?;
    if !header.in_use {
        return None;
    }
    let first = header.first_attribute;
    // Found by index rather than by returning the borrow out of the iterator,
    // which would hold `record` borrowed for the rest of the function.
    let found = attributes(record, first)
        .find(|a| a.kind == DATA && a.non_resident && a.is_named(stream))
        .map(|a| {
            (
                a.at,
                a.bytes.len(),
                usize::from(u16_at(a.bytes, 0x20)),
                a.data_sizes().map_or(0, |(logical, _)| logical),
                u64_at(a.bytes, 0x10),
            )
        });

    let (at, length, offset, logical, start_vcn) = found?;
    if offset >= length {
        return None;
    }
    Some(Stream {
        runs: record.get(at + offset..at + length)?,
        logical,
        start_vcn,
    })
}

// ------------------------------------------------------------------- header

struct Header {
    in_use: bool,
    is_dir: bool,
    base: u32,
    first_attribute: usize,
}

/// Verify and undo the update sequence array.
///
/// NTFS writes a per-record counter over the last two bytes of every sector the
/// record covers, and keeps the displaced bytes in an array in the header. A
/// torn write shows up as a sector whose last two bytes do not match the
/// counter, which is how the filesystem knows the record is half-written. We
/// check the same thing and refuse the record when it does not hold: a record
/// that failed its own integrity check is not one to build a tree out of.
///
/// The sector stride comes from the array's own length rather than from the
/// boot sector, because the two disagree on a 4Kn volume, and the record is the
/// one that knows how it was written.
fn fixup(record: &mut [u8]) -> Option<Header> {
    if record.len() < 48 || &record[0..4] != b"FILE" {
        return None;
    }

    let usa_offset = usize::from(u16_at(record, 0x04));
    let usa_count = usize::from(u16_at(record, 0x06));
    let flags = u16_at(record, 0x16);
    let first_attribute = usize::from(u16_at(record, 0x14));
    let base = u32::try_from(u64_at(record, 0x20) & REFERENCE_MASK).ok()?;

    if first_attribute >= record.len() || usa_count == 0 {
        return None;
    }
    let usa_end = usa_offset.checked_add(usa_count * 2)?;
    if usa_end > record.len() || usa_offset < 42 {
        return None;
    }

    let sectors = usa_count - 1;
    if sectors > 0 {
        if !record.len().is_multiple_of(sectors) {
            return None;
        }
        let stride = record.len() / sectors;
        if stride < 4 {
            return None;
        }
        let check = [record[usa_offset], record[usa_offset + 1]];
        for sector in 0..sectors {
            let tail = (sector + 1) * stride - 2;
            if record[tail..tail + 2] != check {
                return None;
            }
            let saved = usa_offset + 2 + sector * 2;
            record[tail] = record[saved];
            record[tail + 1] = record[saved + 1];
        }
    }

    Some(Header {
        in_use: flags & IN_USE != 0,
        is_dir: flags & IS_DIRECTORY != 0,
        base,
        first_attribute,
    })
}

// --------------------------------------------------------------- attributes

/// One attribute, still as bytes.
struct Attribute<'a> {
    kind: u32,
    flags: u16,
    name_length: u8,
    non_resident: bool,
    /// Where this attribute begins within the record.
    at: usize,
    /// The whole attribute, header included: every offset inside it is
    /// relative to its own start.
    bytes: &'a [u8],
}

impl Attribute<'_> {
    /// Whether this attribute is called `name`, given as UTF-16 units.
    ///
    /// Compared unit by unit out of the record rather than decoded first: the
    /// two names this is ever asked about are `$SDS` and the empty one, and
    /// building a string to compare against four ASCII characters would be the
    /// only allocation in the parse.
    fn is_named(&self, name: &[u16]) -> bool {
        if usize::from(self.name_length) != name.len() {
            return false;
        }
        if name.is_empty() {
            return true;
        }
        let offset = usize::from(u16_at(self.bytes, 0x0A));
        let Some(bytes) = self.bytes.get(offset..offset + name.len() * 2) else {
            return false;
        };
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .zip(name)
            .all(|(pair, want)| u16::from_le_bytes(*pair) == *want)
    }

    /// The value of a resident attribute.
    fn resident_value(&self) -> Option<&[u8]> {
        if self.non_resident || self.bytes.len() < 0x18 {
            return None;
        }
        let length = u32_at(self.bytes, 0x10) as usize;
        let offset = usize::from(u16_at(self.bytes, 0x14));
        self.bytes.get(offset..offset.checked_add(length)?)
    }

    /// Apparent and on-disk size of this attribute's data.
    ///
    /// For a resident attribute — a file small enough to live inside its own
    /// MFT record — the on-disk size is the value rounded to eight bytes, not
    /// to a cluster, because it occupies no cluster at all. That is the number
    /// Explorer shows, and the one the cluster-rounding estimate in the
    /// ordinary walk necessarily gets wrong.
    ///
    /// For a non-resident one it is `AllocatedLength`, except where the
    /// attribute is compressed or sparse: those carry a compression unit and a
    /// further field holding what is *actually* allocated, which is smaller and
    /// is what Windows reports as the allocation size. Reading the first field
    /// for a compressed file would report the space it would have taken had it
    /// not been compressed — which is the saving, stated as though it had not
    /// happened.
    fn data_sizes(&self) -> Option<(u64, u64)> {
        if !self.non_resident {
            let length = u64::from(u32_at(self.bytes.get(0x10..0x14)?, 0));
            return Some((length, length.next_multiple_of(8)));
        }
        if self.bytes.len() < 0x40 {
            return None;
        }
        // Only the fragment holding VCN zero carries the sizes; a continuation
        // in an extension record repeats the fields as zeros.
        if u64_at(self.bytes, 0x10) != 0 {
            return None;
        }

        let allocated = u64_at(self.bytes, 0x28);
        let logical = u64_at(self.bytes, 0x30);
        let compression_unit = u16_at(self.bytes, 0x22);
        let compressed = self.flags & (ATTRIBUTE_COMPRESSED | ATTRIBUTE_SPARSE) != 0;

        if compressed && compression_unit != 0 && self.bytes.len() >= 0x48 {
            return Some((logical, u64_at(self.bytes, 0x40)));
        }
        Some((logical, allocated))
    }
}

/// Walk a record's attributes.
///
/// Stops at the end marker, at the end of the buffer, or at the first attribute
/// whose declared length would take the cursor outside the record or leave it
/// where it started. All three are corruption, and all three are reached by
/// simply stopping: what was read before the bad length is still true.
fn attributes(record: &[u8], first: usize) -> impl Iterator<Item = Attribute<'_>> {
    let mut at = first;
    std::iter::from_fn(move || {
        let header = record.get(at..at.checked_add(16)?)?;
        let kind = u32_at(header, 0);
        if kind == END {
            return None;
        }
        let length = u32_at(header, 4) as usize;
        // Eight-aligned and non-zero: without both, a corrupt length is an
        // infinite loop rather than a short read.
        if length < 16 || !length.is_multiple_of(8) {
            return None;
        }
        let bytes = record.get(at..at.checked_add(length)?)?;
        at += length;

        let attribute = Attribute {
            kind,
            flags: u16_at(bytes, 0x0C),
            name_length: bytes[0x09],
            non_resident: bytes[0x08] != 0,
            at: at - length,
            bytes,
        };
        Some(attribute)
    })
}

// -------------------------------------------------------------------- names

/// Decode one `$FILE_NAME` straight into the caller's arena.
///
/// ```text
/// 0x00  8  parent directory reference
/// 0x28  8  allocated size   (stale: the copy here is not maintained)
/// 0x30  8  real size        (likewise)
/// 0x38  4  attribute flags
/// 0x40  1  name length, in UTF-16 units
/// 0x41  1  namespace
/// 0x42  …  the name
/// ```
///
/// The sizes in it are deliberately ignored. NTFS updates them lazily and they
/// are routinely wrong by the whole content of the file; `$DATA` is the one
/// that is maintained.
///
/// Decoding into the arena rather than returning a name keeps the UTF-16 in a
/// stack buffer for the width of this call. The alternative — handing back a
/// `&[u16]` — cannot be done from a `&[u8]` record without either an allocation
/// per name or an unaligned read, and there are a million names.
fn push_name(attribute: &Attribute<'_>, names: &mut Names<'_>) -> bool {
    let Some(value) = attribute.resident_value() else {
        return false;
    };
    if value.len() < 0x42 {
        return false;
    }
    let Ok(parent) = u32::try_from(u64_at(value, 0x00) & REFERENCE_MASK) else {
        return false;
    };
    let length = usize::from(value[0x40]);
    let namespace = value[0x41];
    if length == 0 {
        return false;
    }
    let Some(bytes) = value.get(0x42..0x42 + length * 2) else {
        return false;
    };

    // A name is at most 255 UTF-16 units, which the format fixes and the
    // single-byte length field could not exceed anyway.
    let mut units = [0u16; 255];
    for (unit, pair) in units[..length].iter_mut().zip(bytes.as_chunks::<2>().0) {
        *unit = u16::from_le_bytes(*pair);
    }

    let text = os_name(&units[..length]);
    let encoded = text.as_encoded_bytes();
    let Ok(len) = u16::try_from(encoded.len()) else {
        return false;
    };
    let off = names.arena.len() as u32;
    names.arena.extend_from_slice(encoded);
    names.refs.push(Name {
        parent,
        off,
        len,
        namespace,
    });
    true
}

// ------------------------------------------------------------------ scalars

/// Windows FILETIME is 100-nanosecond ticks since 1601-01-01.
///
/// Shared with `platform::windows`, which needs the identical conversion for
/// the ordinary walk: the two strategies must agree about what a modification
/// time is, or every row of a comparison between them differs.
#[inline]
pub fn filetime_to_unix(ticks: u64) -> u32 {
    const TICKS_PER_SEC: u64 = 10_000_000;
    const EPOCH_DELTA_SECS: u64 = 11_644_473_600;
    let secs = ticks / TICKS_PER_SEC;
    secs.saturating_sub(EPOCH_DELTA_SECS)
        .min(u64::from(u32::MAX)) as u32
}

#[inline]
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[inline]
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[inline]
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}
