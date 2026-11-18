// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! A scan, kept.
//!
//! A snapshot is a whole [`SizeTree`] written to a file, taken deliberately and
//! stamped with when. Reading one back gives a tree that browses exactly like a
//! fresh scan, without touching the disk it describes.
//!
//! ## Historical records
//!
//! An explicit snapshot is a named record opened deliberately by the user.
//! The GUI also keeps one automatic historical record per drive root, separate
//! from the explicit snapshot list. Saved scans display their age so a tree
//! recorded earlier is distinguishable from a fresh scan. Neither record is
//! updated in place to track filesystem changes.
//!
//! ## The format
//!
//! ```text
//! "KOSHASNP"          8 bytes, so a wrong file is refused rather than parsed
//! u32                 format version
//! u32                 header length
//! <header>            length-prefixed key/value pairs, UNCOMPRESSED
//! <body>              zlib: nodes, names, owners, creation times (v2)
//! ```
//!
//! **zlib rather than raw deflate**, for the four bytes of Adler-32 on the end
//! of it. Raw deflate has no trailer, so a file with its last byte missing
//! still decodes — the missing byte was bit-padding — and a snapshot cut short
//! by a full disk would read back as a complete tree that happened to stop
//! early. The checksum turns every truncation and every flipped bit into an
//! error at the point of reading, which is the only place it can still be
//! explained to somebody.
//!
//! **The header is not compressed, and that is the point of having one.** The
//! window lists every snapshot it has every time it opens: their root, their
//! date, their size, how many entries. Inflating tens of megabytes to draw a
//! row in a sidebar would make keeping snapshots feel expensive, which is
//! exactly the wrong incentive for a feature whose value comes from having
//! taken one earlier.
//!
//! **Fields are written one at a time, little-endian.** Never a memcpy of the
//! struct: [`Node`] is 48 bytes in memory and 44 on disk, the difference being
//! padding the compiler adds for alignment and a file has no reason to keep. A
//! format that copied the struct would be a format that changed when the
//! compiler did.
//!
//! Version 2 appends a u64 creation-table length (zero or node count) and that
//! many u64 Unix-second birth times; u64::MAX means unknown. Version 1 remains
//! readable and has no creation times. No modification time is substituted.
//!
//! **The body is deflated** because this is a tool for reclaiming disk space
//! and it must not be a glutton. A million-entry home directory is about 80 MB
//! of arena and names, which is not a thing to ask somebody to keep ten of.
//!
//! **Every length in the body is a claim, and the body is somebody else's
//! file.** A snapshot is made to be kept and copied, and deflate turns a byte
//! of file into a kilobyte of stream, so a claim is checked against what the
//! tree could possibly need *before* it is read rather than after.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::owner::{Owner, OwnerId, OwnerKind, Owners};
use crate::tree::{ScanStats, SizeTree, Strategy};

/// First eight bytes of every snapshot file.
pub const MAGIC: &[u8; 8] = b"KOSHASNP";

/// The format this build writes. Bumped when the layout below changes in a way
/// an older reader would misread; a reader refuses anything it does not know.
pub const FORMAT: u32 = 2;

/// The file extension, without the dot.
pub const EXTENSION: &str = "kosha";

/// Bytes per entry on disk.
///
/// Forty-four, where [`Node`] is forty-eight in memory: the difference is four
/// bytes of padding the compiler adds to keep the struct's alignment, and a
/// file format has no reason to carry them. Writing the fields one at a time is
/// what makes that true, and this constant is what keeps the reader honest
/// about it — the two disagreeing by four bytes per entry was exactly the bug
/// this was written to not have twice.
const RECORD: usize = 44;

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("not a kosha snapshot")]
    NotASnapshot,
    #[error(
        "this snapshot is version {found}, and this build of kosha reads up to {FORMAT}. \
         A newer kosha wrote it."
    )]
    Newer { found: u32 },
    #[error("the snapshot is damaged: {0}")]
    Damaged(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What a snapshot says about itself, readable without inflating the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// When it was taken, as Unix seconds.
    pub taken: u64,
    /// The millisecond within that second.
    ///
    /// Seconds alone were the whole identity once, and two snapshots taken in
    /// the same second were one snapshot: the second one's filename collided
    /// with the first's and replaced it, silently, which is the worst way for
    /// a tool whose entire promise is "this is kept" to fail. This is what
    /// makes a name unique, and it is also what orders a pair of them that a
    /// clock in seconds calls simultaneous.
    ///
    /// Zero in snapshots written before this field existed, which is the right
    /// answer for them: they were taken at some unrecorded point in their
    /// second and the start of it is as good a guess as any.
    pub taken_ms: u32,
    /// The path that was scanned.
    pub root: PathBuf,
    /// A name somebody gave it. Empty when they did not, which is the usual
    /// case — the date and the root are already a description.
    pub label: String,
    pub entries: u64,
    pub dirs: u64,
    pub files: u64,
    pub bytes_allocated: u64,
    pub bytes_logical: u64,
    /// Which strategy produced the original scan, spelled as its label. Kept as
    /// text rather than as the enum: a snapshot outlives the build that wrote
    /// it, and an unknown strategy should read back as a word rather than fail.
    pub strategy: String,
    /// What the original scan found inside a disk image, and what it chose:
    /// `ext4`, or `NTFS · partition 2 of 2 (GPT)`. Empty when it did not read
    /// one.
    ///
    /// Kept for the same reason `strategy` is: the snapshot is the only record
    /// left of where these numbers came from, and "which of this disk's
    /// filesystems am I looking at" is not a question a date and a path can
    /// answer.
    pub image: String,
    /// True when the original scan could not read everything, so every total
    /// here is a floor. Carried because a snapshot that lost this would invite
    /// a comparison against it to report changes that never happened.
    pub partial: bool,
    /// True when what made it partial was the depth ceiling rather than
    /// something unreadable.
    ///
    /// A key the older format did not have, which costs nothing either way: a
    /// reader that does not know it ignores it, and one that does defaults it to
    /// false — which is what every snapshot written before this was, since a
    /// depth-limited scan did not record itself as partial at all.
    pub depth_limited: bool,
    /// The user stopped this scan. Older records did not carry this field.
    pub cancelled: bool,
    /// Entries the original scan could not read.
    ///
    /// Along with the three below, this is the rest of what the scan knew
    /// about itself. They are here because leaving them out produced a
    /// snapshot that said "incomplete — the scan could not read everything"
    /// and then, four lines later, "unreadable 0". A record that contradicts
    /// itself on screen is worse than one that admits it forgot.
    pub unreadable: u64,
    pub skipped_other_device: u64,
    pub hardlinks_deduped: u64,
    /// Whether the scan counted a hardlinked file's bytes once.
    ///
    /// The *rule*, not the count above it: a tree with no hardlinks folds none
    /// under either rule, so the count cannot tell two records apart. Carried
    /// for the reason `partial` is carried — a comparison across two different
    /// rules reports changes that never happened, and measured it reports them
    /// by the gigabyte: five names for one 8 MiB file came back as 32 MiB of
    /// growth.
    ///
    /// Missing from snapshots written before this field existed, where it
    /// decodes as `false`. That is the honest default rather than a guess: the
    /// walk is what wrote most of them, and on Windows the walk does not dedup.
    pub dedup_hardlinks: bool,
    /// Whether the scan descended into other filesystems.
    ///
    /// Carried for the same reason and with the same hole beneath it:
    /// `skipped_other_device` counts boundaries *not* crossed, which is zero
    /// on a tree with none under either rule. A record that crossed, held
    /// against a scan that did not, reports every file on every mount inside
    /// the tree as gone.
    pub cross_filesystems: bool,
    /// Whether the original scan measured on-disk sizes or estimated them.
    ///
    /// `None` for a record written before this was carried, which is the only
    /// honest answer for one: it did not say, so nothing is claimed about it.
    /// The alternative was to assume, and an assumption here either warns
    /// about every old record on Linux or stays silent about the ones that
    /// matter on Windows.
    ///
    /// See [`ScanStats::exact_allocated`](crate::ScanStats::exact_allocated)
    /// for what the difference does to a comparison: four files reported as
    /// having grown, over a directory nobody had touched.
    pub exact_allocated: Option<bool>,
    pub max_depth: u8,
    /// How long the original scan took. Reported as the scan's own time, never
    /// as the time to open the snapshot — those are different facts and the
    /// header above says which one this line belongs to.
    pub duration_ms: u64,
    /// The kosha that wrote it.
    pub tool: String,
}

impl Meta {
    /// Describe a tree, for a snapshot taken now.
    pub fn of(tree: &SizeTree, taken: SystemTime, label: impl Into<String>) -> Meta {
        let stats = tree.stats();
        Meta {
            taken: taken
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs(),
            taken_ms: taken
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .subsec_millis(),
            root: tree.root_path().to_path_buf(),
            label: label.into(),
            entries: stats.entries,
            dirs: stats.dirs,
            files: stats.files,
            bytes_allocated: stats.bytes_allocated,
            bytes_logical: stats.bytes_logical,
            strategy: stats.strategy.label().to_string(),
            image: stats.image.clone().unwrap_or_default(),
            partial: stats.partial,
            depth_limited: stats.depth_limited,
            cancelled: stats.cancelled,
            unreadable: stats.unreadable,
            skipped_other_device: stats.skipped_other_device,
            hardlinks_deduped: stats.hardlinks_deduped,
            dedup_hardlinks: stats.dedup_hardlinks,
            cross_filesystems: stats.cross_filesystems,
            exact_allocated: Some(stats.exact_allocated),
            max_depth: stats.max_depth,
            duration_ms: stats.duration.as_millis().min(u128::from(u64::MAX)) as u64,
            tool: crate::version::long(),
        }
    }

    /// When it was taken, in milliseconds since the epoch.
    ///
    /// The one number to order snapshots by, and to decide which of two is the
    /// baseline. Saturating rather than wrapping, because a stamp far enough
    /// in the future to overflow this is a broken clock and not a reason to
    /// wrap around to 1970.
    pub fn at_ms(&self) -> u64 {
        self.taken
            .saturating_mul(1000)
            .saturating_add(u64::from(self.taken_ms))
    }

    /// How long ago it was taken, from a clock the caller supplies.
    ///
    /// `None` when the snapshot is stamped in the future, which means a clock
    /// somewhere is wrong and "in -3 days" is not an improvement on saying
    /// nothing.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        let now = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
        now.checked_sub(self.taken).map(Duration::from_secs)
    }
}

// ------------------------------------------------------------------- writing

/// Write `tree` as a snapshot.
pub fn write<W: Write>(tree: &SizeTree, meta: &Meta, mut out: W) -> io::Result<()> {
    let header = encode_header(meta);

    out.write_all(MAGIC)?;
    out.write_all(&FORMAT.to_le_bytes())?;
    out.write_all(&(header.len() as u32).to_le_bytes())?;
    out.write_all(&header)?;

    // Fast rather than small. The gap between this and maximum compression is
    // a few percent of the file and several times the time, and the wait is in
    // front of somebody who just pressed a button.
    let mut body = ZlibEncoder::new(out, Compression::fast());
    write_body(tree, &mut body)?;
    body.finish()?;
    Ok(())
}

fn write_body<W: Write>(tree: &SizeTree, out: &mut W) -> io::Result<()> {
    let nodes = tree.nodes_slice();
    out.write_all(&(nodes.len() as u64).to_le_bytes())?;
    // One buffer, refilled: a write call per field would be a syscall per field
    // once the deflate buffer fills, and there are ten fields and a million
    // nodes.
    let mut buf = Vec::with_capacity(RECORD * 1024);
    for node in nodes {
        buf.extend_from_slice(&node.logical.to_le_bytes());
        buf.extend_from_slice(&node.allocated.to_le_bytes());
        buf.extend_from_slice(&node.parent.to_le_bytes());
        buf.extend_from_slice(&node.first_child.to_le_bytes());
        buf.extend_from_slice(&node.child_count.to_le_bytes());
        buf.extend_from_slice(&node.subtree_files.to_le_bytes());
        buf.extend_from_slice(&node.name_off.to_le_bytes());
        buf.extend_from_slice(&node.mtime.to_le_bytes());
        buf.extend_from_slice(&node.name_len.to_le_bytes());
        buf.push(node.flags.bits());
        buf.push(node.depth);
        debug_assert_eq!(buf.len() % RECORD, 0, "a record that is not `RECORD` long");
        if buf.len() >= RECORD * 1024 {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    out.write_all(&buf)?;

    let names = tree.names_bytes();
    out.write_all(&(names.len() as u64).to_le_bytes())?;
    out.write_all(names)?;

    write_owners(tree.owners(), out)?;
    let times = tree.creation_times();
    out.write_all(&(times.len() as u64).to_le_bytes())?;
    for time in times {
        out.write_all(&time.to_le_bytes())?;
    }
    Ok(())
}

fn write_owners<W: Write>(owners: &Owners, out: &mut W) -> io::Result<()> {
    if !owners.were_collected() {
        out.write_all(&0u64.to_le_bytes())?;
        return Ok(());
    }

    let people = owners.people();
    out.write_all(&(people.len() as u64).to_le_bytes())?;
    for person in people {
        match &person.id {
            OwnerId::Sid(text) => {
                out.write_all(&[0u8])?;
                write_str(text, out)?;
            }
            OwnerId::Uid(uid) => {
                out.write_all(&[1u8])?;
                out.write_all(&uid.to_le_bytes())?;
            }
        }
        write_str(person.name.as_deref().unwrap_or(""), out)?;
        out.write_all(&[match person.kind {
            OwnerKind::Person => 0,
            OwnerKind::Group => 1,
            OwnerKind::Machine => 2,
            OwnerKind::Unknown => 3,
        }])?;
    }

    let slots = owners.slots();
    out.write_all(&(slots.len() as u64).to_le_bytes())?;
    let mut buf = Vec::with_capacity(4 * 1024);
    for slot in slots {
        buf.extend_from_slice(&slot.to_le_bytes());
        if buf.len() >= 4 * 1024 {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    out.write_all(&buf)
}

fn write_str<W: Write>(text: &str, out: &mut W) -> io::Result<()> {
    out.write_all(&(text.len() as u32).to_le_bytes())?;
    out.write_all(text.as_bytes())
}

/// Key/value pairs, each length-prefixed.
///
/// Not JSON: kosha-core writes JSON and deliberately does not read it — see
/// `compare.rs` on why a hand-written reader for it is not a thing to own. Not
/// a text format either, because a root path may contain very nearly any byte
/// on Unix, including a newline, and a header that needed escaping rules would
/// need a test suite for them.
fn encode_header(meta: &Meta) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);
    let mut text = |key: &str, value: &str| {
        buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
        buf.extend_from_slice(key.as_bytes());
        buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
        buf.extend_from_slice(value.as_bytes());
    };

    text("taken", &meta.taken.to_string());
    text("taken-ms", &meta.taken_ms.to_string());
    // Lossy on the way out is acceptable and lossy on the way in is not: the
    // root is shown to a person and compared as text, never opened from here.
    text("root", &meta.root.to_string_lossy());
    text("label", &meta.label);
    text("entries", &meta.entries.to_string());
    text("dirs", &meta.dirs.to_string());
    text("files", &meta.files.to_string());
    text("allocated", &meta.bytes_allocated.to_string());
    text("logical", &meta.bytes_logical.to_string());
    text("strategy", &meta.strategy);
    text("image", &meta.image);
    text("partial", if meta.partial { "1" } else { "0" });
    text("depth-limited", if meta.depth_limited { "1" } else { "0" });
    text("cancelled", if meta.cancelled { "1" } else { "0" });
    text("unreadable", &meta.unreadable.to_string());
    text("other-device", &meta.skipped_other_device.to_string());
    text("hardlinks", &meta.hardlinks_deduped.to_string());
    text(
        "dedup-hardlinks",
        if meta.dedup_hardlinks { "1" } else { "0" },
    );
    text("cross-fs", if meta.cross_filesystems { "1" } else { "0" });
    if let Some(exact) = meta.exact_allocated {
        text("exact", if exact { "1" } else { "0" });
    }
    text("max-depth", &meta.max_depth.to_string());
    text("took-ms", &meta.duration_ms.to_string());
    text("tool", &meta.tool);
    buf
}

/// Is this file a snapshot?
///
/// By its first eight bytes, not by its extension. A file named `.kosha` that
/// is not one should say so rather than being parsed as something else, and a
/// snapshot somebody renamed should still open.
///
/// Here rather than in either front end because both have to agree: the window
/// and the command line are handed the same paths by the same people, and one
/// of them deciding a file is a snapshot while the other tries to scan it as a
/// folder is exactly the kind of difference nobody reports as a bug.
///
/// **A regular file, checked before anything is opened.** This question is
/// asked of every path kosha is given, and the answer used to be found by
/// reading eight bytes out of whatever was there. `kosha CON` hung — no output,
/// no progress, forever — because reading a console waits for somebody to type,
/// and the debugger put the whole process in `is_snapshot` at the `read_exact`
/// below. A serial port answers the same way, and on Unix opening a FIFO blocks
/// before the read even begins: `File::open` waits for a writer that may never
/// come. None of those is a snapshot, and none of them can be asked politely.
pub fn is_snapshot(path: &Path) -> bool {
    if !std::fs::metadata(path).is_ok_and(|about| about.is_file()) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).is_ok() && &magic == MAGIC
}

// ------------------------------------------------------------------- reading

/// Read only what a snapshot says about itself.
///
/// Stops at the end of the header, so listing a directory of snapshots costs
/// one small read each rather than inflating every tree in it.
pub fn read_meta<R: Read>(mut input: R) -> Result<Meta, ReadError> {
    let (_, header) = read_header_bytes(&mut input).map_err(cut_short(BEFORE_HEADER))?;
    decode_header(&header)
}

/// A file that stops in the middle, said the way this format says everything
/// else.
///
/// **Truncation is the likeliest damage a snapshot suffers**, and it was the
/// one kind that answered in somebody else's words. Measured over every length
/// of a 612-byte snapshot: eight bytes in, a cut said `failed to fill whole
/// buffer`, which is `read_exact`'s; past the header it said `incomplete
/// deflate stream`, which is zlib's. Opening a damaged snapshot should produce
/// a sentence about the snapshot, and neither of those is one — a copy
/// that stopped, a download that died, a file recovered off a failing disk, and
/// what the user is told is the name of a buffer.
///
/// Only end-of-file becomes a sentence. A real I/O error — the disk the
/// snapshot sits on going away mid-read — is still [`ReadError::Io`], because
/// that one is not about the file's contents at all.
fn cut_short(what: &'static str) -> impl Fn(ReadError) -> ReadError {
    move |error| match error {
        ReadError::Io(io) if io.kind() == io::ErrorKind::UnexpectedEof => ReadError::Damaged(what),
        other => other,
    }
}

const BEFORE_HEADER: &str = "it ends before its header does";
const BEFORE_TREE: &str = "it ends before its tree does";

/// Read a snapshot back into a browsable tree.
pub fn read<R: Read>(mut input: R) -> Result<(Meta, SizeTree), ReadError> {
    let (format, header) = read_header_bytes(&mut input).map_err(cut_short(BEFORE_HEADER))?;
    let meta = decode_header(&header)?;
    let mut body = ZlibDecoder::new(input);
    let tree = read_body(&meta, format, &mut body).map_err(cut_short(BEFORE_TREE))?;

    // Read *past* the tree, which is what makes the checksum mean anything:
    // zlib verifies its Adler-32 when it reaches the end, and stopping as soon
    // as the tree is assembled would never get there. A snapshot cut short by a
    // full disk decodes perfectly up to the cut, so without this it would read
    // back as a complete tree that stopped early.
    //
    // One byte, not the rest of the stream. Anything behind the tree is already
    // a refusal, and `read_to_end` was refusing it only after holding all of
    // it: deflate turns a quarter of a megabyte of zeros into a quarter of a
    // gigabyte, so the file that made kosha ask the system for 256 MiB was
    // 255 KiB long. A byte is enough to tell "the end" from "more".
    let mut probe = [0u8; 1];
    match body.read(&mut probe) {
        Ok(0) => {}
        Ok(_) => return Err(ReadError::Damaged("there is more here than a tree")),
        // Two different files fail here and they deserve two different
        // sentences. A file cut *after* its last node but before the four
        // bytes of Adler-32 has a complete tree in it and no way to prove it;
        // a file whose bits rotted has a checksum that disagrees. Saying
        // "the checksum does not match" about the first one sends somebody
        // looking for corruption in a file that is merely short.
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(ReadError::Damaged("it ends before its checksum does"));
        }
        Err(_) => {
            return Err(ReadError::Damaged(
                "the checksum does not match the contents",
            ));
        }
    }

    Ok((meta, tree))
}

fn read_header_bytes<R: Read>(input: &mut R) -> Result<(u32, Vec<u8>), ReadError> {
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            ReadError::NotASnapshot
        } else {
            ReadError::Io(error)
        }
    })?;
    if &magic != MAGIC {
        return Err(ReadError::NotASnapshot);
    }

    let format = read_u32(input)?;
    if format > FORMAT {
        return Err(ReadError::Newer { found: format });
    }

    let length = read_u32(input)? as usize;
    // A header is a few hundred bytes. A damaged length field must not become
    // an allocation of whatever number happened to be there.
    if length > 64 * 1024 {
        return Err(ReadError::Damaged("the header length is not plausible"));
    }
    let mut header = vec![0u8; length];
    input.read_exact(&mut header)?;
    if format == 0 {
        return Err(ReadError::Damaged("unknown snapshot format"));
    }
    Ok((format, header))
}

fn decode_header(buf: &[u8]) -> Result<Meta, ReadError> {
    let mut meta = Meta {
        taken: 0,
        taken_ms: 0,
        root: PathBuf::new(),
        label: String::new(),
        entries: 0,
        dirs: 0,
        files: 0,
        bytes_allocated: 0,
        bytes_logical: 0,
        depth_limited: false,
        cancelled: false,
        strategy: String::new(),
        image: String::new(),
        partial: false,
        unreadable: 0,
        skipped_other_device: 0,
        hardlinks_deduped: 0,
        dedup_hardlinks: false,
        cross_filesystems: false,
        exact_allocated: None,
        max_depth: 0,
        duration_ms: 0,
        tool: String::new(),
    };

    let mut at = 0usize;
    while at < buf.len() {
        let key_len = take_u16(buf, &mut at)? as usize;
        let key = take_str(buf, &mut at, key_len)?;
        let value_len = take_u32(buf, &mut at)? as usize;
        let value = take_str(buf, &mut at, value_len)?;

        // Unknown keys are skipped rather than refused: a snapshot written by
        // a later kosha of the same format version may carry fields this build
        // has never heard of, and the ones it does know are still true.
        match key {
            "taken" => meta.taken = value.parse().unwrap_or(0),
            // Absent from anything written before this field existed, and
            // defaulting to zero there, which is why it needs no format bump.
            "taken-ms" => meta.taken_ms = value.parse().unwrap_or(0),
            "root" => meta.root = PathBuf::from(value),
            "label" => meta.label = value.to_string(),
            "entries" => meta.entries = value.parse().unwrap_or(0),
            "dirs" => meta.dirs = value.parse().unwrap_or(0),
            "files" => meta.files = value.parse().unwrap_or(0),
            "allocated" => meta.bytes_allocated = value.parse().unwrap_or(0),
            "logical" => meta.bytes_logical = value.parse().unwrap_or(0),
            "strategy" => meta.strategy = value.to_string(),
            "image" => meta.image = value.to_string(),
            "partial" => meta.partial = value == "1",
            "depth-limited" => meta.depth_limited = value == "1",
            "cancelled" => meta.cancelled = value == "1",
            "unreadable" => meta.unreadable = value.parse().unwrap_or(0),
            "other-device" => meta.skipped_other_device = value.parse().unwrap_or(0),
            "hardlinks" => meta.hardlinks_deduped = value.parse().unwrap_or(0),
            "dedup-hardlinks" => meta.dedup_hardlinks = value == "1",
            "cross-fs" => meta.cross_filesystems = value == "1",
            "exact" => meta.exact_allocated = Some(value == "1"),
            "max-depth" => meta.max_depth = value.parse().unwrap_or(0),
            "took-ms" => meta.duration_ms = value.parse().unwrap_or(0),
            "tool" => meta.tool = value.to_string(),
            _ => {}
        }
    }
    Ok(meta)
}

fn read_body<R: Read>(meta: &Meta, format: u32, input: &mut R) -> Result<SizeTree, ReadError> {
    let count = read_u64(input)? as usize;
    // The header says how many entries there were, and the body says it again.
    // They must agree: a file whose two halves disagree is damaged, and
    // trusting the second would build a tree the listing described wrongly.
    if count as u64 != meta.entries {
        return Err(ReadError::Damaged("the header and the tree disagree"));
    }

    // A `NodeId` is a u32, so a tree cannot hold more than that many entries
    // however confidently the file claims otherwise.
    if count > u32::MAX as usize {
        return Err(ReadError::Damaged("more entries than an arena can hold"));
    }
    // Capacity from a *file's* number is how a damaged byte becomes a
    // multi-exabyte allocation and a hard abort rather than an error message.
    // Reserve something sensible and let it grow as the bytes actually arrive.
    let mut nodes = Vec::with_capacity(count.min(64 * 1024));
    let mut raw = [0u8; RECORD];
    for _ in 0..count {
        input.read_exact(&mut raw)?;
        nodes.push(Node {
            logical: u64::from_le_bytes(raw[0..8].try_into().unwrap()),
            allocated: u64::from_le_bytes(raw[8..16].try_into().unwrap()),
            parent: u32::from_le_bytes(raw[16..20].try_into().unwrap()),
            first_child: u32::from_le_bytes(raw[20..24].try_into().unwrap()),
            child_count: u32::from_le_bytes(raw[24..28].try_into().unwrap()),
            subtree_files: u32::from_le_bytes(raw[28..32].try_into().unwrap()),
            name_off: u32::from_le_bytes(raw[32..36].try_into().unwrap()),
            mtime: u32::from_le_bytes(raw[36..40].try_into().unwrap()),
            name_len: u16::from_le_bytes(raw[40..42].try_into().unwrap()),
            flags: Flags::from_bits(raw[42]),
            depth: raw[43],
        });
    }

    // Same rule as the arena above, and this one is why the rule exists: a
    // truncated body leaves the decompressor handing back whatever bytes
    // happened to be there, and `vec![0; that]` aborts the process before any
    // error can be returned. `take` bounds the read and the vector grows only
    // as far as there is data.
    let names_len = read_u64(input)?;
    // And the claim itself is checked before a byte of it is read. The arena
    // holds one name per entry and no name twice — that is what the scanner
    // appends and what every image builder pushes — and a name is at most
    // `u16::MAX` bytes because that is all `name_len` can say. So a file
    // claiming more arena than its own entry count could reference is not a
    // file this program wrote. A `name_off` is also a `u32`, which puts a
    // second ceiling under the first: arena bytes at or past 4 GiB are bytes
    // nothing in the tree can address.
    //
    // Reading first and noticing afterwards is what `take` alone did, and the
    // difference is not academic: the bytes a bomb claims do arrive, deflated
    // 1000 to 1, so a 255 KiB file asking for a terabyte of names got as far as
    // 256 MiB of memory before the length check below fired.
    let reachable = (count as u64).saturating_mul(u64::from(u16::MAX));
    if names_len > reachable.min(u64::from(u32::MAX)) {
        return Err(ReadError::Damaged("a name arena larger than its tree"));
    }
    let mut names = Vec::new();
    input.take(names_len).read_to_end(&mut names)?;
    if names.len() as u64 != names_len {
        return Err(ReadError::Damaged("the name arena is short"));
    }
    let names_len = names.len();

    let owners = read_owners(count, input)?;
    let mut created = Vec::new();
    if format >= 2 {
        let length = read_u64(input)?;
        if length != 0 && length != count as u64 {
            return Err(ReadError::Damaged("creation times do not match the tree"));
        }
        for _ in 0..length {
            created.push(read_u64(input)?);
        }
    }

    // Everything a reader could act on is checked before a tree exists to act
    // on: every index in the arena is followed by the table, the treemap and
    // the query engine, and a damaged file must not become a panic three
    // screens later.
    validate(&nodes, names_len)?;

    Ok(SizeTree::from_parts(
        nodes,
        NameArena::from_bytes(names),
        owners,
        meta.root.clone(),
        stats_from(meta),
    )
    .with_creation_times(created))
}

fn read_owners<R: Read>(nodes: usize, input: &mut R) -> Result<Owners, ReadError> {
    let people_count = read_u64(input)? as usize;
    if people_count == 0 {
        return Ok(Owners::default());
    }
    if people_count > nodes + 1 {
        return Err(ReadError::Damaged("more owners than entries"));
    }

    // Bounded by the tree above, so the number is not absurd — but a tree of
    // ten million entries would still reserve half a gigabyte here for a table
    // that is a dozen people long in every snapshot anybody has taken. Same
    // rule as the arena: reserve what is plausible and grow into what arrives.
    let mut people = Vec::with_capacity(people_count.min(1024));
    for _ in 0..people_count {
        let mut tag = [0u8; 1];
        input.read_exact(&mut tag)?;
        let id = match tag[0] {
            0 => OwnerId::Sid(read_str(input)?),
            1 => OwnerId::Uid(read_u32(input)?),
            _ => return Err(ReadError::Damaged("an owner id of an unknown kind")),
        };
        let name = read_str(input)?;
        let mut kind = [0u8; 1];
        input.read_exact(&mut kind)?;
        people.push(Owner {
            id,
            name: (!name.is_empty()).then_some(name),
            kind: match kind[0] {
                0 => OwnerKind::Person,
                1 => OwnerKind::Group,
                2 => OwnerKind::Machine,
                _ => OwnerKind::Unknown,
            },
        });
    }

    let slot_count = read_u64(input)? as usize;
    if slot_count != nodes {
        return Err(ReadError::Damaged("an owner slot per entry was expected"));
    }
    let mut slots = Vec::with_capacity(slot_count);
    let mut raw = [0u8; 4];
    for _ in 0..slot_count {
        input.read_exact(&mut raw)?;
        slots.push(u32::from_le_bytes(raw));
    }

    Ok(Owners::from_parts(people, slots))
}

/// Every invariant the rest of the crate is allowed to assume.
///
/// A snapshot is a file, and a file can be truncated by a full disk, corrupted
/// by the medium, or edited by somebody curious. The arena's accessors index
/// without checking because the scanner cannot produce a bad one; a reader can.
fn validate(nodes: &[Node], names_len: usize) -> Result<(), ReadError> {
    if nodes.is_empty() {
        return Err(ReadError::Damaged("a tree with no root"));
    }
    let count = nodes.len() as u64;
    for (index, node) in nodes.iter().enumerate() {
        if u64::from(node.parent) >= count {
            return Err(ReadError::Damaged("a parent that is not an entry"));
        }
        // The root is its own parent; everything else must point upward, which
        // is what makes walking to the root terminate.
        if index > 0 && u64::from(node.parent) >= index as u64 {
            return Err(ReadError::Damaged("a parent that is not above its child"));
        }
        if node.child_count > 0 {
            let first = u64::from(node.first_child);
            let end = first + u64::from(node.child_count);
            if first <= index as u64 || end > count {
                return Err(ReadError::Damaged("children outside the arena"));
            }
        }
        let name_end = u64::from(node.name_off) + u64::from(node.name_len);
        if name_end > names_len as u64 {
            return Err(ReadError::Damaged("a name outside the name arena"));
        }
    }
    Ok(())
}

fn stats_from(meta: &Meta) -> ScanStats {
    ScanStats {
        strategy: Strategy::from_label(&meta.strategy),
        declined: None,
        image: (!meta.image.is_empty()).then(|| meta.image.clone()),
        entries: meta.entries,
        dirs: meta.dirs,
        files: meta.files,
        unreadable: meta.unreadable,
        skipped_other_device: meta.skipped_other_device,
        hardlinks_deduped: meta.hardlinks_deduped,
        dedup_hardlinks: meta.dedup_hardlinks,
        cross_filesystems: meta.cross_filesystems,
        // A record that did not say is taken as having estimated, which is
        // what the default does; the comparison asks `Meta` rather than this,
        // so "did not say" stays distinguishable where it matters.
        exact_allocated: meta.exact_allocated.unwrap_or(false),
        bytes_logical: meta.bytes_logical,
        bytes_allocated: meta.bytes_allocated,
        max_depth: meta.max_depth,
        duration: Duration::from_millis(meta.duration_ms),
        partial: meta.partial,
        depth_limited: meta.depth_limited,
        cancelled: meta.cancelled,
    }
}

fn read_u32<R: Read>(input: &mut R) -> Result<u32, ReadError> {
    let mut raw = [0u8; 4];
    input.read_exact(&mut raw)?;
    Ok(u32::from_le_bytes(raw))
}

fn read_u64<R: Read>(input: &mut R) -> Result<u64, ReadError> {
    let mut raw = [0u8; 8];
    input.read_exact(&mut raw)?;
    Ok(u64::from_le_bytes(raw))
}

fn read_str<R: Read>(input: &mut R) -> Result<String, ReadError> {
    let len = read_u32(input)? as usize;
    if len > 4096 {
        return Err(ReadError::Damaged("an implausibly long name"));
    }
    let mut raw = vec![0u8; len];
    input.read_exact(&mut raw)?;
    String::from_utf8(raw).map_err(|_| ReadError::Damaged("a name that is not UTF-8"))
}

fn take_u16(buf: &[u8], at: &mut usize) -> Result<u16, ReadError> {
    let end = *at + 2;
    let slice = buf
        .get(*at..end)
        .ok_or(ReadError::Damaged("the header ends mid-field"))?;
    *at = end;
    Ok(u16::from_le_bytes(slice.try_into().unwrap()))
}

fn take_u32(buf: &[u8], at: &mut usize) -> Result<u32, ReadError> {
    let end = *at + 4;
    let slice = buf
        .get(*at..end)
        .ok_or(ReadError::Damaged("the header ends mid-field"))?;
    *at = end;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn take_str<'a>(buf: &'a [u8], at: &mut usize, len: usize) -> Result<&'a str, ReadError> {
    let end = at
        .checked_add(len)
        .ok_or(ReadError::Damaged("the header ends mid-field"))?;
    let slice = buf
        .get(*at..end)
        .ok_or(ReadError::Damaged("the header ends mid-field"))?;
    *at = end;
    std::str::from_utf8(slice).map_err(|_| ReadError::Damaged("a header field that is not UTF-8"))
}

/// A filename for a snapshot of `root` taken at `taken`.
///
/// Sortable by name, because that is the order a directory listing comes back
/// in and the order the window wants: newest last, always, without reading a
/// single header to find out.
///
/// **Milliseconds are part of it**, and they are not decoration. This was
/// seconds alone, and two snapshots of one folder taken in the same second
/// produced the same filename: the second one replaced the first, no error, no
/// warning, and a listing that still said one snapshot. A person clicking
/// "Keep a snapshot" twice — which is what somebody does when they are not sure
/// the first click registered — lost the earlier one. Nothing about that is
/// visible at the moment it happens.
pub fn file_name(root: &Path, taken: SystemTime) -> String {
    file_name_nth(root, taken, 0)
}

/// The same name, distinguished by a number, for when that name is taken.
///
/// Two snapshots inside one millisecond is a script rather than a person, but
/// "a script" is precisely the case where nobody is watching, and the failure
/// this exists to prevent is a silent one. The number goes last so the name
/// still sorts by the moment it was taken.
pub(crate) fn file_name_nth(root: &Path, taken: SystemTime, nth: u32) -> String {
    let since = taken.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = since.as_secs();
    let millis = since.subsec_millis();
    let slug = slug(root);
    match nth {
        0 => format!("{secs:012}-{millis:03}-{slug}.{EXTENSION}"),
        n => format!("{secs:012}-{millis:03}-{slug}-{n}.{EXTENSION}"),
    }
}

/// A short, filesystem-safe stand-in for a path.
///
/// Only for the filename, never for identity: two different roots can slug the
/// same and the header is what says which is which.
///
/// Built from the **end** of the path. The first version took the beginning and
/// truncated, which for anything nested gave every snapshot on the machine the
/// same name: `C:\Users\me\AppData\Local\Temp\project` and
/// `C:\Users\me\Documents\taxes` both came out `c-users-me-appdata`. The
/// last component is the one that identifies a folder to the person who
/// scanned it.
fn slug(root: &Path) -> String {
    const LIMIT: usize = 28;

    // Components from the end, taking as many as fit. A name alone is usually
    // enough; two says which `src` this is.
    let mut parts: Vec<String> = Vec::new();
    let mut budget = LIMIT;
    for component in root.components().rev() {
        let word = safe(&component.as_os_str().to_string_lossy());
        if word.is_empty() {
            continue;
        }
        if word.len() + 1 > budget {
            break;
        }
        budget -= word.len() + 1;
        parts.push(word);
        if parts.len() == 2 {
            break;
        }
    }

    parts.reverse();
    let joined = parts.join("-");
    if joined.is_empty() {
        // A root with no named component at all: `/`, or a bare `C:\`.
        "root".to_string()
    } else {
        joined
    }
}

/// One path component, reduced to something every filesystem accepts.
fn safe(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for ch in word.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

pub mod store;

#[cfg(test)]
mod tests;
