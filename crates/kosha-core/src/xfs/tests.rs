// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! An XFS filesystem built byte by byte, and walked by exactly the code that
//! walks a real one.
//!
//! There is no test-only path through the reader: the builder below writes a
//! superblock, inodes, a shortform directory and a block directory the way the
//! format defines them, big-endian throughout. Which means these run — and
//! catch things — on a machine with no XFS on it at all, which is every
//! machine this is likely to be edited on.
//!
//! The reader was checked against a real 195 GiB filesystem as it was written,
//! and agreed with that filesystem's own accounting to two inodes out of
//! 104,862. What the real disk cannot do is run in CI, or fail when somebody
//! changes an offset.

use super::format::{self, Fork, Kind};
use super::reader::Filesystem;
use crate::scan::{Progress, ScanOptions};

pub(crate) const BLOCK: usize = 4096;
pub(crate) const INODE: usize = 512;
const INODE_LOG: u32 = 3;
const AG_BLOCK_LOG: u32 = 6;
const AG_BLOCKS: u64 = 64;

/// The first block holding inodes, and so where inode numbering starts.
const INODE_BLOCK: u64 = 8;
/// The block the one block-directory in the fixture lives in.
const DIR_BLOCK: u64 = 20;

pub(crate) fn root_inode() -> u64 {
    INODE_BLOCK * (1 << INODE_LOG)
}

fn put16(image: &mut [u8], at: usize, value: u16) {
    image[at..at + 2].copy_from_slice(&value.to_be_bytes());
}

fn put32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

fn put64(image: &mut [u8], at: usize, value: u64) {
    image[at..at + 8].copy_from_slice(&value.to_be_bytes());
}

/// What the fixture should be built with. Both of these change the layout of
/// something, which is why they are choices rather than constants.
#[derive(Clone, Copy)]
pub(crate) struct Options {
    pub ftype: bool,
    /// 64-bit extent counters, which move the count from 0x4C to 0x18. On by
    /// default in `mkfs.xfs` since xfsprogs 6.0, and on for the real disk this
    /// was checked against.
    pub big_extents: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            ftype: true,
            big_extents: true,
        }
    }
}

/// Where an inode's bytes begin.
fn inode_at(number: u64) -> usize {
    let block = (number >> INODE_LOG) & ((1 << AG_BLOCK_LOG) - 1);
    let slot = number & ((1 << INODE_LOG) - 1);
    (block as usize * BLOCK) + slot as usize * INODE
}

pub(crate) struct Builder {
    image: Vec<u8>,
    options: Options,
    next: u64,
}

impl Builder {
    pub(crate) fn new(options: Options) -> Builder {
        let mut image = vec![0u8; AG_BLOCKS as usize * BLOCK];
        image[0..4].copy_from_slice(b"XFSB");
        put32(&mut image, 0x04, BLOCK as u32);
        put64(&mut image, 0x08, AG_BLOCKS);
        put64(&mut image, 0x38, root_inode());
        put32(&mut image, 0x54, AG_BLOCKS as u32);
        put32(&mut image, 0x58, 1); // one allocation group
        // Version 5, with the "more bits" flag a v5 superblock also carries.
        put16(&mut image, 0x64, 5);
        put16(&mut image, 0x68, INODE as u16);
        put16(&mut image, 0x6A, (BLOCK / INODE) as u16);
        image[0x7B] = INODE_LOG as u8;
        image[0x7C] = AG_BLOCK_LOG as u8;
        image[0xC0] = 0; // one filesystem block per directory block
        let mut incompat = 0u32;
        if options.ftype {
            incompat |= 0x1;
        }
        if options.big_extents {
            incompat |= 0x20;
        }
        put32(&mut image, 0xD8, incompat);

        Builder {
            image,
            options,
            next: root_inode(),
        }
    }

    fn claim(&mut self) -> u64 {
        let number = self.next;
        self.next += 1;
        number
    }

    /// Write an inode's core. The fork is filled in by the caller.
    fn put_inode(&mut self, number: u64, mode: u16, fork: u8, size: u64, blocks: u64, links: u32) {
        let at = inode_at(number);
        self.image[at..at + 2].copy_from_slice(b"IN");
        put16(&mut self.image, at + 0x02, mode);
        self.image[at + 0x04] = 3; // version 3, so the fork begins at 176
        self.image[at + 0x05] = fork;
        put32(&mut self.image, at + 0x10, links);
        put64(&mut self.image, at + 0x38, size);
        put64(&mut self.image, at + 0x40, blocks);
        put32(&mut self.image, at + 0x28, 1_700_000_000); // mtime
    }

    fn set_extents(&mut self, number: u64, count: u64) {
        let at = inode_at(number);
        if self.options.big_extents {
            put64(&mut self.image, at + 0x18, count);
        } else {
            put32(&mut self.image, at + 0x4C, count as u32);
        }
    }

    /// A file of `size` bytes occupying `blocks` filesystem blocks.
    pub(crate) fn file(&mut self, size: u64, blocks: u64) -> u64 {
        let number = self.claim();
        self.put_inode(number, 0o100644, 2, size, blocks, 1);
        number
    }

    /// A file with more than one name, so hardlink accounting has something to
    /// do.
    pub(crate) fn hardlinked_file(&mut self, size: u64, blocks: u64) -> u64 {
        let number = self.claim();
        self.put_inode(number, 0o100644, 2, size, blocks, 2);
        number
    }

    pub(crate) fn symlink(&mut self, target: &str) -> u64 {
        let number = self.claim();
        self.put_inode(number, 0o120777, 1, target.len() as u64, 0, 1);
        let at = inode_at(number) + 176;
        self.image[at..at + target.len()].copy_from_slice(target.as_bytes());
        number
    }

    /// A directory whose entries live inside its own inode.
    pub(crate) fn shortform_dir(&mut self, parent: u64, entries: &[(&str, u64)]) -> u64 {
        let number = self.claim();
        self.put_inode(number, 0o40755, 1, 0, 0, 2);

        let base = inode_at(number) + 176;
        self.image[base] = entries.len() as u8;
        self.image[base + 1] = 0; // four-byte inode numbers throughout
        put32(&mut self.image, base + 2, parent as u32);

        let mut at = base + 6;
        for (name, child) in entries {
            self.image[at] = name.len() as u8;
            put16(&mut self.image, at + 1, 0); // offset, which nothing reads
            let name_at = at + 3;
            self.image[name_at..name_at + name.len()].copy_from_slice(name.as_bytes());
            let mut after = name_at + name.len();
            if self.options.ftype {
                self.image[after] = 1;
                after += 1;
            }
            put32(&mut self.image, after, *child as u32);
            at = after + 4;
        }
        number
    }

    /// A directory whose entries live in a block of their own, reached through
    /// an extent list.
    pub(crate) fn block_dir(&mut self, entries: &[(&str, u64)]) -> u64 {
        let number = self.claim();
        self.put_inode(number, 0o40755, 2, BLOCK as u64, 1, 2);
        self.set_extents(number, 1);

        // One extent: file offset 0, disk block DIR_BLOCK, one block long.
        let high = DIR_BLOCK >> 43;
        let low = (DIR_BLOCK << 21) | 1;
        let at = inode_at(number) + 176;
        put64(&mut self.image, at, high);
        put64(&mut self.image, at + 8, low);

        let block = DIR_BLOCK as usize * BLOCK;
        self.image[block..block + 4].copy_from_slice(b"XDB3");

        let mut cursor = block + 64;
        for (name, child) in entries {
            put64(&mut self.image, cursor, *child);
            self.image[cursor + 8] = name.len() as u8;
            let name_at = cursor + 9;
            self.image[name_at..name_at + name.len()].copy_from_slice(name.as_bytes());
            let used = 9 + name.len() + usize::from(self.options.ftype) + 2;
            cursor += used.next_multiple_of(8);
        }

        // The tail: a hash index nothing here reads, and a count of it. The
        // entries must stop before it, which is the whole reason it is written.
        let leaves = 2usize;
        let tail = block + BLOCK - 8;
        put32(&mut self.image, tail, leaves as u32);
        let entries_end = tail - leaves * 8;

        // Free space between the last entry and the index, announced the way
        // the format announces it rather than left as zeros — zeros would read
        // as an entry with no name.
        if entries_end > cursor {
            put16(&mut self.image, cursor, 0xFFFF);
            put16(&mut self.image, cursor + 2, (entries_end - cursor) as u16);
        }
        number
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.image
    }
}

/// A filesystem with one of everything in it.
pub(crate) fn fixture(options: Options) -> Vec<u8> {
    let mut builder = Builder::new(options);
    // The root claims the first number, so everything it names is built first
    // and wired up afterwards.
    let root = builder.claim();
    let big = builder.file(100_000, 25);
    let small = builder.file(6, 1);
    let link = builder.symlink("deep/one.bin");
    let deep = builder.shortform_dir(root, &[("one.bin", small)]);
    let wide = builder.block_dir(&[("a.bin", big), ("b.bin", small)]);

    builder.put_inode(root, 0o40755, 1, 0, 0, 2);
    let base = inode_at(root) + 176;
    builder.image[base] = 4;
    builder.image[base + 1] = 0;
    put32(&mut builder.image, base + 2, root as u32);
    let mut at = base + 6;
    for (name, child) in [
        ("big.bin", big),
        ("deep", deep),
        ("link", link),
        ("wide", wide),
    ] {
        builder.image[at] = name.len() as u8;
        let name_at = at + 3;
        builder.image[name_at..name_at + name.len()].copy_from_slice(name.as_bytes());
        let mut after = name_at + name.len();
        if options.ftype {
            builder.image[after] = 1;
            after += 1;
        }
        put32(&mut builder.image, after, child as u32);
        at = after + 4;
    }
    builder.finish()
}

fn tree_of(image: &[u8], options: &ScanOptions) -> crate::SizeTree {
    let mut fs = Filesystem::open(image).expect("the builder writes a readable superblock");
    super::build::build(
        &mut fs,
        std::path::Path::new("/image"),
        root_inode(),
        options,
        |_: Progress| {},
        || false,
    )
    .expect("a built image is walkable")
}

fn shape(tree: &crate::SizeTree) -> Vec<String> {
    let mut rows: Vec<String> = (0..tree.len())
        .map(crate::NodeId::from_index)
        .filter(|&id| id != tree.root())
        .map(|id| {
            let node = tree.node(id);
            format!(
                "{} {} {}",
                tree.name_lossy(id),
                node.allocated,
                if node.is_dir() { "dir" } else { "file" }
            )
        })
        .collect();
    rows.sort();
    rows
}

// ------------------------------------------------------------- the format

#[test]
fn a_superblock_that_is_not_one_is_refused() {
    assert_eq!(
        format::superblock(&[0u8; 512]),
        Err(format::Unreadable::NotXfs)
    );
}

/// The one feature that moves a field rather than adding one. Reading the old
/// place on a new filesystem gives zero extents, which is a directory reported
/// empty — a wrong answer wearing a right one's clothes.
#[test]
fn sixty_four_bit_extent_counts_are_read_from_the_right_place() {
    for big_extents in [false, true] {
        let image = fixture(Options {
            big_extents,
            ..Options::default()
        });
        let fs = Filesystem::open(image.as_slice()).unwrap();
        assert_eq!(fs.superblock().big_extent_counts, big_extents);

        // `wide` is the block directory, and its fork is an extent list.
        let tree = tree_of(&image, &ScanOptions::default());
        let names = shape(&tree);
        assert!(
            names.iter().any(|row| row.starts_with("a.bin ")),
            "with big_extents={big_extents} the block directory read as empty: {names:?}"
        );
    }
}

#[test]
fn a_feature_kosha_does_not_know_is_declined_rather_than_guessed_at() {
    let mut image = fixture(Options::default());
    // A bit nobody has defined yet.
    put32(&mut image, 0xD8, 0x8000_0000);
    assert_eq!(
        format::superblock(&image),
        Err(format::Unreadable::UnknownFeature)
    );
}

/// Every one of these divides or indexes something later, and a zero or an
/// absurdity is a panic or an enormous allocation a few hundred lines away.
#[test]
fn a_geometry_that_cannot_be_true_is_refused() {
    for (at, value, what) in [
        (0x04u32, 0u32, "a block size of zero"),
        (0x04, 7, "a block size that is not a power of two"),
        (0x04, 1 << 30, "a block size of a gigabyte"),
        (0x58, 0, "no allocation groups"),
        (0x54, 0, "an allocation group of no blocks"),
    ] {
        let mut image = fixture(Options::default());
        put32(&mut image, at as usize, value);
        assert!(format::superblock(&image).is_err(), "{what} was accepted");
    }
}

// --------------------------------------------------------------- the walk

#[test]
fn a_filesystem_walks_into_the_tree_it_describes() {
    let tree = tree_of(&fixture(Options::default()), &ScanOptions::default());
    assert_eq!(
        shape(&tree),
        vec![
            "a.bin 102400 file".to_string(),
            "b.bin 4096 file".to_string(),
            "big.bin 102400 file".to_string(),
            // A directory's own entry blocks plus everything under it: `deep`
            // has no blocks of its own and one 4 KiB file in it.
            "deep 4096 dir".to_string(),
            "link 0 file".to_string(),
            "one.bin 4096 file".to_string(),
            // One block of entries, and the two files they name.
            "wide 110592 dir".to_string(),
        ]
    );
}

/// A shortform directory is inside its inode and a block directory is not, and
/// the two are parsed by completely different code. Both are in the fixture so
/// that neither can be broken without a test noticing.
#[test]
fn both_shapes_of_directory_are_read() {
    let tree = tree_of(&fixture(Options::default()), &ScanOptions::default());
    let at = |name: &str| {
        (0..tree.len())
            .map(crate::NodeId::from_index)
            .find(|&id| tree.name_lossy(id) == name)
            .unwrap_or_else(|| panic!("{name} is not in the tree"))
    };
    assert_eq!(tree.node(at("deep")).child_count, 1, "shortform");
    assert_eq!(tree.node(at("wide")).child_count, 2, "block directory");
}

/// A symlink is measured as itself, never as what it points at — the same rule
/// the ordinary walk follows, and what stops a link into a large directory
/// counting that directory twice.
#[test]
fn a_symlink_is_itself_and_has_no_children() {
    let tree = tree_of(&fixture(Options::default()), &ScanOptions::default());
    let link = (0..tree.len())
        .map(crate::NodeId::from_index)
        .find(|&id| tree.name_lossy(id) == "link")
        .expect("the link");
    assert!(tree.node(link).flags.contains(crate::Flags::SYMLINK));
    assert_eq!(tree.node(link).child_count, 0);
}

/// `.` and `..` are entries like any other in XFS, and following either is a
/// walk with no end rather than a subtree.
#[test]
fn a_directory_that_names_itself_does_not_loop() {
    let options = Options::default();
    let mut builder = Builder::new(options);
    let root = builder.claim();
    builder.put_inode(root, 0o40755, 1, 0, 0, 2);
    let base = inode_at(root) + 176;
    builder.image[base] = 2;
    put32(&mut builder.image, base + 2, root as u32);
    let mut at = base + 6;
    for name in [".", "loop"] {
        builder.image[at] = name.len() as u8;
        let name_at = at + 3;
        builder.image[name_at..name_at + name.len()].copy_from_slice(name.as_bytes());
        let mut after = name_at + name.len();
        if options.ftype {
            builder.image[after] = 1;
            after += 1;
        }
        // Both entries name the root itself.
        put32(&mut builder.image, after, root as u32);
        at = after + 4;
    }

    let tree = tree_of(&builder.finish(), &ScanOptions::default());
    assert!(tree.len() < 8, "a self-naming directory produced a loop");
}

#[test]
fn hardlinked_bytes_are_counted_once_when_asked() {
    let options = Options::default();
    let mut builder = Builder::new(options);
    let root = builder.claim();
    let shared = builder.hardlinked_file(4096, 1);
    builder.put_inode(root, 0o40755, 1, 0, 0, 2);
    let base = inode_at(root) + 176;
    builder.image[base] = 2;
    put32(&mut builder.image, base + 2, root as u32);
    let mut at = base + 6;
    for name in ["one", "two"] {
        builder.image[at] = name.len() as u8;
        let name_at = at + 3;
        builder.image[name_at..name_at + name.len()].copy_from_slice(name.as_bytes());
        let mut after = name_at + name.len();
        if options.ftype {
            builder.image[after] = 1;
            after += 1;
        }
        put32(&mut builder.image, after, shared as u32);
        at = after + 4;
    }
    let image = builder.finish();

    let both = tree_of(
        &image,
        &ScanOptions {
            dedup_hardlinks: Some(false),
            ..ScanOptions::default()
        },
    );
    // The default, because the link count came free with the inode.
    let once = tree_of(&image, &ScanOptions::default());
    assert_eq!(both.total_allocated(), 8192, "two names, two lots of bytes");
    assert_eq!(once.total_allocated(), 4096, "the second name is free");
}

// ------------------------------------------------------------ damaged input

/// Every bit of every structure the reader trusts, flipped one at a time.
///
/// XFS is read out of an image somebody else wrote, and an image being copied
/// or a disk being written to is a half-updated structure rather than an
/// attack. Every one of these must produce a tree or an error, and never a
/// panic, a hang, or an allocation the machine cannot serve.
#[test]
fn every_flipped_bit_produces_a_tree_or_an_error() {
    let image = fixture(Options::default());
    let mut swept = 0usize;

    // The superblock, the inode block, and the one directory block: every
    // structure that has a number in it that the reader acts on.
    let regions = [
        0..512usize,
        INODE_BLOCK as usize * BLOCK..INODE_BLOCK as usize * BLOCK + 8 * INODE,
        DIR_BLOCK as usize * BLOCK..DIR_BLOCK as usize * BLOCK + 512,
    ];
    for region in regions {
        for at in region {
            for bit in 0..8 {
                let mut mutated = image.clone();
                mutated[at] ^= 1 << bit;
                if let Ok(mut fs) = Filesystem::open(mutated.as_slice()) {
                    let _ = super::build::build(
                        &mut fs,
                        std::path::Path::new("/image"),
                        root_inode(),
                        &ScanOptions::default(),
                        |_: Progress| {},
                        || false,
                    );
                }
                swept += 1;
            }
        }
    }

    // Computed from what was covered rather than written down, and floored so
    // a sweep that quietly covered nothing fails rather than passes.
    assert_eq!(swept, (512 + 8 * INODE + 512) * 8);
    assert!(swept >= 20_000, "only {swept} mutations");
}

#[test]
fn every_truncation_produces_a_tree_or_an_error() {
    let image = fixture(Options::default());
    // Every sector boundary, rather than every byte: the image is 256 KiB and
    // a read that stops mid-structure is the same case whichever byte it
    // stopped on.
    for keep in (0..image.len()).step_by(512) {
        if let Ok(mut fs) = Filesystem::open(&image[..keep]) {
            let _ = super::build::build(
                &mut fs,
                std::path::Path::new("/image"),
                root_inode(),
                &ScanOptions::default(),
                |_: Progress| {},
                || false,
            );
        }
    }
}

/// An extent count comes off the disk, and a directory's blocks are read into
/// memory. Without a bound one flipped bit asks for an allocation nothing can
/// serve — which is not an error anybody can catch, because a failed
/// allocation aborts. The same shape as the 68 GB ext4 inode table.
#[test]
fn an_absurd_extent_count_is_refused_rather_than_allocated() {
    let mut image = fixture(Options::default());
    // `wide` is the sixth inode claimed, and its fork is an extent list.
    let wide = root_inode() + 5;
    // A single extent claiming every block a 21-bit length can express: two
    // million blocks, which is eight gigabytes of directory read into memory
    // if nothing stops it.
    let at = inode_at(wide) + 176;
    put64(&mut image, at, 0);
    put64(&mut image, at + 8, (1u64 << 21) | 0x001F_FFFF);
    put64(&mut image, inode_at(wide) + 0x18, 1);

    let mut fs = Filesystem::open(image.as_slice()).expect("the superblock is untouched");
    let tree = super::build::build(
        &mut fs,
        std::path::Path::new("/image"),
        root_inode(),
        &ScanOptions::default(),
        |_: Progress| {},
        || false,
    )
    .expect("the rest of the filesystem is still readable");
    assert!(
        tree.stats().partial,
        "a directory claiming two million blocks has to be refused, and said so"
    );
    assert!(
        tree.total_allocated() > 0,
        "the rest of the filesystem still reads"
    );
}

/// A fork the reader does not walk is a named refusal rather than an empty
/// directory: empty is a claim and unreadable is the truth.
#[test]
fn an_inode_with_an_unknown_fork_is_not_reported_empty() {
    let mut image = fixture(Options::default());
    let deep = root_inode() + 4;
    image[inode_at(deep) + 0x05] = 9; // not local, extents or b+tree

    let mut fs = Filesystem::open(image.as_slice()).unwrap();
    let tree = super::build::build(
        &mut fs,
        std::path::Path::new("/image"),
        root_inode(),
        &ScanOptions::default(),
        |_: Progress| {},
        || false,
    )
    .unwrap();
    assert!(tree.stats().partial);
}

#[test]
fn the_fork_of_an_inode_is_where_its_version_says() {
    let image = fixture(Options::default());
    let mut fs = Filesystem::open(image.as_slice()).unwrap();
    let (inode, _) = fs.inode(root_inode()).unwrap().expect("the root");
    assert_eq!(inode.kind, Kind::Directory);
    assert_eq!(inode.fork, Fork::Local);
    assert_eq!(inode.fork_at, 176, "a version 3 inode's core is 176 bytes");
}
