// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! A disc built byte by byte, and walked by exactly the code that walks a real
//! one.
//!
//! **Three discs, because there are three ways a name can be spelt** and the
//! choice between them is made once, at the top, from evidence on the disc.
//! Building one of each is the only way to know the choice is being made
//! rather than assumed — and the Rock Ridge one hides a name in a continuation
//! area, which is where a long one really ends up and is a read this reader
//! would otherwise never take.
//!
//! The three agree on every size and on everything but the names, which is the
//! whole point of them: a disc's contents cannot depend on which dialect its
//! author chose to write the names in.

use std::path::Path;

use super::format::{self, Naming};
use super::reader::Filesystem;
use crate::scan::{Progress, ScanOptions};

const BLOCK: usize = 2048;
/// How many blocks the fixtures claim, and how long they are.
const BLOCKS: u32 = 64;

/// 2026-09-23 20:53:21 UTC, the instant on the disc this was written against.
const WHEN: u32 = 1_790_196_801;

/// The extent `mkisofs` writes for a file of no bytes.
///
/// **Not a block on the disc, and not zero either**, which is what the standard
/// asks for. A file with no data has nowhere to be, so the program that built a
/// very large share of the world's discs writes a number that is obviously not
/// a place — and a reader that checks every extent against the size of the disc
/// finds it, correctly, and then has to decide what it means. It means nothing:
/// there are no bytes to locate.
const NOWHERE: u32 = 0xFFFF_FFF0;

/// A both-endian field: the same number little-endian and then big-endian, so
/// a machine of either kind could read the disc without swapping bytes.
fn both16(image: &mut [u8], at: usize, value: u16) {
    image[at..at + 2].copy_from_slice(&value.to_le_bytes());
    image[at + 2..at + 4].copy_from_slice(&value.to_be_bytes());
}

fn both32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
    image[at + 4..at + 8].copy_from_slice(&value.to_be_bytes());
}

/// The seven bytes a record spends on a date: a year since 1900, a month, a
/// day, an hour, a minute, a second, and an offset from UTC in fifteen-minute
/// steps.
const DATE: [u8; 7] = [126, 9, 23, 20, 53, 21, 0];

/// Which dialect the fixture is written in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dialect {
    /// Neither extension: uppercase names with a version suffix, which is all
    /// the published standard allows.
    Published,
    /// Rock Ridge entries in the primary tree.
    RockRidge,
    /// A second tree of its own, in UTF-16.
    Joliet,
}

/// What the tree holds, in one place, so the three fixtures cannot drift.
///
/// A name in each of the two spellings, a size, and whether it is a directory.
const TREE: &[(&str, &str, u32, bool)] = &[
    ("A_LONG_F.BIN", "A long file name.bin", 100_000, false),
    ("README.TXT", "README.TXT", 5_000, false),
    ("EMPTY.BIN", "EMPTY.BIN", 0, false),
    ("DOCS", "docs", BLOCK as u32, true),
    ("NOTE.MD", "note.md", 10, false),
    ("DEEPER", "deeper", BLOCK as u32, true),
    ("GR__E___.TXT", "Grüße 日本.txt", 10, false),
];

const LONG_FILE: usize = 0;
const README: usize = 1;
const EMPTY: usize = 2;
const DOCS: usize = 3;
const NOTE: usize = 4;
const DEEPER: usize = 5;
const UNICODE: usize = 6;

/// One System Use entry: two letters, a length, a version, and data.
fn entry(signature: &[u8; 2], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(signature);
    out.push((4 + data.len()) as u8);
    out.push(1);
    out.extend_from_slice(data);
    out
}

/// The `SP` entry, which says the bytes after a name are Rock Ridge and not
/// somebody else's. It lives in the `.` record of the root and nowhere else.
fn sharing() -> Vec<u8> {
    entry(b"SP", &[0xBE, 0xEF, 0])
}

/// An `NM` entry: a flags byte and then a piece of the name.
fn long_name(name: &str, continues: bool) -> Vec<u8> {
    let mut data = vec![u8::from(continues)];
    data.extend_from_slice(name.as_bytes());
    entry(b"NM", &data)
}

/// A `PX` entry: the POSIX attributes, of which this reader takes the mode.
///
/// Thirty-two bytes of data — mode, link count, uid and gid, each both-endian —
/// which is the shape without the optional inode number, and the shape
/// `xorrisofs` writes.
fn posix(mode: u32, links: u32) -> Vec<u8> {
    let mut data = vec![0u8; 32];
    both32(&mut data, 0, mode);
    both32(&mut data, 8, links);
    entry(b"PX", &data)
}

/// `S_IFLNK | 0777`, which is what a symbolic link's mode is on every disc
/// anybody has, and `S_IFREG | 0444`, which is what a file's is on a read-only
/// medium.
const MODE_SYMLINK: u32 = 0xA1FF;
const MODE_FILE: u32 = 0x81A4;

/// A `CE` entry: the rest of the System Use area, somewhere else on the disc.
fn continuation(block: u32, offset: u32, length: u32) -> Vec<u8> {
    let mut data = vec![0u8; 24];
    both32(&mut data, 0, block);
    both32(&mut data, 8, offset);
    both32(&mut data, 16, length);
    entry(b"CE", &data)
}

/// One directory record.
fn record(extent: u32, size: u32, flags: u8, name: &[u8], system_use: &[u8]) -> Vec<u8> {
    // A pad byte after a name of even length, so that every record starts on
    // an even offset.
    let pad = usize::from(name.len().is_multiple_of(2));
    let mut length = format::RECORD_HEADER + name.len() + pad + system_use.len();
    // And the record itself is even, which is what keeps the next one aligned.
    length += length % 2;

    let mut out = vec![0u8; length];
    out[0] = u8::try_from(length).expect("a record fits in a byte");
    both32(&mut out, 2, extent);
    both32(&mut out, 10, size);
    out[18..25].copy_from_slice(&DATE);
    out[25] = flags;
    both16(&mut out, 28, 1);
    out[32] = name.len() as u8;
    let at = format::RECORD_HEADER;
    out[at..at + name.len()].copy_from_slice(name);
    let at = at + name.len() + pad;
    out[at..at + system_use.len()].copy_from_slice(system_use);
    out
}

const DIRECTORY: u8 = 0x02;
const MULTI_EXTENT: u8 = 0x80;

/// `.` and `..`, which every directory records as its first two entries.
fn dots(here: u32, parent: u32, system_use: &[u8]) -> Vec<u8> {
    let mut out = record(here, BLOCK as u32, DIRECTORY, &[0x00], system_use);
    out.extend_from_slice(&record(parent, BLOCK as u32, DIRECTORY, &[0x01], &[]));
    out
}

/// A name as a Joliet tree spells it: UTF-16, big-endian, with the version
/// suffix a file still carries.
fn joliet_name(name: &str, directory: bool) -> Vec<u8> {
    let mut units: Vec<u16> = name.encode_utf16().collect();
    if !directory {
        units.extend(";1".encode_utf16());
    }
    units.iter().flat_map(|unit| unit.to_be_bytes()).collect()
}

/// A name as the published standard spells it.
fn published_name(name: &str, directory: bool) -> Vec<u8> {
    let mut out = name.as_bytes().to_vec();
    if !directory {
        out.extend_from_slice(b";1");
    }
    out
}

pub(crate) struct Builder {
    image: Vec<u8>,
    dialect: Dialect,
    next: u32,
}

impl Builder {
    fn new(dialect: Dialect) -> Builder {
        Builder {
            image: vec![0u8; BLOCKS as usize * BLOCK],
            dialect,
            // The first sixteen blocks belong to whoever wrote the disc.
            next: 16,
        }
    }

    fn claim(&mut self) -> u32 {
        let block = self.next;
        self.next += 1;
        block
    }

    fn at(&mut self, block: u32) -> &mut [u8] {
        let start = block as usize * BLOCK;
        &mut self.image[start..start + BLOCK]
    }

    fn write(&mut self, block: u32, bytes: &[u8]) {
        assert!(bytes.len() <= BLOCK, "a directory of more than one block");
        self.at(block)[..bytes.len()].copy_from_slice(bytes);
    }

    /// A primary or supplementary volume descriptor.
    fn descriptor(&mut self, block: u32, kind: u8, root_extent: u32, joliet: bool) {
        let blocks = BLOCKS;
        let sector = self.at(block);
        sector[0] = kind;
        sector[1..6].copy_from_slice(b"CD001");
        sector[6] = 1;
        both32(sector, 80, blocks);
        if joliet {
            // The escape sequence that says the names in this tree are UTF-16.
            sector[88..91].copy_from_slice(b"%/E");
        }
        both16(sector, 120, 1); // volume set size
        both16(sector, 124, 1); // volume sequence number
        both16(sector, 128, BLOCK as u16);
        let root = record(root_extent, BLOCK as u32, DIRECTORY, &[0x00], &[]);
        sector[156..156 + root.len()].copy_from_slice(&root);
    }

    fn terminator(&mut self, block: u32) {
        let sector = self.at(block);
        sector[0] = 255;
        sector[1..6].copy_from_slice(b"CD001");
        sector[6] = 1;
    }

    /// The name the *primary* tree carries, which on every real disc is the
    /// published one whatever else the disc also has.
    ///
    /// **This was wrong once and a test caught it.** The Joliet fixture used to
    /// write UTF-16 names into the primary tree as well, which no disc does —
    /// and the only test that could notice was the one that damages the
    /// supplementary descriptor and expects the primary tree to still be
    /// readable. Everything else read the Joliet tree and never looked.
    fn name_of(&self, which: usize) -> Vec<u8> {
        let (published, _, _, directory) = TREE[which];
        published_name(published, directory)
    }

    /// The name the Joliet tree carries for the same file.
    fn joliet_name_of(&self, which: usize) -> Vec<u8> {
        let (_, real, _, directory) = TREE[which];
        joliet_name(real, directory)
    }

    /// The System Use area a record carries, which is empty unless the disc is
    /// a Rock Ridge one.
    fn system_use(&self, which: usize) -> Vec<u8> {
        if self.dialect != Dialect::RockRidge {
            return Vec::new();
        }
        long_name(TREE[which].1, false)
    }
}

/// Which block each structure of a fixture landed in.
///
/// **Returned rather than written down**, because the three dialects need
/// different numbers of descriptors and so land their directories in different
/// places — and a test that damages "the root, which is block 19" is a test
/// that silently damages a descriptor on two of the three discs and proves
/// nothing on either.
pub(crate) struct Layout {
    pub primary: u32,
    pub supplementary: Option<u32>,
    pub terminator: u32,
    pub root: u32,
    pub docs: u32,
    pub deeper: u32,
    pub spill: u32,
}

pub(crate) fn fixture(dialect: Dialect) -> Vec<u8> {
    built(dialect).0
}

pub(crate) fn built(dialect: Dialect) -> (Vec<u8>, Layout) {
    let mut builder = Builder::new(dialect);

    let primary = builder.claim();
    let supplementary = (dialect == Dialect::Joliet).then(|| builder.claim());
    let terminator = builder.claim();

    let root = builder.claim();
    let docs = builder.claim();
    let deeper = builder.claim();
    // Where the long name of the last file lives on the Rock Ridge disc: not
    // in its record, but in an area of its own that has to be read.
    let spill = builder.claim();

    let joliet_root = supplementary.map(|_| builder.claim());
    let joliet_docs = joliet_root.map(|_| builder.claim());
    let joliet_deeper = joliet_root.map(|_| builder.claim());

    // Blocks the files would occupy. Nothing here ever reads one — a file's
    // length and its first block are both in the record that names it — but a
    // record pointing at a block the disc does not have is refused, so they
    // have to be blocks the disc has.
    //
    // **One each, which was not always so.** Every file used to point at a
    // single shared block, on the reasoning that nothing reads them. That made
    // the fixture a disc on which every file is the same run of blocks — which
    // is how a mastering tool records hardlinks, and it hid the reader counting
    // such a disc several times over. A fixture in a shape no disc has cannot
    // catch a bug about that shape.
    let blocks: Vec<u32> = TREE.iter().map(|_| builder.claim()).collect();
    let data = |which: usize| blocks[which];

    builder.descriptor(primary, 1, root, false);
    if let Some(block) = supplementary {
        builder.descriptor(block, 2, joliet_root.expect("a Joliet root"), true);
    }
    builder.terminator(terminator);

    // --- the primary tree

    let rock_ridge = dialect == Dialect::RockRidge;
    let mut records = dots(root, root, &if rock_ridge { sharing() } else { Vec::new() });
    for which in [LONG_FILE, README, EMPTY] {
        let (_, _, size, _) = TREE[which];
        records.extend_from_slice(&record(
            if size == 0 { NOWHERE } else { data(which) },
            size,
            0,
            &builder.name_of(which),
            &builder.system_use(which),
        ));
    }
    records.extend_from_slice(&record(
        docs,
        BLOCK as u32,
        DIRECTORY,
        &builder.name_of(DOCS),
        &builder.system_use(DOCS),
    ));
    builder.write(root, &records);

    let mut records = dots(docs, root, &[]);
    records.extend_from_slice(&record(
        data(NOTE),
        TREE[NOTE].2,
        0,
        &builder.name_of(NOTE),
        &builder.system_use(NOTE),
    ));
    records.extend_from_slice(&record(
        deeper,
        BLOCK as u32,
        DIRECTORY,
        &builder.name_of(DEEPER),
        &builder.system_use(DEEPER),
    ));
    builder.write(docs, &records);

    // **The one record whose name is not in it.** On a Rock Ridge disc its
    // System Use area holds only a `CE`, so the name is found by spending a
    // read — which is what a long name on a real disc costs, and what nothing
    // else here would exercise.
    let spilled = long_name(TREE[UNICODE].1, false);
    let system_use = if rock_ridge {
        continuation(spill, 0, spilled.len() as u32)
    } else {
        Vec::new()
    };
    let records = {
        let mut records = dots(deeper, docs, &[]);
        records.extend_from_slice(&record(
            data(UNICODE),
            TREE[UNICODE].2,
            0,
            &builder.name_of(UNICODE),
            &system_use,
        ));
        records
    };
    builder.write(deeper, &records);
    if rock_ridge {
        builder.write(spill, &spilled);
    }

    // --- the Joliet tree, which is a second tree over the same files

    if let (Some(root), Some(docs), Some(deeper)) = (joliet_root, joliet_docs, joliet_deeper) {
        let mut records = dots(root, root, &[]);
        for which in [LONG_FILE, README, EMPTY] {
            let size = TREE[which].2;
            records.extend_from_slice(&record(
                if size == 0 { NOWHERE } else { data(which) },
                size,
                0,
                &builder.joliet_name_of(which),
                &[],
            ));
        }
        records.extend_from_slice(&record(
            docs,
            BLOCK as u32,
            DIRECTORY,
            &builder.joliet_name_of(DOCS),
            &[],
        ));
        builder.write(root, &records);

        let mut records = dots(docs, root, &[]);
        records.extend_from_slice(&record(
            data(NOTE),
            TREE[NOTE].2,
            0,
            &builder.joliet_name_of(NOTE),
            &[],
        ));
        records.extend_from_slice(&record(
            deeper,
            BLOCK as u32,
            DIRECTORY,
            &builder.joliet_name_of(DEEPER),
            &[],
        ));
        builder.write(docs, &records);

        let mut records = dots(deeper, docs, &[]);
        records.extend_from_slice(&record(
            data(UNICODE),
            TREE[UNICODE].2,
            0,
            &builder.joliet_name_of(UNICODE),
            &[],
        ));
        builder.write(deeper, &records);
    }

    (
        builder.image,
        Layout {
            primary,
            supplementary,
            terminator,
            root,
            docs,
            deeper,
            spill,
        },
    )
}

// -------------------------------------------------------------------- helpers

fn tree_of(image: &[u8], options: &ScanOptions) -> crate::SizeTree {
    let mut fs = Filesystem::open(image).expect("the builder writes a readable disc");
    let start = super::build::root(&fs);
    super::build::build(
        &mut fs,
        Path::new("/image"),
        start,
        options,
        |_: Progress| {},
        || false,
    )
    .expect("a built disc is walkable")
}

fn shape(tree: &crate::SizeTree) -> Vec<String> {
    let mut rows: Vec<String> = (0..tree.len())
        .map(crate::NodeId::from_index)
        .filter(|&id| id != tree.root())
        .map(|id| {
            let node = tree.node(id);
            format!(
                "{} {} {} {}",
                tree.name_lossy(id),
                node.logical,
                node.allocated,
                if node.is_dir() { "dir" } else { "file" }
            )
        })
        .collect();
    rows.sort();
    rows
}

/// What the tree holds when the names are the real ones, which both extensions
/// produce and the published standard does not.
fn expected_real_names() -> Vec<String> {
    vec![
        // 100,000 bytes is forty-nine blocks of 2048, and the last of them is
        // mostly empty — which is the whole reason anybody scans a disc.
        "A long file name.bin 100000 100352 file".to_string(),
        "EMPTY.BIN 0 0 file".to_string(),
        "Grüße 日本.txt 10 2048 file".to_string(),
        "README.TXT 5000 6144 file".to_string(),
        // Its own block of records, plus everything under it.
        "deeper 10 4096 dir".to_string(),
        "docs 20 8192 dir".to_string(),
        "note.md 10 2048 file".to_string(),
    ]
}

// ------------------------------------------------------------------ the format

#[test]
fn a_descriptor_that_is_not_one_is_refused() {
    assert_eq!(format::descriptor(&[0u8; 2048]), None);
    assert_eq!(format::descriptor(&[0u8; 8]), None);
    assert_eq!(
        format::volume(&[0u8; 2048]),
        Err(format::Unreadable::NotIso)
    );
}

/// The standard has one version and a disc claiming another was written to a
/// document that does not exist.
#[test]
fn a_descriptor_of_an_unpublished_version_is_refused() {
    let mut image = fixture(Dialect::Published);
    image[16 * BLOCK + 6] = 2;
    assert!(Filesystem::open(image.as_slice()).is_err());
}

#[test]
fn the_seven_bytes_of_a_date_are_the_instant_they_name() {
    assert_eq!(format::timestamp(&DATE), WHEN);
    // The same instant written by a machine two hours east of UTC, which
    // records the same moment and a different wall clock.
    let mut shifted = DATE;
    shifted[3] = 22;
    shifted[6] = 8;
    assert_eq!(format::timestamp(&shifted), WHEN);
    // A field never written is no date rather than the first of January 1900.
    assert_eq!(format::timestamp(&[0u8; 7]), 0);
    // A date no calendar has.
    assert_eq!(format::timestamp(&[126, 13, 1, 0, 0, 0, 0]), 0);
    assert_eq!(format::timestamp(&[126, 2, 30, 0, 0, 0, 0]), 0);
}

/// Two of them, and neither is part of the name: the `;1` is a version number
/// the standard requires, and a trailing dot is what a file with no extension
/// is recorded as.
#[test]
fn a_published_name_loses_its_version_and_its_empty_extension() {
    for (recorded, wanted) in [
        (&b"README.TXT;1"[..], "README.TXT"),
        (b"README.;1", "README"),
        (b"MAKEFILE.", "MAKEFILE"),
        (b"NOVERSION", "NOVERSION"),
    ] {
        let mut out = Vec::new();
        format::push_published_name(recorded, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), wanted);
    }
}

/// Big-endian, which is the one place in this reader where a number is not
/// little-endian — and the one that turns an English filename into Chinese
/// characters when it is forgotten.
#[test]
fn a_joliet_name_is_utf16_the_other_way_round() {
    let mut out = Vec::new();
    format::push_joliet_name(&joliet_name("Grüße 日本.txt", false), &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), "Grüße 日本.txt");

    // The same bytes read the wrong way round, to show the difference is not
    // subtle.
    let swapped: Vec<u8> = joliet_name("hello", false)
        .as_chunks::<2>()
        .0
        .iter()
        .flat_map(|pair| [pair[1], pair[0]])
        .collect();
    let mut out = Vec::new();
    format::push_joliet_name(&swapped, &mut out);
    assert_ne!(String::from_utf8(out).unwrap(), "hello");
}

// -------------------------------------------------------------------- the walk

/// A file of no bytes is not an unreadable one, whatever its record says about
/// where its bytes are.
///
/// **Found on a real disc.** `mkisofs` writes 0xFFFFFFF0 as the extent of an
/// empty file, so every disc it has ever built — which is a great many of them —
/// reported one unreadable entry and a partial tree, and every total came out
/// with a `~` in front of it. The check was right that the block does not exist
/// and wrong to care: a file with no bytes has nothing to locate.
///
/// The fixtures all write that extent now, so the three walks above assert this
/// as well; this says it out loud, because a number that means nothing is
/// exactly the sort of thing a later change would start checking again.
#[test]
fn a_file_of_no_bytes_is_not_unreadable_wherever_its_record_points() {
    for dialect in [Dialect::Published, Dialect::RockRidge, Dialect::Joliet] {
        let tree = tree_of(&fixture(dialect), &ScanOptions::default());
        assert!(
            !tree.stats().partial,
            "{dialect:?} called a disc partial over an empty file"
        );
        assert_eq!(tree.stats().unreadable, 0, "{dialect:?}");
        let empty = (0..tree.len())
            .map(crate::NodeId::from_index)
            .find(|&id| tree.name_lossy(id) == "EMPTY.BIN")
            .expect("the empty file");
        assert!(!tree.node(empty).flags.contains(crate::Flags::UNREADABLE));
    }
}

/// A record's own flags say "directory or not" and nothing else, so the only
/// thing on a disc that says an entry is a symbolic link is a Rock Ridge `PX`
/// mode. Until it was read, a link on a disc was a file of no bytes — while the
/// ext4, XFS and UDF readers all said `symlink` about the same tree.
///
/// A link is a link *first*, which is why the one here has the directory bit set
/// as well: a link to a directory must be measured as itself and must not be
/// descended into, whatever the record claims.
#[test]
fn a_rock_ridge_link_is_a_link_and_not_a_file_of_no_bytes() {
    let mut builder = Builder::new(Dialect::RockRidge);
    let primary = builder.claim();
    let terminator = builder.claim();
    let root = builder.claim();
    let elsewhere = builder.claim();

    builder.descriptor(primary, 1, root, false);
    builder.terminator(terminator);

    // A directory somewhere else on the disc, so that descending into the link
    // would produce children rather than nothing and the test could tell.
    builder.write(elsewhere, &dots(elsewhere, root, &[]));

    let mut records = dots(root, root, &sharing());
    let mut system_use = long_name("link-to-elsewhere", false);
    system_use.extend_from_slice(&posix(MODE_SYMLINK, 1));
    records.extend_from_slice(&record(
        elsewhere,
        0,
        DIRECTORY,
        &published_name("LINK", true),
        &system_use,
    ));
    let mut system_use = long_name("plain.txt", false);
    system_use.extend_from_slice(&posix(MODE_FILE, 1));
    records.extend_from_slice(&record(
        elsewhere,
        10,
        0,
        &published_name("PLAIN.TXT", false),
        &system_use,
    ));
    builder.write(root, &records);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    let link = (0..tree.len())
        .map(crate::NodeId::from_index)
        .find(|&id| tree.name_lossy(id) == "link-to-elsewhere")
        .expect("the link");
    assert!(tree.node(link).is_symlink(), "a link read as a plain entry");
    assert!(!tree.node(link).is_dir(), "a link is a link first");
    assert_eq!(tree.node(link).child_count, 0, "a link was descended into");

    // And a `PX` that says an ordinary file says nothing else.
    let plain = (0..tree.len())
        .map(crate::NodeId::from_index)
        .find(|&id| tree.name_lossy(id) == "plain.txt")
        .expect("the file");
    assert!(!tree.node(plain).is_symlink());
    assert_eq!(tree.node(plain).logical, 10);
}

/// A `PX` entry is the mode and then the fields this reader does not take.
#[test]
fn a_posix_entry_is_read_as_its_mode_and_nothing_else() {
    let bytes = posix(MODE_SYMLINK, 3);
    let mut seen = None;
    format::system_use(&bytes, |entry| {
        seen = format::posix_mode(&entry);
        true
    });
    assert_eq!(seen, Some(MODE_SYMLINK));
    assert!(format::mode_is_symlink(MODE_SYMLINK));
    assert!(!format::mode_is_symlink(MODE_FILE));
    // A directory is not a link either, and the two differ in one bit of the
    // four that say what a thing is.
    assert!(!format::mode_is_symlink(0x41ED));

    // Anything that is not a `PX` is not a mode, and a truncated one is not
    // guessed at.
    let mut seen = None;
    format::system_use(&long_name("x", false), |entry| {
        seen = format::posix_mode(&entry);
        true
    });
    assert_eq!(seen, None);
    let short = entry(b"PX", &[0u8; 4]);
    let mut seen = None;
    format::system_use(&short, |entry| {
        seen = format::posix_mode(&entry);
        true
    });
    assert_eq!(seen, None);
}

/// Several names pointing at one extent are one run of blocks, and counting
/// them per name reports a disc as bigger than the medium it came from.
///
/// **This is what a mastering tool does with a source tree that had hardlinks**,
/// and it is not a corner: `xorrisofs` pools them with Rock Ridge and without
/// it, and writes a `PX` link count of *1* for every one of them — measured, on a
/// disc with three names for one file. So the extent is the only thing on the
/// disc that says the bytes are there once, and before this a four-name disc of
/// 392 KiB of data read as 786 KiB, out of an image file 758 KiB long.
#[test]
fn several_names_for_one_extent_are_one_run_of_blocks() {
    let mut builder = Builder::new(Dialect::Published);
    let primary = builder.claim();
    let terminator = builder.claim();
    let root = builder.claim();
    let shared = builder.claim();

    builder.descriptor(primary, 1, root, false);
    builder.terminator(terminator);

    let mut records = dots(root, root, &[]);
    for name in ["A.BIN", "LINK1.BIN", "LINK2.BIN"] {
        records.extend_from_slice(&record(
            shared,
            BLOCK as u32,
            0,
            &published_name(name, false),
            &[],
        ));
    }
    builder.write(root, &records);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    assert_eq!(tree.stats().files, 3, "all three names stay in the tree");
    assert_eq!(tree.stats().hardlinks_deduped, 2);
    assert_eq!(
        shape(&tree),
        vec![
            "A.BIN 2048 2048 file".to_string(),
            "LINK1.BIN 0 0 file".to_string(),
            "LINK2.BIN 0 0 file".to_string(),
        ],
        "the first name reached carries the blocks and the others carry nothing"
    );

    // And asked for `du -l`, every name carries them — which is what somebody
    // asking what the disc would cost to unpack wants.
    let separately = tree_of(
        &builder.image,
        &ScanOptions {
            dedup_hardlinks: Some(false),
            ..ScanOptions::default()
        },
    );
    assert_eq!(separately.stats().hardlinks_deduped, 0);
    // Three names' worth, and the difference between the two totals is the
    // blocks the disc holds once: the root's own block is in both.
    assert_eq!(
        separately.total_allocated() - tree.total_allocated(),
        BLOCK as u64 * 2
    );
}

/// All three dialects, one expectation for the sizes. A disc's contents cannot
/// depend on which way its author chose to spell the names.
#[test]
fn every_dialect_walks_into_the_same_tree() {
    for dialect in [Dialect::RockRidge, Dialect::Joliet] {
        let tree = tree_of(&fixture(dialect), &ScanOptions::default());
        assert_eq!(
            shape(&tree),
            expected_real_names(),
            "{dialect:?} walked into a different tree"
        );
        assert!(!tree.stats().partial, "{dialect:?} reported a partial tree");
    }

    // The published names are the same tree with the same sizes and different
    // spelling, which is what the standard leaves somebody with.
    let tree = tree_of(&fixture(Dialect::Published), &ScanOptions::default());
    assert_eq!(
        shape(&tree),
        vec![
            "A_LONG_F.BIN 100000 100352 file".to_string(),
            "DEEPER 10 4096 dir".to_string(),
            "DOCS 20 8192 dir".to_string(),
            "EMPTY.BIN 0 0 file".to_string(),
            "GR__E___.TXT 10 2048 file".to_string(),
            "NOTE.MD 10 2048 file".to_string(),
            "README.TXT 5000 6144 file".to_string(),
        ]
    );
}

/// The choice is made from evidence on the disc, and it is made once. A disc
/// with both extensions must take Rock Ridge, which is the tree its author
/// meant for a machine that understands files.
#[test]
fn which_tree_is_walked_is_decided_by_what_the_disc_carries() {
    for (dialect, wanted) in [
        (Dialect::Published, Naming::Published),
        (Dialect::RockRidge, Naming::RockRidge),
        (Dialect::Joliet, Naming::Joliet),
    ] {
        let image = fixture(dialect);
        let fs = Filesystem::open(image.as_slice()).unwrap();
        assert_eq!(fs.naming(), wanted, "{dialect:?}");
    }
}

/// Without the `SP` entry the bytes after a name belong to whoever wrote the
/// disc, and reading them as Rock Ridge would invent names out of somebody
/// else's data. Removing it has to fall back rather than guess.
#[test]
fn rock_ridge_without_its_announcement_is_not_assumed() {
    let (mut image, layout) = built(Dialect::RockRidge);
    // The `SP` entry is in the `.` record of the root, which is the first
    // record of the root's block.
    let root = layout.root as usize * BLOCK;
    let at = image[root..]
        .windows(2)
        .position(|pair| pair == b"SP")
        .expect("the fixture announces sharing")
        + root;
    image[at] = b'X';

    let fs = Filesystem::open(image.as_slice()).unwrap();
    assert_eq!(fs.naming(), Naming::Published);
    let tree = tree_of(&image, &ScanOptions::default());
    assert!(
        shape(&tree)
            .iter()
            .any(|row| row.starts_with("A_LONG_F.BIN ")),
        "the published names are what is left"
    );
}

/// A record may not exceed 255 bytes, so a long name and a full set of POSIX
/// attributes do not fit together and the name goes somewhere else. Following
/// it is the one read this reader takes that is not a directory.
#[test]
fn a_name_that_did_not_fit_is_followed_to_where_it_went() {
    let tree = tree_of(&fixture(Dialect::RockRidge), &ScanOptions::default());
    assert!(
        shape(&tree)
            .iter()
            .any(|row| row.starts_with("Grüße 日本.txt ")),
        "the name in the continuation area was not read"
    );
}

/// And where the area cannot be read, the record still has a name — the one
/// the standard put in it. An entry reported without a name would be one
/// nobody could act on.
#[test]
fn a_continuation_that_cannot_be_read_falls_back_to_the_published_name() {
    let (mut image, layout) = built(Dialect::RockRidge);
    // Point the continuation at a block past the end of the disc.
    let deeper = layout.deeper as usize * BLOCK;
    let at = image[deeper..]
        .windows(2)
        .position(|pair| pair == b"CE")
        .expect("the fixture spills a name")
        + deeper;
    both32(&mut image, at + 4, BLOCKS + 10);

    let tree = tree_of(&image, &ScanOptions::default());
    let names = shape(&tree);
    assert!(
        names.iter().any(|row| row.starts_with("GR__E___.TXT ")),
        "the name the standard put in the record is still there: {names:?}"
    );
}

/// How ISO 9660 stores a file larger than four gigabytes: several records in a
/// row with the same name, each but the last flagged to continue, and the
/// lengths adding up.
#[test]
fn a_file_in_several_extents_is_one_file_of_their_total() {
    let mut builder = Builder::new(Dialect::Published);
    let primary = builder.claim();
    let terminator = builder.claim();
    let root = builder.claim();
    let data = builder.claim();

    builder.descriptor(primary, 1, root, false);
    builder.terminator(terminator);

    let mut records = dots(root, root, &[]);
    let name = published_name("HUGE.BIN", false);
    // Two extents of four kilobytes, which is one file of eight.
    records.extend_from_slice(&record(data, 4096, MULTI_EXTENT, &name, &[]));
    records.extend_from_slice(&record(data + 2, 4096, 0, &name, &[]));
    builder.write(root, &records);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    assert_eq!(shape(&tree), vec!["HUGE.BIN 8192 8192 file".to_string()]);
    assert!(!tree.stats().partial);
}

/// The last extent of a file that was never written. Its earlier records are
/// real, its total is not known, and reporting the part as the whole would be
/// a number that is wrong rather than one that is missing.
#[test]
fn a_file_whose_last_extent_is_missing_is_said_rather_than_halved() {
    let mut builder = Builder::new(Dialect::Published);
    let primary = builder.claim();
    let terminator = builder.claim();
    let root = builder.claim();
    let data = builder.claim();

    builder.descriptor(primary, 1, root, false);
    builder.terminator(terminator);

    let mut records = dots(root, root, &[]);
    records.extend_from_slice(&record(
        data,
        4096,
        MULTI_EXTENT,
        &published_name("HUGE.BIN", false),
        &[],
    ));
    builder.write(root, &records);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    assert!(tree.stats().partial);
    assert!(shape(&tree).is_empty(), "half a file is not an entry");
}

/// A record may not straddle a block boundary, so one that would have to is
/// pushed into the next block and zeros fill the gap. A reader that took the
/// zeros for the end of the directory would lose everything after the first
/// block of a large one.
#[test]
fn padding_at_the_end_of_a_block_is_not_the_end_of_the_directory() {
    let mut builder = Builder::new(Dialect::Published);
    let primary = builder.claim();
    let terminator = builder.claim();
    let root = builder.claim();
    let second = builder.claim();
    // One block each, so that the two files are not one run of blocks under two
    // names — which is a real shape and a different test's subject.
    let first_data = builder.claim();
    let second_data = builder.claim();

    builder.descriptor(primary, 1, root, false);
    builder.terminator(terminator);

    // Two blocks of records with the first one left part empty, which is
    // exactly what a directory whose next record did not fit looks like.
    let mut first = dots(root, root, &[]);
    first.extend_from_slice(&record(
        first_data,
        1024,
        0,
        &published_name("FIRST.BIN", false),
        &[],
    ));
    builder.write(root, &first);
    builder.write(
        second,
        &record(
            second_data,
            2048,
            0,
            &published_name("SECOND.BIN", false),
            &[],
        ),
    );

    // The root is two blocks long, and the second follows the first.
    assert_eq!(second, root + 1);
    let sector = builder.at(primary);
    both32(sector, 156 + 10, 2 * BLOCK as u32);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    assert_eq!(
        shape(&tree),
        vec![
            "FIRST.BIN 1024 2048 file".to_string(),
            "SECOND.BIN 2048 2048 file".to_string(),
        ]
    );
}

#[test]
fn a_path_inside_a_disc_resolves_whatever_case_it_is_typed_in() {
    for dialect in [Dialect::Published, Dialect::RockRidge, Dialect::Joliet] {
        let image = fixture(dialect);
        let mut fs = Filesystem::open(image.as_slice()).unwrap();
        for typed in ["docs", "DOCS", "Docs"] {
            let place = super::build::resolve(&mut fs, Path::new(typed))
                .unwrap()
                .unwrap_or_else(|| panic!("{dialect:?}: {typed} should resolve"));
            let tree = super::build::build(
                &mut fs,
                Path::new("/image/docs"),
                place,
                &ScanOptions::default(),
                |_: Progress| {},
                || false,
            )
            .unwrap();
            assert_eq!(tree.len(), 4, "{dialect:?}: {typed}");
        }
        assert!(
            super::build::resolve(&mut fs, Path::new("nowhere"))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn a_scan_that_is_cancelled_says_so() {
    let image = fixture(Dialect::RockRidge);
    let mut fs = Filesystem::open(image.as_slice()).unwrap();
    let start = super::build::root(&fs);
    let tree = super::build::build(
        &mut fs,
        Path::new("/image"),
        start,
        &ScanOptions::default(),
        |_: Progress| {},
        || true,
    )
    .unwrap();
    assert!(tree.stats().cancelled);
    assert!(tree.stats().partial);
}

#[test]
fn a_depth_limit_stops_the_walk_where_it_says() {
    let tree = tree_of(
        &fixture(Dialect::RockRidge),
        &ScanOptions {
            max_depth: Some(1),
            ..ScanOptions::default()
        },
    );
    let names = shape(&tree);
    assert!(names.iter().any(|row| row.starts_with("docs ")));
    assert!(
        !names.iter().any(|row| row.starts_with("note.md ")),
        "the walk went past the depth it was given: {names:?}"
    );
}

// ------------------------------------------------------------- damaged input

/// Every bit of every structure the reader trusts, flipped one at a time.
///
/// A disc image is a file somebody else wrote and a copy that ran out of room
/// is a truncated structure rather than an attack. Every one of these must
/// produce a tree or an error, and never a panic, a hang, or an allocation the
/// machine cannot serve.
#[test]
fn every_flipped_bit_produces_a_tree_or_an_error() {
    let (mut image, layout) = built(Dialect::RockRidge);
    let mut swept = 0usize;

    // The primary descriptor, the terminator, the three directories, and the
    // continuation area: every structure with a number in it the reader acts
    // on.
    let block = |at: u32| at as usize * BLOCK..(at as usize + 1) * BLOCK;
    let regions = [
        block(layout.primary),
        block(layout.terminator),
        block(layout.root),
        block(layout.docs),
        block(layout.deeper),
        block(layout.spill),
    ];
    for region in regions {
        for at in region {
            for bit in 0..8 {
                let original = image[at];
                image[at] ^= 1 << bit;
                if let Ok(mut fs) = Filesystem::open(image.as_slice()) {
                    let start = super::build::root(&fs);
                    let _ = super::build::build(
                        &mut fs,
                        Path::new("/image"),
                        start,
                        &ScanOptions::default(),
                        |_: Progress| {},
                        || false,
                    );
                }
                image[at] = original;
                swept += 1;
            }
        }
    }

    // Computed from what was covered rather than written down, and floored so
    // a sweep that quietly covered nothing fails rather than passes.
    assert_eq!(swept, 6 * BLOCK * 8);
    assert!(swept >= 90_000, "only {swept} mutations");
}

#[test]
fn every_truncation_produces_a_tree_or_an_error() {
    for dialect in [Dialect::Published, Dialect::RockRidge, Dialect::Joliet] {
        let image = fixture(dialect);
        // Every 512 bytes rather than every byte: a read that stops
        // mid-structure is the same case whichever byte it stopped on.
        for keep in (0..image.len()).step_by(512) {
            if let Ok(mut fs) = Filesystem::open(&image[..keep]) {
                let start = super::build::root(&fs);
                let _ = super::build::build(
                    &mut fs,
                    Path::new("/image"),
                    start,
                    &ScanOptions::default(),
                    |_: Progress| {},
                    || false,
                );
            }
        }
    }
}

/// A directory record naming its own directory. A disc cannot legally hold one
/// and a damaged one can, and following it would be a walk with no end.
#[test]
fn a_directory_that_names_itself_does_not_loop() {
    let mut builder = Builder::new(Dialect::Published);
    let primary = builder.claim();
    let terminator = builder.claim();
    let root = builder.claim();

    builder.descriptor(primary, 1, root, false);
    builder.terminator(terminator);

    let mut records = dots(root, root, &[]);
    // A third name for the root, which is neither `.` nor `..`.
    records.extend_from_slice(&record(
        root,
        BLOCK as u32,
        DIRECTORY,
        &published_name("AGAIN", true),
        &[],
    ));
    builder.write(root, &records);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    assert!(tree.len() < 8, "a self-naming directory produced a loop");
}

/// A record naming a block the disc does not have. The name is real and the
/// thing it names is not, so it is counted as unreadable rather than dropped
/// without trace.
#[test]
fn an_entry_pointing_off_the_disc_is_named_rather_than_dropped() {
    let mut builder = Builder::new(Dialect::Published);
    let primary = builder.claim();
    let terminator = builder.claim();
    let root = builder.claim();

    builder.descriptor(primary, 1, root, false);
    builder.terminator(terminator);

    let mut records = dots(root, root, &[]);
    records.extend_from_slice(&record(
        BLOCKS + 10,
        4096,
        0,
        &published_name("GONE.BIN", false),
        &[],
    ));
    builder.write(root, &records);

    let tree = tree_of(&builder.image, &ScanOptions::default());
    let names = shape(&tree);
    assert!(
        names.contains(&"GONE.BIN 0 0 file".to_string()),
        "the name is real even where the block is not: {names:?}"
    );
    assert!(tree.stats().partial);
}

// ------------------------------------------------- a disc that is also a disk

/// A bootable disc image, which is honestly two things at once.
///
/// ISO 9660 leaves its first sixteen blocks to whoever wrote the disc, and a
/// bootable installer puts a master boot record there and appends a FAT EFI
/// system partition — that is what makes it start on a UEFI machine. So the
/// same file *is* a disc and *is* a partitioned disk with a filesystem on it,
/// and both answers are true.
///
/// Only one of them is useful. **This is the test that pins which**, and it
/// exists because adding FAT made a five-gigabyte installer report the ten
/// megabytes of boot loader on it — an answer that was not wrong, and was no
/// use to anybody.
mod on_a_bootable_disc {
    use super::*;
    use crate::fat::format::Flavour;
    use crate::partition::tests::{SECTOR as DISK_SECTOR, mbr_entry};

    /// The disc, with a FAT boot partition appended and a table describing it.
    fn hybrid() -> (Vec<u8>, Layout) {
        let (disc, layout) = built(Dialect::RockRidge);
        let boot = crate::fat::tests::fixture(Flavour::Fat12);

        let mut disk = vec![0u8; disc.len() + boot.len()];
        disk[..disc.len()].copy_from_slice(&disc);
        disk[disc.len()..].copy_from_slice(&boot);
        mbr_entry(
            &mut disk,
            0,
            0,
            0x0C,
            (disc.len() / DISK_SECTOR) as u32,
            (boot.len() / DISK_SECTOR) as u32,
        );
        (disk, layout)
    }

    fn scan_image(disk: &[u8]) -> Result<crate::SizeTree, crate::ScanError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installer.iso");
        std::fs::write(&path, disk).unwrap();
        crate::Scanner::new(ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        })
        .scan(&path)
    }

    fn names(tree: &crate::SizeTree) -> Vec<String> {
        let mut rows: Vec<String> = (0..tree.len())
            .map(crate::NodeId::from_index)
            .filter(|&id| id != tree.root())
            .map(|id| tree.name_lossy(id).to_string())
            .collect();
        rows.sort();
        rows
    }

    #[test]
    fn the_disc_is_what_a_scan_of_it_means() {
        let (disk, _) = hybrid();
        let tree = scan_image(&disk).expect("a bootable disc is an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("ISO 9660 · Rock Ridge"),
            "the disc is the answer, not the boot partition on it"
        );
        assert!(names(&tree).contains(&"A long file name.bin".to_string()));
    }

    /// And the boot partition really is there, and really would have been
    /// found. Damaging the disc's own descriptor is the only way to show that
    /// the order above is a decision rather than an accident of what the
    /// fixture happens to contain.
    #[test]
    fn the_boot_partition_is_what_is_left_when_the_disc_is_not_readable() {
        let (mut disk, layout) = hybrid();
        // One byte of the primary descriptor's signature.
        disk[layout.primary as usize * BLOCK + 1] = b'X';

        let tree = scan_image(&disk).expect("a partitioned disk is still an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("FAT12 · partition 1 of 1 (MBR)"),
            "the partition table is the fallback, and it is a real one"
        );
    }
}

/// A supplementary descriptor that will not parse is one tree fewer, not a
/// dead disc. The files are in the primary tree; this one is a second set of
/// names over the same files, and refusing the disc because the spare copy of
/// the names is damaged would throw away everything to save nothing.
#[test]
fn a_damaged_joliet_descriptor_falls_back_to_the_primary_tree() {
    let (mut image, layout) = built(Dialect::Joliet);
    let at = layout.supplementary.expect("a Joliet fixture has one") as usize * BLOCK;
    // A block size no volume may state, which is the geometry check refusing
    // it rather than the signature.
    both16(&mut image, at + 128, 777);

    let fs = Filesystem::open(image.as_slice()).expect("the primary tree is untouched");
    assert_eq!(fs.naming(), Naming::Published);

    let tree = tree_of(&image, &ScanOptions::default());
    let names = shape(&tree);
    assert!(
        names.iter().any(|row| row.starts_with("A_LONG_F.BIN ")),
        "the published names are what is left: {names:?}"
    );
}
