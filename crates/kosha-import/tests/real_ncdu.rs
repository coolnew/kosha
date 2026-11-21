// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The importer against real ncdu 2.9.1 output.
//!
//! `fixtures/real-ncdu-2.9.1.json`, `real-ncdu-2.9.1-ext.json` and
//! `real-ncdu-2.9.1.json.zst` were written by the official ncdu 2.9.1 static
//! x86_64 binary (`ncdu -o`, `ncdu -e -o`, `ncdu -c -o`) on a small synthetic
//! tree on Linux; nothing in them comes from a real home directory. The tree:
//!
//! ```text
//! tree/
//!   a.txt        5000 bytes of 'a'
//!   hard.txt     hard link to a.txt
//!   link.txt     symlink to a.txt
//!   café.txt     one byte, a non-ASCII name
//!   zero.empty   empty file
//!   empty/       empty directory
//!   sub/b.bin    70000 bytes
//!   sub/deep/c.dat  1200 bytes
//! ```
//!
//! No path or file name here is opened; the tests only read the fixtures
//! through `include_bytes!`, so they hold on every platform.

use std::ffi::OsStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kosha_core::{Flags, NodeId, SizeTree, Strategy};
use kosha_import::{Compression, Imported, Limits, import_ncdu};

const PLAIN: &[u8] = include_bytes!("fixtures/real-ncdu-2.9.1.json");
const EXT: &[u8] = include_bytes!("fixtures/real-ncdu-2.9.1-ext.json");
const ZST: &[u8] = include_bytes!("fixtures/real-ncdu-2.9.1.json.zst");

/// The export's own `timestamp`, in seconds since the epoch.
const EXPORTED: u64 = 1_795_296_603;

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_795_400_000)
}

fn ok(bytes: &[u8], name: &str) -> Imported {
    match import_ncdu(bytes, name, "", now(), &Limits::default()) {
        Ok(imported) => imported,
        Err(error) => panic!("refused {name}: {error}"),
    }
}

fn find(tree: &SizeTree, path: &[&str]) -> NodeId {
    let mut at = tree.root();
    for part in path {
        at = tree
            .child_named(at, OsStr::new(part))
            .unwrap_or_else(|| panic!("no {part}"));
    }
    at
}

/// Every entry as (depth-first path, apparent, allocated), in tree order.
fn listing(tree: &SizeTree) -> Vec<(String, u64, u64)> {
    fn walk(tree: &SizeTree, id: NodeId, prefix: &str, out: &mut Vec<(String, u64, u64)>) {
        let node = tree.node(id);
        let path = format!("{prefix}/{}", tree.name_lossy(id));
        out.push((path.clone(), node.logical, node.allocated));
        for child in tree.children(id) {
            walk(tree, child, &path, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, tree.root(), "", &mut out);
    out
}

fn check_tree(imported: &Imported) {
    let tree = &imported.tree;
    let stats = tree.stats();

    assert_eq!(stats.strategy, Strategy::Imported);
    assert_eq!(tree.name_lossy(tree.root()), "tree");
    // The root, five files, one link and three directories below it.
    assert_eq!(tree.len(), 11);
    assert_eq!(stats.dirs, 4);
    assert_eq!(stats.files, 7);
    assert_eq!(stats.max_depth, 3);

    // Apparent: directories' own sizes are ignored and the second name of the
    // hard-linked pair contributes nothing: 0 + 1 + 5 + 5000 + 70000 + 1200.
    assert_eq!(tree.total_logical(), 76_206);
    // On disk: ncdu 2.9.1 writes no dsize for directories, symlinks or empty
    // files: 4096 + 8192 + 73728 + 4096.
    assert_eq!(tree.total_allocated(), 90_112);

    let top: Vec<_> = tree
        .children(tree.root())
        .map(|id| tree.name_lossy(id).into_owned())
        .collect();
    assert_eq!(
        top,
        [
            "a.txt",
            "café.txt",
            "empty",
            "hard.txt",
            "link.txt",
            "sub",
            "zero.empty"
        ]
    );

    // zero.empty has no size fields at all and imports as zero.
    let zero = tree.node(find(tree, &["zero.empty"]));
    assert_eq!((zero.logical, zero.allocated), (0, 0));
    assert!(!zero.is_dir());

    let cafe = tree.node(find(tree, &["café.txt"]));
    assert_eq!((cafe.logical, cafe.allocated), (1, 4096));

    // hard.txt is met first in the file and owns the bytes; a.txt is the
    // duplicate. Same inode, nlink 2, counted once.
    let hard = tree.node(find(tree, &["hard.txt"]));
    let a = tree.node(find(tree, &["a.txt"]));
    assert_eq!((hard.logical, hard.allocated), (5000, 8192));
    assert!(!hard.flags.contains(Flags::HARDLINK_DUP));
    assert!(a.flags.contains(Flags::HARDLINK_DUP));
    assert_eq!((a.logical, a.allocated), (0, 0));
    assert_eq!(stats.hardlinks_deduped, 1);
    assert!(stats.dedup_hardlinks);

    // link.txt is not a regular file: its five bytes are the target's length.
    let link = tree.node(find(tree, &["link.txt"]));
    assert_eq!((link.logical, link.allocated), (5, 0));

    // empty / sub / deep nesting.
    let empty = find(tree, &["empty"]);
    assert!(tree.node(empty).is_dir());
    assert_eq!(tree.children(empty).count(), 0);
    let sub = find(tree, &["sub"]);
    assert!(tree.node(sub).is_dir());
    let b = tree.node(find(tree, &["sub", "b.bin"]));
    assert_eq!((b.logical, b.allocated), (70_000, 73_728));
    let deep = find(tree, &["sub", "deep"]);
    assert!(tree.node(deep).is_dir());
    let c = tree.node(find(tree, &["sub", "deep", "c.dat"]));
    assert_eq!((c.logical, c.allocated), (1200, 4096));

    assert!(!stats.partial, "nothing was excluded or unreadable");
    assert_eq!(stats.unreadable, 0);

    let meta = &imported.meta;
    assert_eq!(meta.strategy, "Imported");
    let import = meta.import.as_ref().unwrap();
    assert_eq!(import.format, "ncdu-json");
    assert_eq!(import.format_version, "1.2");
    assert_eq!(import.tool, "ncdu 2.9.1");
    assert_eq!(import.observed_basis, "tool-timestamp");
    assert_eq!(import.imported_at, 1_795_400_000);
    assert_eq!(import.duplicate_paths, 0);
    assert_eq!(import.unknown_size, 0);
    assert_eq!(import.excluded_by_source, 0);
}

#[test]
fn real_plain_export_imports_with_the_expected_shape_and_totals() {
    let imported = ok(PLAIN, "real-ncdu-2.9.1.json");
    assert_eq!(imported.compression, Compression::None);
    check_tree(&imported);

    let import = imported.meta.import.as_ref().unwrap();
    assert_eq!(imported.meta.taken, EXPORTED);
    assert_eq!(import.observed, Some(EXPORTED));
    assert_eq!(import.source_name, "real-ncdu-2.9.1.json");
    // dsize is written without -e; modification times are not.
    assert!(import.allocated_known);
    assert!(!import.mtime_known);
    let tree = &imported.tree;
    assert_eq!(tree.node(find(tree, &["sub", "b.bin"])).mtime, 0);
}

#[test]
fn real_extended_export_carries_modification_times_and_modes() {
    let imported = ok(EXT, "real-ncdu-2.9.1-ext.json");
    check_tree(&imported);

    let import = imported.meta.import.as_ref().unwrap();
    assert_eq!(imported.meta.taken, EXPORTED);
    assert!(import.allocated_known);
    assert!(import.mtime_known);

    let tree = &imported.tree;
    let seconds = |path: &[&str]| tree.node(find(tree, path)).mtime;
    assert_eq!(u64::from(seconds(&[])), EXPORTED);
    assert_eq!(u64::from(seconds(&["zero.empty"])), EXPORTED);
    assert_eq!(u64::from(seconds(&["sub", "deep", "c.dat"])), EXPORTED);
    // The empty directory was made two seconds earlier.
    assert_eq!(u64::from(seconds(&["empty"])), EXPORTED - 2);
    // With a mode in the export, the symlink is recognised as one.
    assert!(
        tree.node(find(tree, &["link.txt"]))
            .flags
            .contains(Flags::SYMLINK)
    );
}

#[test]
fn real_zstd_export_decodes_to_the_same_tree_as_the_plain_one() {
    let plain = ok(PLAIN, "real-ncdu-2.9.1.json");
    let zstd = ok(ZST, "real-ncdu-2.9.1.json.zst");
    assert_eq!(zstd.compression, Compression::Zstd);
    assert!(zstd.bytes_decoded > zstd.bytes_read);
    check_tree(&zstd);

    assert_eq!(zstd.tree.len(), plain.tree.len());
    assert_eq!(zstd.tree.total_logical(), plain.tree.total_logical());
    assert_eq!(zstd.tree.total_allocated(), plain.tree.total_allocated());
    assert_eq!(listing(&zstd.tree), listing(&plain.tree));

    let provenance = zstd.meta.import.as_ref().unwrap();
    assert_eq!(provenance.source_name, "real-ncdu-2.9.1.json.zst");
    assert_eq!(provenance.tool, "ncdu 2.9.1");
    assert!(provenance.allocated_known);
    assert!(!provenance.mtime_known);
}
