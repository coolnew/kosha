// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reading directories out of an ISO 9660 filesystem.
//!
//! The layer between [`format`](super::format), which turns bytes into
//! numbers, and [`build`](super::build), which turns a walk into a tree.
//!
//! Two things happen here that do not happen in [`format`]: choosing which of
//! a disc's trees to read, and spending a read to follow a name that did not
//! fit where it was written.
//!
//! **No file is ever read.** A file's length and its first block are both in
//! the record that names it, and the blocks after that one are the blocks
//! after that one — a medium nobody can rewrite has no reason to scatter
//! anything. So the whole of a scan is directories.

use std::io;

use crate::vhdx::ReadAt;

use super::format::{self, Continuation, Naming, Volume};

/// The most bytes of records this reader will read as one directory.
///
/// A directory's length comes off the disc, and a `u32` of it reaches four
/// gigabytes. Sixty-four megabytes is a directory of several hundred thousand
/// entries, past anything a disc holds in one.
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

/// How many times a name may be chased into a continuation area.
///
/// One is the case that happens; the rest is a bound rather than an
/// expectation. A chain of these is how a damaged disc asks a reader to walk
/// for ever.
const MAX_CONTINUATIONS: usize = 8;

/// The most of a continuation area to read.
///
/// The area is a handful of System Use entries — 237 bytes on the disc this
/// was written against. The length is a `u32` off the disc, so it is bounded
/// for the same reason everything else here is.
const MAX_CONTINUATION_BYTES: u64 = 8 * 2048;

/// An open ISO 9660 filesystem.
pub struct Filesystem<R> {
    source: R,
    volume: Volume,
    naming: Naming,
}

impl<R: ReadAt> Filesystem<R> {
    /// Read the volume descriptors and decide which tree to walk.
    ///
    /// **The choice between trees is made here and nowhere else**, so that
    /// everything above this line walks one tree without knowing there was a
    /// second. It is made the way `mount` makes it: Rock Ridge where the disc
    /// has it, Joliet where it does not, and the published names where there
    /// is neither.
    pub fn open(mut source: R) -> io::Result<Filesystem<R>> {
        let mut block = vec![0u8; format::DESCRIPTOR_BYTES];
        let mut primary: Option<Volume> = None;
        let mut joliet: Option<Volume> = None;

        for index in 0..format::MAX_DESCRIPTORS {
            let at = format::DESCRIPTORS_AT + (index * format::DESCRIPTOR_BYTES) as u64;
            source.read_exact_at(at, &mut block)?;
            let Some(kind) = format::descriptor(&block) else {
                // The first block that is not a descriptor ends the list,
                // whether or not a terminator was written. A disc with no
                // descriptor at all fails below, where the reason is known.
                break;
            };
            if format::is_terminator(kind) {
                break;
            }
            if format::is_primary(kind) && primary.is_none() {
                primary = Some(volume_of(&block)?);
            } else if format::is_supplementary(kind) && joliet.is_none() {
                // **A supplementary descriptor that will not parse is one tree
                // fewer, not a dead disc.** The primary tree is where the
                // files are; this one is a second set of names over the same
                // files, and refusing the whole disc because the spare copy of
                // the names is damaged would throw away everything to save
                // nothing.
                //
                // One that parses and is not Joliet describes a tree in some
                // other encoding this reader has no rule for, and taking it
                // would produce names made of the wrong bytes.
                if let Ok(supplementary) = format::volume(&block)
                    && supplementary.joliet
                {
                    joliet = Some(supplementary);
                }
            }
        }

        let primary = primary.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, format::Unreadable::NotIso)
        })?;

        let mut filesystem = Filesystem {
            source,
            volume: primary,
            naming: Naming::Published,
        };
        if filesystem.announces_rock_ridge()? {
            filesystem.naming = Naming::RockRidge;
        } else if let Some(joliet) = joliet {
            filesystem.volume = joliet;
            filesystem.naming = Naming::Joliet;
        }
        Ok(filesystem)
    }

    pub fn volume(&self) -> &Volume {
        &self.volume
    }

    pub fn naming(&self) -> Naming {
        self.naming
    }

    /// Whether the root directory's own record carries the `SP` entry.
    ///
    /// **One read, and it decides how every name on the disc is spelt.** The
    /// `SP` entry lives in the `.` record of the root directory and nowhere
    /// else; without it the bytes after a name belong to whoever wrote the
    /// disc, and parsing them as Rock Ridge would invent names out of
    /// somebody else's data.
    fn announces_rock_ridge(&mut self) -> io::Result<bool> {
        let Some(at) = self.volume.block_offset(self.volume.root_extent) else {
            return Ok(false);
        };
        let mut block = vec![0u8; self.volume.block_size as usize];
        if self.source.read_exact_at(at, &mut block).is_err() {
            return Ok(false);
        }
        let Some(record) = format::record(&block) else {
            return Ok(false);
        };
        if !record.is_self() {
            return Ok(false);
        }
        Ok(format::announces_sharing(record.system_use))
    }

    /// Read a directory's records into `buffer`.
    ///
    /// A block at a time, and stopping at the first that will not read. One
    /// read of the whole thing would be fewer calls and would turn a disc
    /// whose last block is missing — a copy that ran out of room, the ordinary
    /// way an image is damaged — into a directory with nothing in it rather
    /// than one with most of it.
    fn read_directory(&mut self, extent: u32, size: u64, buffer: &mut Vec<u8>) -> bool {
        let block_size = u64::from(self.volume.block_size);
        if block_size == 0 {
            return false;
        }
        let wanted = size.min(MAX_DIRECTORY_BYTES);
        let blocks = wanted.div_ceil(block_size);
        buffer.clear();

        let mut whole = size <= MAX_DIRECTORY_BYTES;
        for step in 0..blocks {
            let Some(block) = u32::try_from(u64::from(extent) + step).ok() else {
                whole = false;
                break;
            };
            let Some(at) = self.volume.block_offset(block) else {
                whole = false;
                break;
            };
            let was = buffer.len();
            buffer.resize(was + block_size as usize, 0);
            if self.source.read_exact_at(at, &mut buffer[was..]).is_err() {
                buffer.truncate(was);
                whole = false;
                break;
            }
        }
        buffer.truncate(wanted as usize);
        whole
    }

    /// Read a continuation area, where a name that did not fit was put.
    fn read_continuation(&mut self, at: &Continuation, spare: &mut Vec<u8>) -> io::Result<()> {
        let length = u64::from(at.length).min(MAX_CONTINUATION_BYTES);
        if length == 0 {
            return Err(malformed("a continuation area of no bytes"));
        }
        let offset = self
            .volume
            .block_offset(at.block)
            .and_then(|block| block.checked_add(u64::from(at.offset)))
            .ok_or_else(|| malformed("a continuation area outside the disc"))?;
        spare.clear();
        spare.resize(length as usize, 0);
        self.source.read_exact_at(offset, spare)
    }
}

fn volume_of(block: &[u8]) -> io::Result<Volume> {
    format::volume(block).map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))
}

/// What one entry of a directory says.
pub struct Entry<'a> {
    /// UTF-8, whichever of the three ways the disc spelt it.
    pub name: &'a [u8],
    /// The first block of the file, which is also how a directory is reached.
    pub extent: u32,
    /// Every extent of the file added together, which for all but a file over
    /// four gigabytes is one number read from one record.
    pub size: u64,
    pub is_directory: bool,
    /// What a Rock Ridge `PX` mode said, where there was one. A link is a link
    /// first, exactly as in the walk: a symbolic link to a directory is measured
    /// as itself and is not descended into.
    pub is_symlink: bool,
    pub mtime: u32,
}

/// Every entry of a directory.
///
/// `each` is handed a name and the few numbers behind it, and nothing is kept:
/// the name lives in `name`, which is rewritten for every entry.
pub fn entries<R: ReadAt>(
    fs: &mut Filesystem<R>,
    extent: u32,
    size: u64,
    buffer: &mut Vec<u8>,
    spare: &mut Vec<u8>,
    name: &mut Vec<u8>,
    mut each: impl FnMut(Entry<'_>),
) -> io::Result<bool> {
    let mut whole = fs.read_directory(extent, size, buffer);
    let block_size = fs.volume.block_size as usize;
    if block_size == 0 || buffer.is_empty() {
        return Ok(false);
    }
    let naming = fs.naming;

    // A file larger than four gigabytes is several records in a row with the
    // same name, each but the last flagged to continue. The bytes add up and
    // the first record says where they start.
    let mut carried = 0u64;
    let mut carried_extent: Option<u32> = None;

    let mut at = 0usize;
    while at < buffer.len() {
        if buffer[at] == 0 {
            // The rest of this block is padding: the standard forbids a record
            // from straddling a block boundary, so one that would have to is
            // pushed into the next block and zeros fill the gap.
            let next = (at / block_size + 1) * block_size;
            if next <= at {
                break;
            }
            at = next;
            continue;
        }

        let Some(record) = format::record(&buffer[at..]) else {
            // A length that runs past the end of what was read. Everything
            // before it is still true.
            whole = false;
            break;
        };
        let length = record.length;
        let is_directory = record.is_directory();
        let continues = record.continues();
        let extent = record.extent;
        let size = u64::from(record.size);
        let mtime = record.mtime;

        // `.` and `..` are entries like any other here, and following either
        // is a cycle rather than a subtree.
        if record.is_self() || record.is_parent() {
            at += length;
            continue;
        }

        name.clear();
        // Only a Rock Ridge disc says what kind of thing an entry is beyond
        // "directory or not"; the other two dialects have nowhere to put it.
        let mut mode = None;
        match naming {
            Naming::Joliet => format::push_joliet_name(record.name, name),
            Naming::Published => format::push_published_name(record.name, name),
            Naming::RockRidge => {
                // A record on a Rock Ridge disc that has no `NM` is not an
                // error — the extension is per record, and a name that was
                // already legal needs no second spelling.
                if !rock_ridge_name(fs, record.system_use, spare, name, &mut mode)? {
                    format::push_published_name(record.name, name);
                }
            }
        }
        let is_symlink = mode.is_some_and(format::mode_is_symlink);
        if name.is_empty() {
            whole = false;
            at += length;
            continue;
        }

        if continues {
            carried = carried.saturating_add(size);
            carried_extent.get_or_insert(extent);
            at += length;
            continue;
        }

        each(Entry {
            name,
            extent: carried_extent.take().unwrap_or(extent),
            size: carried.saturating_add(size),
            is_directory: is_directory && !is_symlink,
            is_symlink,
            mtime,
        });
        carried = 0;
        at += length;
    }

    // A file whose last extent was never written. Its earlier records are
    // real, its total is not known, and neither is a thing to report as a
    // finished entry.
    if carried_extent.is_some() {
        whole = false;
    }
    Ok(whole)
}

/// Assemble a Rock Ridge name, following it off the record if it went there.
///
/// Returns whether one was found at all.
fn rock_ridge_name<R: ReadAt>(
    fs: &mut Filesystem<R>,
    area: &[u8],
    spare: &mut Vec<u8>,
    out: &mut Vec<u8>,
    mode: &mut Option<u32>,
) -> io::Result<bool> {
    let (mut found, mut more, mut hop) = pieces(area, out, mode);

    let mut hops = 0usize;
    while more && hops < MAX_CONTINUATIONS {
        let Some(at) = hop.take() else { break };
        hops += 1;
        if fs.read_continuation(&at, spare).is_err() {
            break;
        }
        let (also, again, next) = pieces(spare, out, mode);
        found |= also;
        more = again;
        hop = next;
    }
    Ok(found)
}

/// Walk one System Use area, taking the pieces of a name out of it.
///
/// Three answers: whether a name was found, whether the last piece said more
/// is coming, and where to look for it. `more` starts true so that an area
/// holding only a `CE` — which is where a very long name ends up entirely —
/// is followed rather than treated as a record with no name.
fn pieces(
    area: &[u8],
    out: &mut Vec<u8>,
    mode: &mut Option<u32>,
) -> (bool, bool, Option<Continuation>) {
    let mut found = false;
    let mut more = true;
    let mut next = None;
    format::system_use(area, |entry| {
        if let Some(continues) = format::push_name(&entry, out) {
            found = true;
            more = continues;
        } else if let Some(read) = format::posix_mode(&entry) {
            // The first `PX` wins. A record has one, and a second would be a
            // damaged area rather than a second opinion.
            mode.get_or_insert(read);
        } else if next.is_none() {
            next = format::continuation(&entry);
        }
        true
    });
    (found, more, next)
}

fn malformed(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}
