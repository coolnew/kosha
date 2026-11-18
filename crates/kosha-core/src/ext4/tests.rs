// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! An ext4 filesystem built byte by byte, and walked by exactly the code that
//! walks a real one.
//!
//! There is no test-only path through the reader: the builder below writes a
//! superblock, a group descriptor, an inode table and directory blocks, and
//! [`Filesystem`] reads them the way it reads a distribution's. Which means
//! these run — and catch things — on a machine with no WSL on it at all.

use std::path::Path;

use super::build;
use super::format::{self, Kind};
use super::reader::Filesystem;
use crate::node::Flags;
use crate::scan::{Progress, ScanOptions};

const BLOCK: u64 = 4096;
const INODE_SIZE: u32 = 256;
const INODES_PER_GROUP: u32 = 256;
const BLOCKS_PER_GROUP: u32 = 8192;

/// Where each piece of the filesystem goes. One block group is enough for
/// everything here, and the layout is the ordinary one.
const DESCRIPTOR_BLOCK: u64 = 1;
const INODE_TABLE_BLOCK: u64 = 2;
/// The inode table is 256 inodes of 256 bytes: sixteen blocks.
const FIRST_DATA_BLOCK: u64 = INODE_TABLE_BLOCK + 16;

const INCOMPAT_FILETYPE: u32 = 0x0002;
const INCOMPAT_64BIT: u32 = 0x0080;
const EXTENTS_FLAG: u32 = 0x0008_0000;

/// What the builder is asked to make.
struct Entry {
    kind: Kind,
    children: Vec<(String, u32)>,
    size: u64,
    /// Bytes on disk, which becomes `i_blocks` in 512-byte units.
    allocated: u64,
    mtime: u32,
    links: u16,
    /// Map this inode's data with the pre-extent chain of block pointers
    /// instead of an extent tree, the way ext2 and ext3 do.
    indirect: bool,
}

struct Builder {
    /// Indexed by `inode - 2`, so entry zero is the root.
    entries: Vec<Entry>,
    /// Directory blocks that carry something other than plain records.
    padding: std::collections::HashMap<u32, Padding>,
    filetype: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Padding {
    /// A record with inode zero in the middle, which is what an erased entry
    /// and a hash-tree index node both look like.
    Erased,
    /// The twelve-byte checksum record `metadata_csum` puts at the end.
    Checksum,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            entries: vec![Entry {
                kind: Kind::Directory,
                children: Vec::new(),
                size: BLOCK,
                allocated: BLOCK,
                mtime: 1_600_000_000,
                links: 2,
                indirect: false,
            }],
            padding: std::collections::HashMap::new(),
            filetype: true,
        }
    }

    fn root(&self) -> u32 {
        format::ROOT_INODE
    }

    fn add(&mut self, parent: u32, name: &str, entry: Entry) -> u32 {
        let inode = format::ROOT_INODE + self.entries.len() as u32;
        self.entries.push(entry);
        self.entries[(parent - format::ROOT_INODE) as usize]
            .children
            .push((name.to_string(), inode));
        inode
    }

    fn dir(&mut self, parent: u32, name: &str) -> u32 {
        self.add(
            parent,
            name,
            Entry {
                kind: Kind::Directory,
                children: Vec::new(),
                size: BLOCK,
                allocated: BLOCK,
                mtime: 1_600_000_000,
                links: 2,
                indirect: false,
            },
        )
    }

    fn file(&mut self, parent: u32, name: &str, size: u64) -> u32 {
        self.add(
            parent,
            name,
            Entry {
                kind: Kind::File,
                children: Vec::new(),
                size,
                allocated: size.div_ceil(BLOCK) * BLOCK,
                mtime: 1_600_000_000,
                links: 1,
                indirect: false,
            },
        )
    }

    /// A file that occupies far less than it claims.
    fn sparse(&mut self, parent: u32, name: &str, size: u64, allocated: u64) -> u32 {
        let inode = self.file(parent, name, size);
        self.entries[(inode - format::ROOT_INODE) as usize].allocated = allocated;
        inode
    }

    fn symlink(&mut self, parent: u32, name: &str, target: &str) -> u32 {
        self.add(
            parent,
            name,
            Entry {
                kind: Kind::Symlink,
                children: Vec::new(),
                size: target.len() as u64,
                // A short target lives inside the inode and occupies nothing.
                allocated: 0,
                mtime: 1_600_000_000,
                links: 1,
                indirect: false,
            },
        )
    }

    /// A second name for a file that already exists.
    fn hardlink(&mut self, parent: u32, name: &str, inode: u32) {
        self.entries[(inode - format::ROOT_INODE) as usize].links += 1;
        self.entries[(parent - format::ROOT_INODE) as usize]
            .children
            .push((name.to_string(), inode));
    }

    fn build(&self) -> Vec<u8> {
        // One data block per directory. Everything here fits.
        let directories: Vec<u32> = (0..self.entries.len() as u32)
            .filter(|index| self.entries[*index as usize].kind == Kind::Directory)
            .map(|index| index + format::ROOT_INODE)
            .collect();

        let total_blocks = FIRST_DATA_BLOCK + directories.len() as u64 + 8;
        let mut image = vec![0u8; (total_blocks * BLOCK) as usize];

        self.put_superblock(&mut image, total_blocks);
        // The group descriptor: the only field anything reads is where the
        // inode table starts.
        put32(
            &mut image,
            (DESCRIPTOR_BLOCK * BLOCK) as usize + 0x08,
            INODE_TABLE_BLOCK as u32,
        );

        for (slot, inode) in directories.iter().enumerate() {
            let block = FIRST_DATA_BLOCK + slot as u64;
            self.put_directory(&mut image, *inode, block);
            self.put_inode(&mut image, *inode, Some(block));
        }
        for index in 0..self.entries.len() as u32 {
            let inode = index + format::ROOT_INODE;
            if self.entries[index as usize].kind != Kind::Directory {
                self.put_inode(&mut image, inode, None);
            }
        }
        image
    }

    fn put_superblock(&self, image: &mut [u8], total_blocks: u64) {
        let at = 1024usize;
        put32(image, at, INODES_PER_GROUP);
        put32(image, at + 0x04, total_blocks as u32);
        put32(image, at + 0x14, 0); // first_data_block: zero above 1 KiB blocks
        put32(image, at + 0x18, 2); // 1024 << 2 == 4096
        put32(image, at + 0x20, BLOCKS_PER_GROUP);
        put32(image, at + 0x28, INODES_PER_GROUP);
        put16(image, at + 0x38, 0xEF53);
        put32(image, at + 0x4C, 1); // revision 1, so `s_inode_size` is real
        put16(image, at + 0x58, INODE_SIZE as u16);
        let mut incompat = INCOMPAT_64BIT;
        if self.filetype {
            incompat |= INCOMPAT_FILETYPE;
        }
        put32(image, at + 0x60, incompat);
        put16(image, at + 0xFE, 64); // descriptor size
    }

    /// One directory's block: `.`, `..`, then its children.
    fn put_directory(&self, image: &mut [u8], inode: u32, block: u64) {
        let base = (block * BLOCK) as usize;
        let entry = &self.entries[(inode - format::ROOT_INODE) as usize];
        let padding = self.padding.get(&inode).copied();

        let mut at = base;
        let mut records: Vec<(u32, u8, &str)> = vec![(inode, 2, "."), (inode, 2, "..")];
        for (name, child) in &entry.children {
            let kind = match self.entries[(*child - format::ROOT_INODE) as usize].kind {
                Kind::Directory => 2u8,
                Kind::Symlink => 7,
                _ => 1,
            };
            records.push((*child, if self.filetype { kind } else { 0 }, name));
        }

        for (index, (child, kind, name)) in records.iter().enumerate() {
            let needed = 8 + name.len();
            let length = needed.div_ceil(4) * 4;
            let last = index + 1 == records.len();

            // The last record's length runs to the end of the block, which is
            // how the format says "nothing after this".
            let reserved = if last && padding.is_none() {
                base + BLOCK as usize - at
            } else {
                length
            };
            put32(image, at, *child);
            put16(image, at + 4, reserved as u16);
            image[at + 6] = name.len() as u8;
            image[at + 7] = *kind;
            image[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
            at += reserved;

            if last && padding == Some(Padding::Erased) {
                // A record naming nothing, covering the rest of the block. An
                // erased entry and a hash-tree index node both look like this.
                put32(image, at, 0);
                put16(image, at + 4, (base + BLOCK as usize - at) as u16);
            } else if last && padding == Some(Padding::Checksum) {
                let tail = base + BLOCK as usize - 12;
                put32(image, at, 0);
                put16(image, at + 4, (tail - at) as u16);
                put32(image, tail, 0);
                put16(image, tail + 4, 12);
            }
        }
    }

    fn put_inode(&self, image: &mut [u8], inode: u32, block: Option<u64>) {
        let entry = &self.entries[(inode - format::ROOT_INODE) as usize];
        let at = (INODE_TABLE_BLOCK * BLOCK) as usize
            + ((inode - 1) % INODES_PER_GROUP) as usize * INODE_SIZE as usize;

        let mode = match entry.kind {
            Kind::Directory => 0x41EDu16,
            Kind::File => 0x81A4,
            Kind::Symlink => 0xA1FF,
            Kind::Other => 0x1000,
        };
        put16(image, at, mode);
        put32(image, at + 0x04, entry.size as u32);
        put32(image, at + 0x6C, (entry.size >> 32) as u32);
        put32(image, at + 0x10, entry.mtime);
        put16(image, at + 0x1A, entry.links);
        put32(image, at + 0x1C, (entry.allocated / 512) as u32);

        match block {
            Some(block) if entry.indirect => {
                // The pre-extent map: the first of the twelve direct pointers.
                put32(image, at + 0x28, block as u32);
            }
            Some(block) => {
                put32(image, at + 0x20, EXTENTS_FLAG);
                let root = at + 0x28;
                put16(image, root, 0xF30A);
                put16(image, root + 0x02, 1); // one entry
                put16(image, root + 0x04, 4); // four would fit
                put16(image, root + 0x06, 0); // a leaf
                put32(image, root + 0x0C, 0); // first block of the file
                put16(image, root + 0x10, 1); // one block long
                put32(image, root + 0x14, block as u32);
            }
            None => {
                // A file, whose data blocks are never read. An extent tree with
                // nothing in it is what a reader would find if it looked.
                put32(image, at + 0x20, EXTENTS_FLAG);
                put16(image, at + 0x28, 0xF30A);
                put16(image, at + 0x2C, 4);
            }
        }
    }
}

fn put16(image: &mut [u8], at: usize, value: u16) {
    image[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A redirector path, spelled for the platform running the test.
///
/// `file_name` is what gives the root its label, and on Linux a backslash is an
/// ordinary character — so the Windows spelling makes the whole string one
/// component and every assertion about the root's name fails. The same trap the
/// `ntfs` tests hit.
fn redirector(inside: &[&str]) -> std::path::PathBuf {
    let mut path = if cfg!(windows) {
        std::path::PathBuf::from(r"\\wsl.localhost\test")
    } else {
        std::path::PathBuf::from("/wsl/test")
    };
    for part in inside {
        path.push(part);
    }
    path
}

/// Walk a built image and hand back the tree.
fn tree_of(image: &[u8], options: &ScanOptions) -> crate::SizeTree {
    let mut fs = Filesystem::open(image).expect("the builder writes a readable superblock");
    build::build(
        &mut fs,
        &redirector(&[]),
        format::ROOT_INODE,
        options,
        |_: Progress| {},
        || false,
    )
    .expect("a built image is readable")
}

/// Follow names down from the root, without going through a platform's idea of
/// a path separator.
fn at(tree: &crate::SizeTree, names: &[&str]) -> crate::NodeId {
    let mut id = tree.root();
    for name in names {
        id = tree
            .children(id)
            .find(|&child| tree.name_lossy(child) == *name)
            .unwrap_or_else(|| panic!("{name} is not below {}", tree.name_lossy(id)));
    }
    id
}

/// The invariants everything downstream is written against. Breaking one does
/// not fail loudly — it produces a tree that renders, sorts and exports, and is
/// wrong.
fn assert_arena_invariants(tree: &crate::SizeTree) {
    let nodes = tree.nodes();
    assert_eq!(nodes[0].parent, 0, "the root is its own parent");
    for (index, node) in nodes.iter().enumerate().skip(1) {
        assert!(
            (node.parent as usize) < index,
            "node {index} comes before its parent"
        );
    }
    for (index, node) in nodes.iter().enumerate() {
        let range = node.child_range();
        if node.child_count > 0 {
            assert!(range.start > index, "a child sits before its parent");
            assert!(range.end <= nodes.len(), "children run past the arena");
            // Contiguous and sorted by name.
            let mut previous: Option<&std::ffi::OsStr> = None;
            for child in range {
                let name = tree.name(crate::NodeId::from_index(child));
                if let Some(before) = previous {
                    assert!(before <= name, "a directory's children are out of order");
                }
                previous = Some(name);
            }
        }
    }
}

// ---------------------------------------------------------------- the tests

#[test]
fn a_filesystem_becomes_a_tree() {
    let mut fs = Builder::new();
    let root = fs.root();
    let bin = fs.dir(root, "bin");
    fs.file(bin, "sh", 100_000);
    fs.file(bin, "ls", 50_000);
    let etc = fs.dir(root, "etc");
    fs.file(etc, "hostname", 10);

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    assert_arena_invariants(&tree);

    assert_eq!(tree.stats().dirs, 3, "root, bin and etc");
    assert_eq!(tree.stats().files, 3);
    assert_eq!(tree.node(at(&tree, &["bin", "sh"])).logical, 100_000);
    assert_eq!(tree.node(at(&tree, &["etc", "hostname"])).logical, 10);
}

/// The whole reason for the second pass: a size has to reach the node it
/// belongs to after the arena is built, and getting the mapping wrong gives
/// every file somebody else's number.
#[test]
fn every_file_gets_its_own_size() {
    let mut fs = Builder::new();
    let root = fs.root();
    let a = fs.dir(root, "a");
    let b = fs.dir(root, "b");
    fs.file(a, "one", 1_000);
    fs.file(b, "two", 2_000);
    fs.file(a, "three", 3_000);
    fs.file(b, "four", 4_000);

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    assert_eq!(tree.node(at(&tree, &["a", "one"])).logical, 1_000);
    assert_eq!(tree.node(at(&tree, &["b", "two"])).logical, 2_000);
    assert_eq!(tree.node(at(&tree, &["a", "three"])).logical, 3_000);
    assert_eq!(tree.node(at(&tree, &["b", "four"])).logical, 4_000);
}

/// `.` and `..` are this directory and its parent. Either one becoming a child
/// is a tree that never ends.
#[test]
fn dot_and_dot_dot_are_not_children() {
    let mut fs = Builder::new();
    let root = fs.root();
    fs.dir(root, "sub");

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    for id in tree.children(tree.root()) {
        assert_ne!(tree.name_lossy(id), ".");
        assert_ne!(tree.name_lossy(id), "..");
    }
    assert_eq!(tree.children(tree.root()).count(), 1);
}

/// A hardlinked file is one inode under two names. The first name reached owns
/// the bytes; the second contributes nothing and frees nothing.
#[test]
fn a_second_name_for_a_file_is_counted_once() {
    let mut fs = Builder::new();
    let root = fs.root();
    let bin = fs.dir(root, "bin");
    let busybox = fs.file(bin, "busybox", 900_000);
    fs.hardlink(bin, "sh", busybox);
    fs.hardlink(bin, "ls", busybox);

    // The default, on both platforms: an image reader has the link count in
    // the inode it has already read, so there is nothing to pay and nothing to
    // ask about.
    let counted = tree_of(&fs.build(), &ScanOptions::default());
    assert_eq!(counted.stats().hardlinks_deduped, 2);
    // 900 000 rounds up to 221 blocks of 4 KiB.
    let once = 900_000u64.div_ceil(BLOCK) * BLOCK;
    assert_eq!(counted.total_allocated() - BLOCK * 2, once);

    // And asked to stop, every name carries its own bytes — which is what
    // `du -l` reports and what somebody comparing against Explorer expects.
    // `Some(false)`, not the default: "I did not ask" and "do not" are
    // different answers now.
    let separately = tree_of(
        &fs.build(),
        &ScanOptions {
            dedup_hardlinks: Some(false),
            ..Default::default()
        },
    );
    assert_eq!(separately.stats().hardlinks_deduped, 0);
    assert_eq!(separately.total_allocated() - BLOCK * 2, once * 3);
}

/// A symlink is measured as itself and never followed, whatever it points at.
#[test]
fn a_symlink_is_measured_as_itself() {
    let mut fs = Builder::new();
    let root = fs.root();
    fs.symlink(root, "usr", "/usr/local/a/very/long/target/path");

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    let link = at(&tree, &["usr"]);
    assert!(tree.node(link).is_symlink());
    assert!(!tree.node(link).is_dir());
    assert_eq!(tree.node(link).allocated, 0, "a short target is free");
    assert_eq!(tree.children(link).count(), 0);

    // And *not* sparse. A symlink short enough to live inside its own inode
    // occupies nothing while claiming the length of its target, so a naive
    // "occupies less than it claims" marks every symlink in a distribution as
    // a sparse file. The ordinary walk allows a block of slack for the same
    // reason and the two have to agree.
    assert!(
        !tree.node(link).flags.contains(Flags::SPARSE),
        "a symlink was reported as a sparse file"
    );
}

/// The line between the two: a block of slack, exactly as the Unix walk allows.
#[test]
fn a_file_shorter_than_one_block_is_not_sparse() {
    let mut fs = Builder::new();
    let root = fs.root();
    // Claims a little under two blocks, occupies one. Inside the slack.
    fs.sparse(root, "snug.bin", BLOCK + 100, BLOCK);
    // Claims far more than it occupies. Outside it.
    fs.sparse(root, "holey.bin", 100 * BLOCK, BLOCK);

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    assert!(
        !tree
            .node(at(&tree, &["snug.bin"]))
            .flags
            .contains(Flags::SPARSE)
    );
    assert!(
        tree.node(at(&tree, &["holey.bin"]))
            .flags
            .contains(Flags::SPARSE)
    );
}

/// The exact numbers are the point: `i_blocks` is what a file really occupies,
/// so a sparse file is reported as what it costs rather than as what it claims.
#[test]
fn a_sparse_file_reports_what_it_occupies() {
    let mut fs = Builder::new();
    let root = fs.root();
    fs.sparse(root, "disk.img", 40_000_000_000, 4096);

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    let id = at(&tree, &["disk.img"]);
    assert_eq!(tree.node(id).logical, 40_000_000_000);
    assert_eq!(tree.node(id).allocated, 4096);
    assert!(tree.node(id).flags.contains(Flags::SPARSE));
}

/// A large directory grows a hash tree, and its index nodes are stored as
/// blocks that look like one record naming nothing. Walking blocks linearly and
/// skipping those visits every name exactly once — and the same rule skips the
/// checksum record `metadata_csum` leaves at the end of a block.
#[test]
fn records_naming_nothing_are_skipped() {
    for padding in [Padding::Erased, Padding::Checksum] {
        let mut fs = Builder::new();
        let root = fs.root();
        fs.file(root, "kept", 10);
        fs.padding.insert(root, padding);

        let tree = tree_of(&fs.build(), &ScanOptions::default());
        assert_eq!(tree.children(tree.root()).count(), 1);
        assert_eq!(tree.name_lossy(at(&tree, &["kept"])), "kept");
    }
}

/// ext2 and ext3 map a file's blocks with a chain of pointers rather than an
/// extent tree, and so does a file on an ext4 filesystem that predates its
/// upgrade. A directory mapped that way must still be read.
#[test]
fn a_directory_mapped_the_old_way_is_still_read() {
    let mut fs = Builder::new();
    let root = fs.root();
    let old = fs.dir(root, "old");
    fs.file(old, "inside", 1_234);
    fs.entries[(old - format::ROOT_INODE) as usize].indirect = true;

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    assert_eq!(tree.children(at(&tree, &["old"])).count(), 1);
    assert_eq!(tree.node(at(&tree, &["old", "inside"])).logical, 1_234);
}

/// Without `filetype` in its entries — an ext2 filesystem old enough — the
/// only way to tell a directory from a file is to read the inode. Slower, and
/// it has to produce the same tree.
#[test]
fn a_filesystem_without_types_in_its_entries_still_walks() {
    let mut fs = Builder::new();
    fs.filetype = false;
    let root = fs.root();
    let sub = fs.dir(root, "sub");
    fs.file(sub, "deep", 77);
    fs.file(root, "shallow", 88);

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    assert_arena_invariants(&tree);
    assert!(tree.node(at(&tree, &["sub"])).is_dir());
    assert!(!tree.node(at(&tree, &["shallow"])).is_dir());
    assert_eq!(tree.node(at(&tree, &["sub", "deep"])).logical, 77);
}

/// A scan of one directory of a distribution should cost what that directory
/// costs, so the path is resolved and the walk starts there.
#[test]
fn a_scan_can_start_below_the_filesystem_root() {
    let mut fs = Builder::new();
    let root = fs.root();
    let home = fs.dir(root, "home");
    let me = fs.dir(home, "me");
    fs.file(me, "notes.txt", 4_000);
    fs.file(root, "elsewhere.bin", 9_000_000);

    let image = fs.build();
    let mut filesystem = Filesystem::open(image.as_slice()).unwrap();
    let start = build::resolve(&mut filesystem, Path::new("home/me"))
        .unwrap()
        .expect("home/me is in the image");

    let tree = build::build(
        &mut filesystem,
        &redirector(&["home", "me"]),
        start,
        &ScanOptions::default(),
        |_: Progress| {},
        || false,
    )
    .unwrap();

    assert_arena_invariants(&tree);
    assert_eq!(tree.stats().entries, 2, "the directory and one file");
    assert_eq!(tree.name_lossy(tree.root()), "me");
    assert_eq!(tree.node(at(&tree, &["notes.txt"])).logical, 4_000);
}

#[test]
fn a_path_that_is_not_in_the_image_resolves_to_nothing() {
    let mut fs = Builder::new();
    let root = fs.root();
    let home = fs.dir(root, "home");
    fs.file(home, "a-file", 1);

    let image = fs.build();
    let mut filesystem = Filesystem::open(image.as_slice()).unwrap();
    for missing in ["nowhere", "home/nowhere", "home/a-file/deeper"] {
        assert!(
            build::resolve(&mut filesystem, Path::new(missing))
                .unwrap()
                .is_none(),
            "`{missing}` resolved to something"
        );
    }
}

/// A directory cannot really be hardlinked, so a cycle means a corrupt image —
/// and a cycle is a tree that never ends.
#[test]
fn a_directory_loop_terminates() {
    let mut fs = Builder::new();
    let root = fs.root();
    let a = fs.dir(root, "a");
    let b = fs.dir(a, "b");
    // `b` names `a` as a child of its own, which nothing legitimate does.
    fs.entries[(b - format::ROOT_INODE) as usize]
        .children
        .push(("back".to_string(), a));

    let tree = tree_of(&fs.build(), &ScanOptions::default());
    assert_arena_invariants(&tree);
    // The loop is entered once and not followed a second time.
    let back = at(&tree, &["a", "b", "back"]);
    assert_eq!(tree.children(back).count(), 0);
}

#[test]
fn an_image_that_is_not_a_filesystem_is_refused() {
    let empty = vec![0u8; 64 * 1024];
    assert!(Filesystem::open(empty.as_slice()).is_err());
}

/// Every one of these is a number something later divides or indexes by.
#[test]
fn impossible_geometry_is_refused_rather_than_divided_by() {
    let mut fs = Builder::new();
    fs.dir(fs.root(), "sub");
    let whole = fs.build();

    let broken: &[(usize, u32, &str)] = &[
        (0x28, 0, "inodes per group"),
        (0x20, 0, "blocks per group"),
        (0x04, 0, "block count"),
        (0x00, 0, "inode count"),
        (0x18, 9, "block size"),
    ];
    for (offset, value, what) in broken {
        let mut image = whole.clone();
        put32(&mut image, 1024 + offset, *value);
        assert!(
            Filesystem::open(image.as_slice()).is_err(),
            "a filesystem with no {what} was accepted"
        );
    }
}

/// The superblock is 1024 bytes into the volume whatever the block size is,
/// which is what lets it sit behind a boot sector. Reading it from block zero
/// instead would work on a 1 KiB filesystem and nowhere else.
#[test]
fn the_superblock_is_found_at_a_fixed_offset() {
    let mut fs = Builder::new();
    fs.dir(fs.root(), "sub");
    let image = fs.build();
    assert_eq!(format::SUPERBLOCK_OFFSET, 1024);
    assert!(format::superblock(&image[1024..]).is_ok());
    assert!(format::superblock(&image[..1024]).is_err());
}

// ------------------------------------------------- nothing may panic, ever

/// Every structure the reader trusts, with one bit wrong — superblock, group
/// descriptor, inode table and a directory block.
///
/// The tests above break the filesystem in the ways somebody thought of. This
/// breaks it in the ways nobody did. Each mutation must open and walk, or be
/// refused; never panic, and never hang.
///
/// **A corrupt image here is the ordinary case, not an attack.** kosha reads a
/// distribution's `ext4.vhdx` *while the distribution is running*, so these
/// bytes are being rewritten underneath the read — a half-updated inode table
/// is what a live filesystem looks like from outside. An index out of bounds
/// in one would take down a scan somebody started by clicking a row.
///
/// The walk is bounded by construction — `entered` refuses an inode twice,
/// `.` and `..` are skipped, and the frontier is breadth-first rather than
/// recursive — so a mutation that invents a cycle ends rather than hanging.
/// That is what makes this test safe to write as a loop with no timeout.
#[test]
fn no_single_flipped_bit_can_panic_the_walk() {
    let mut builder = Builder::new();
    let root = builder.root();
    let docs = builder.dir(root, "docs");
    builder.file(docs, "notes.txt", 4096);
    builder.file(root, "big.bin", 40960);
    builder.symlink(root, "link", "docs/notes.txt");
    let mut image = builder.build();

    let options = ScanOptions::default();
    let regions: [(usize, usize); 4] = [
        // The superblock lives at a fixed 1024 bytes in, whatever the block
        // size is, and everything else is found through it.
        (1024, 1024),
        (DESCRIPTOR_BLOCK as usize * BLOCK as usize, 64),
        // The first four inodes, which include the root.
        (
            INODE_TABLE_BLOCK as usize * BLOCK as usize,
            4 * INODE_SIZE as usize,
        ),
        (FIRST_DATA_BLOCK as usize * BLOCK as usize, 512),
    ];

    let mut tried = 0usize;
    for (start, length) in regions {
        for at in start..start + length {
            for bit in 0..8u8 {
                image[at] ^= 1 << bit;
                // Both answers are correct. The assertion is that we get one.
                if let Ok(mut fs) = Filesystem::open(image.as_slice()) {
                    let _ = build::build(
                        &mut fs,
                        &redirector(&[]),
                        format::ROOT_INODE,
                        &options,
                        |_: Progress| {},
                        || false,
                    );
                }
                image[at] ^= 1 << bit;
                tried += 1;
            }
        }
    }

    let expected: usize = regions.iter().map(|(_, length)| length * 8).sum();
    assert_eq!(tried, expected, "the sweep did not cover what it meant to");
    assert!(tried > 10_000, "only {tried} mutations");
}

/// And an image that stops early, which is what a copy interrupted halfway
/// looks like — and what happens when a sparse image is read past the last
/// block anybody wrote.
#[test]
fn no_truncation_can_panic_the_walk() {
    let mut builder = Builder::new();
    let root = builder.root();
    builder.file(root, "a.bin", 4096);
    let image = builder.build();

    let options = ScanOptions::default();
    let lengths = (0..2048)
        .chain((2048..image.len()).step_by(512))
        .chain([image.len()]);

    for length in lengths {
        if let Ok(mut fs) = Filesystem::open(&image[..length]) {
            let _ = build::build(
                &mut fs,
                &redirector(&[]),
                format::ROOT_INODE,
                &options,
                |_: Progress| {},
                || false,
            );
        }
    }
}

/// A disk image with no container around it.
///
/// `mkfs` on a file, `dd` of a partition, an image from a virtualiser that does
/// not write VHDX — none of those have a container, and every one of them is a
/// filesystem somebody may want measured without mounting anything and without
/// being root. The same bytes reached two ways have to produce the same tree,
/// because nothing about the filesystem changes: only how a byte is found.
mod without_a_container {
    use super::*;

    /// A small filesystem with something of every shape in it.
    fn image() -> Vec<u8> {
        let mut builder = Builder::new();
        let root = builder.root();
        let deep = builder.dir(root, "deep");
        builder.file(deep, "one.bin", 8192);
        builder.file(root, "top.txt", 10);
        builder.sparse(root, "sparse.bin", 1 << 20, BLOCK);
        builder.symlink(root, "link", "deep/one.bin");
        builder.build()
    }

    /// Two entries per node, so a mismatch names what differs rather than
    /// reporting that two numbers are not equal.
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
                    node.is_dir()
                )
            })
            .collect();
        rows.sort();
        rows
    }

    #[test]
    fn reads_the_same_as_the_same_bytes_in_memory() {
        let bytes = image();
        let dir = tempfile::tempdir().unwrap();
        // Deliberately not `.vhdx`, and deliberately not `.img` either: the
        // name is not what decides, and a test that used a familiar extension
        // would pass even if the extension were being consulted again.
        let path = dir.path().join("exported-disk");
        std::fs::write(&path, &bytes).unwrap();

        let from_file = crate::Scanner::new(ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        })
        .scan(&path)
        .expect("a bare filesystem in a file is an image");

        let from_memory = tree_of(&bytes, &ScanOptions::default());

        assert_eq!(
            shape(&from_file),
            shape(&from_memory),
            "the same bytes produced different trees through the two routes"
        );
        assert_eq!(from_file.total_allocated(), from_memory.total_allocated());
        assert_eq!(
            from_file.stats.strategy,
            crate::Strategy::DiskImage,
            "the tree has to say it came from the image"
        );
    }

    /// The ordinary walk must not be what answers. A file is not a directory,
    /// so a fallback here would be a refusal rather than a slow success — but
    /// the reason it must be checked is that a silent fallback is exactly how
    /// this would regress without anybody noticing.
    #[test]
    fn is_not_reached_unless_images_were_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exported-disk");
        std::fs::write(&path, image()).unwrap();

        match crate::Scanner::new(ScanOptions::default()).scan(&path) {
            Err(crate::ScanError::ImageNotRead(_)) => {}
            other => panic!("expected ImageNotRead, got {other:?}"),
        }
    }
}

/// A filesystem that does not start at the beginning of the disk.
///
/// `dd if=/dev/sda of=disk.img` is a file kosha could see nothing inside, while
/// `dd if=/dev/sda2` was one it read completely — the difference being a
/// megabyte of partition table in front. Both schemes are parsed in
/// [`crate::partition`] and tested there against tables built byte by byte;
/// these are about the join, which is the part that has a real filesystem on
/// one side and an offset on the other.
mod on_a_partitioned_disk {
    use super::*;
    use crate::partition::tests::{EFI_SYSTEM, LINUX_DATA, SECTOR, gpt, mbr_entry};
    use crate::tree::Unavailable;

    /// Where the filesystem is put, and where every real tool puts one: a
    /// megabyte in, which is 2048 sectors of 512 bytes.
    const START: usize = 2048 * SECTOR;

    fn filesystem() -> Vec<u8> {
        let mut builder = Builder::new();
        let root = builder.root();
        let deep = builder.dir(root, "deep");
        builder.file(deep, "one.bin", 8192);
        builder.file(root, "top.txt", 10);
        builder.build()
    }

    /// A disk holding that filesystem at `START`, with room to spare.
    fn disk_around(filesystem: &[u8]) -> Vec<u8> {
        let mut disk = vec![0u8; START + filesystem.len() + SECTOR];
        disk[START..START + filesystem.len()].copy_from_slice(filesystem);
        disk
    }

    fn scan_image(disk: &[u8], name: &str) -> Result<crate::SizeTree, crate::ScanError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
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
            .map(|id| format!("{} {}", tree.name_lossy(id), tree.node(id).allocated))
            .collect();
        rows.sort();
        rows
    }

    #[test]
    fn a_master_boot_record_leads_to_the_filesystem() {
        let filesystem = filesystem();
        let mut disk = disk_around(&filesystem);
        mbr_entry(
            &mut disk,
            0,
            0,
            0x83,
            2048,
            (filesystem.len() / SECTOR) as u32,
        );

        let tree = scan_image(&disk, "sda.img").expect("a partitioned disk is an image");
        assert_eq!(
            names(&tree),
            names(&tree_of(&filesystem, &ScanOptions::default()))
        );
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("ext4 · partition 1 of 1 (MBR)"),
            "a scan must say which partition it read"
        );
    }

    /// A partition holding something kosha does not read, and a root one it
    /// does. Picking the readable one is the whole job, and saying so is the
    /// rest of it.
    ///
    /// The unreadable partition here is *empty* — an entry in the table with
    /// nothing formatted behind it, which is what a disk half way through
    /// being set up looks like. It carries the EFI type code because that is
    /// the entry a real disk has in that position; an EFI system partition
    /// with a filesystem in it is read rather than skipped, and
    /// [`a_boot_partition_and_a_root_one_are_both_read`] is that case.
    #[test]
    fn a_guid_table_skips_the_partition_it_cannot_read() {
        let filesystem = filesystem();
        let mut disk = disk_around(&filesystem);
        let last = ((START + filesystem.len()) / SECTOR - 1) as u64;
        gpt(
            &mut disk,
            &[
                // An entry with nothing formatted behind it.
                (34, 2047, EFI_SYSTEM),
                (2048, last, LINUX_DATA),
            ],
        );

        let tree = scan_image(&disk, "sda.img").expect("a partitioned disk is an image");
        assert_eq!(
            names(&tree),
            names(&tree_of(&filesystem, &ScanOptions::default()))
        );
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("ext4 · partition 2 of 2 (GPT)"),
            "the unreadable partition is still counted in the total"
        );
    }

    /// **The shape of every disk a UEFI machine starts from**: a small FAT
    /// boot partition and a large root one, and now both are filesystems kosha
    /// reads. Which one a scan means is no longer decided by which one it
    /// happens to understand — so the rule that decides it, and the line that
    /// records the decision, are the whole of the answer.
    #[test]
    fn a_boot_partition_and_a_root_one_are_both_read() {
        let filesystem = filesystem();
        let boot = crate::fat::tests::fixture(crate::fat::format::Flavour::Fat16);

        let boot_start = 2048usize;
        let boot_sectors = boot.len() / SECTOR;
        let root_start = boot_start + boot_sectors + 1;
        // On a real disk the root partition is larger by a factor of hundreds.
        let root_sectors = (filesystem.len() / SECTOR).max(boot_sectors * 2);

        let mut disk = vec![0u8; (root_start + root_sectors + 1) * SECTOR];
        let at = boot_start * SECTOR;
        disk[at..at + boot.len()].copy_from_slice(&boot);
        let at = root_start * SECTOR;
        disk[at..at + filesystem.len()].copy_from_slice(&filesystem);
        gpt(
            &mut disk,
            &[
                (
                    boot_start as u64,
                    (boot_start + boot_sectors - 1) as u64,
                    EFI_SYSTEM,
                ),
                (
                    root_start as u64,
                    (root_start + root_sectors - 1) as u64,
                    LINUX_DATA,
                ),
            ],
        );

        let tree = scan_image(&disk, "sda.img").expect("a partitioned disk is an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("ext4 · partition 2 of 2 (GPT)"),
            "the root partition is the one a scan of the disk means"
        );
        assert_eq!(
            names(&tree),
            names(&tree_of(&filesystem, &ScanOptions::default()))
        );
    }

    /// Two filesystems, and the larger is taken. Right almost every time and
    /// wrong occasionally, which is exactly why the tree records it.
    #[test]
    fn the_larger_of_two_readable_partitions_is_taken_and_named() {
        let small = {
            let mut builder = Builder::new();
            let root = builder.root();
            builder.file(root, "little.bin", 4096);
            builder.build()
        };
        let large = filesystem();

        let mut disk = vec![0u8; START + small.len() + large.len() + SECTOR];
        disk[START..START + small.len()].copy_from_slice(&small);
        let second = START + small.len();
        disk[second..second + large.len()].copy_from_slice(&large);

        mbr_entry(&mut disk, 0, 0, 0x83, 2048, (small.len() / SECTOR) as u32);
        mbr_entry(
            &mut disk,
            0,
            1,
            0x83,
            (second / SECTOR) as u32,
            (large.len() / SECTOR) as u32,
        );

        let tree = scan_image(&disk, "sda.img").expect("a partitioned disk is an image");
        assert_eq!(
            tree.stats.image.as_deref(),
            Some("ext4 · partition 2 of 2 (MBR)")
        );
        assert!(
            (0..tree.len())
                .map(crate::NodeId::from_index)
                .any(|id| tree.name_lossy(id) == "top.txt"),
            "the larger filesystem is the one that should have been read"
        );
    }

    /// A partition is a boundary, not a suggestion. A reader that wandered off
    /// the end of one and into the next would build a tree that is wrong rather
    /// than raise an error that is right — and a truncated image, which is what
    /// a copy that ran out of disk leaves, is the ordinary way to find out.
    #[test]
    fn a_table_that_claims_more_disk_than_the_file_holds_is_clamped() {
        let filesystem = filesystem();
        let mut disk = disk_around(&filesystem);
        // Claiming a gigabyte where there is barely a megabyte.
        mbr_entry(&mut disk, 0, 0, 0x83, 2048, 2_000_000);

        let tree = scan_image(&disk, "cut-short.img").expect("still readable as far as it goes");
        assert_eq!(
            names(&tree),
            names(&tree_of(&filesystem, &ScanOptions::default()))
        );
    }

    /// A disk with a table and nothing kosha reads in any of it is still a
    /// disk. Saying "there is no filesystem in this file" would be wrong twice:
    /// there are filesystems, and the file is plainly an image.
    #[test]
    fn a_disk_of_filesystems_kosha_cannot_read_says_so() {
        let mut disk = vec![0u8; START + SECTOR];
        mbr_entry(&mut disk, 0, 0, 0x07, 2048, 1024);

        match scan_image(&disk, "windows.img") {
            Err(crate::ScanError::Image { reason, .. }) => {
                assert_eq!(reason, Unavailable::UnknownFilesystem)
            }
            other => panic!("expected UnknownFilesystem, got {other:?}"),
        }
    }
}
