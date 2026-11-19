// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Scanner behaviour against a real filesystem.
//!
//! These assert the structural invariants the rest of the workspace relies on —
//! contiguous children, breadth-first ordering, correct roll-up — not just
//! "the number looked right".

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kosha_core::{Flags, NodeId, ScanError, ScanOptions, Scanner, SizeTree};

/// Builds:
///
/// ```text
/// root/
///   a/
///     deep/
///       big.bin      4096 bytes
///     small.txt        10 bytes
///   b/
///     mid.bin        1024 bytes
///   top.txt             5 bytes
/// ```
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("a/deep")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::write(root.join("a/deep/big.bin"), vec![0u8; 4096]).unwrap();
    fs::write(root.join("a/small.txt"), vec![b'x'; 10]).unwrap();
    fs::write(root.join("b/mid.bin"), vec![0u8; 1024]).unwrap();
    fs::write(root.join("top.txt"), b"hello").unwrap();
    dir
}

fn scan(root: &Path) -> SizeTree {
    Scanner::new(ScanOptions::default())
        .scan(root)
        .expect("scan")
}

fn child(tree: &SizeTree, parent: NodeId, name: &str) -> NodeId {
    tree.child_named(parent, OsStr::new(name))
        .unwrap_or_else(|| panic!("no child named {name}"))
}

#[test]
fn counts_every_entry() {
    let dir = fixture();
    let tree = scan(dir.path());

    // root + a + a/deep + a/deep/big.bin + a/small.txt + b + b/mid.bin + top.txt
    assert_eq!(tree.len(), 8);
    assert_eq!(tree.stats().dirs, 4);
    assert_eq!(tree.stats().files, 4);
    assert_eq!(tree.stats().entries, 8);
    assert!(!tree.is_partial(), "a temp dir must be fully readable");
}

#[test]
fn logical_sizes_roll_up_to_the_root() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(tree.total_logical(), 4096 + 10 + 1024 + 5);

    let a = child(&tree, tree.root(), "a");
    assert_eq!(tree.node(a).logical, 4096 + 10);

    let deep = child(&tree, a, "deep");
    assert_eq!(tree.node(deep).logical, 4096);
}

#[test]
fn subtree_file_counts_exclude_directories() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(tree.node(tree.root()).subtree_files, 4);

    let a = child(&tree, tree.root(), "a");
    assert_eq!(tree.node(a).subtree_files, 2);

    let top = child(&tree, tree.root(), "top.txt");
    assert_eq!(tree.node(top).subtree_files, 1, "a file counts as itself");
}

#[test]
fn a_directory_contributes_no_apparent_bytes_of_its_own() {
    // Unix gives a directory a real `st_size` — 60 bytes on tmpfs, 4096 on
    // ext4 — and NTFS gives it zero. Counting it would make the same tree
    // measure differently on the two platforms, so the scanner zeroes it and
    // this pins that rather than leaving it to whichever filesystem runs.
    //
    // Found by running the suite on Linux for the first time: the roll-up came
    // to 5335 against an expected 5135, which is exactly the 80 + 60 + 60 that
    // tmpfs charges for the three subdirectories.
    let dir = fixture();
    let tree = scan(dir.path());

    // After aggregation a directory's `logical` is its children's sum, so the
    // leaf directory is where an unzeroed own-size would show up first.
    let a = child(&tree, tree.root(), "a");
    let deep = child(&tree, a, "deep");
    assert_eq!(
        tree.node(deep).logical,
        4096,
        "a leaf directory reported more than the file inside it"
    );
    assert_eq!(tree.total_logical(), 4096 + 10 + 1024 + 5);
}

/// `--exact` has to return an *allocation*, not the apparent size again.
///
/// It returned the apparent size for every ordinary file on Windows, because
/// `GetCompressedFileSizeW` is documented to do exactly that when the file is
/// neither compressed nor sparse. So the option advertised as the precise one
/// was further from the truth than the cluster estimate it replaced, and the
/// only way to notice was to compare an export against the same tree measured
/// by Linux — where `st_blocks` had it right all along.
///
/// Written for both platforms deliberately. The claim is not Windows-specific:
/// a file of a few hundred bytes occupies a whole allocation unit, or none at
/// all where the filesystem can store it inline, and either way "on disk" and
/// "apparent" are different questions.
#[test]
fn the_exact_size_is_an_allocation_not_the_apparent_size_again() {
    let dir = tempfile::tempdir().unwrap();
    // Deliberately not a multiple of any plausible cluster size, so an
    // allocation and an apparent size cannot coincide by luck.
    const ODD: usize = 127_387;
    fs::write(dir.path().join("odd.bin"), vec![0u8; ODD]).unwrap();

    let tree = Scanner::new(ScanOptions {
        exact_allocated: true,
        ..Default::default()
    })
    .scan(dir.path())
    .unwrap();

    let file = child(&tree, tree.root(), "odd.bin");
    let allocated = tree.node(file).allocated;

    assert_eq!(tree.node(file).logical, ODD as u64);
    assert_ne!(
        allocated, ODD as u64,
        "the exact query handed back the apparent size"
    );
    assert!(
        allocated > ODD as u64,
        "an ordinary file occupies whole allocation units: {allocated} for {ODD} bytes"
    );
    assert_eq!(
        allocated % 512,
        0,
        "an allocation is a whole number of blocks: {allocated}"
    );
}

/// Windows stops at 260 characters unless you ask it not to, and `node_modules`
/// blows past that routinely. A disk tool that quietly stops there reports a
/// smaller number than the truth, which is the worst way for this app to be
/// wrong: it does not look like a failure, it looks like a smaller directory.
///
/// Built with the verbatim prefix so that creating the fixture is not the thing
/// under test, then scanned by the ordinary path, which is what a user has.
#[cfg(windows)]
#[test]
fn a_tree_deeper_than_max_path_is_scanned_whole() {
    const SEGMENT: &str = "a-directory-with-a-longish-name";
    const PAYLOAD: usize = 4096;

    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    let mut planted = 0u64;

    // Past 260 by a wide margin, so this is not an off-by-one at the boundary.
    while deep.as_os_str().len() < 400 {
        deep.push(SEGMENT);
        let verbatim = std::path::PathBuf::from(format!("\\\\?\\{}", deep.display()));
        fs::create_dir_all(&verbatim).unwrap();
        fs::write(verbatim.join("payload.bin"), vec![0u8; PAYLOAD]).unwrap();
        planted += 1;
    }
    assert!(planted > 4, "the fixture did not get deep enough");

    let tree = scan(dir.path());

    assert_eq!(
        tree.total_logical(),
        planted * PAYLOAD as u64,
        "the scan stopped short of the bottom of the tree"
    );
    assert_eq!(tree.stats().files as u64, planted);
    assert!(
        !tree.is_partial(),
        "a path over 260 characters was reported as unreadable"
    );
    assert_eq!(tree.stats().unreadable, 0);

    // Cleanup needs the prefix too, or the directory outlives the test.
    let _ = fs::remove_dir_all(std::path::PathBuf::from(format!(
        "\\\\?\\{}",
        dir.path().display()
    )));
}

/// A OneDrive placeholder reports its full length while holding no bytes here,
/// and reading one downloads it. The scanner has to mark that, because three
/// separate decisions hang off it: do not cluster-round its size, do not hash
/// it looking for duplicates, and do not treat deleting it as freeing space.
///
/// `attrib +O` sets `FILE_ATTRIBUTE_OFFLINE`, the same bit a cloud placeholder
/// carries, so the flag can be exercised without OneDrive.
#[cfg(windows)]
#[test]
fn a_file_whose_data_is_elsewhere_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("local.bin");
    let away = dir.path().join("offline.bin");
    fs::write(&here, vec![7u8; 64 * 1024]).unwrap();
    fs::write(&away, vec![7u8; 64 * 1024]).unwrap();

    let marked = std::process::Command::new("attrib")
        .arg("+O")
        .arg(&away)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !marked {
        return; // `attrib` unavailable; nothing to assert.
    }

    let tree = scan(dir.path());
    let offline = child(&tree, tree.root(), "offline.bin");
    let local = child(&tree, tree.root(), "local.bin");

    assert!(
        tree.node(offline).flags.contains(Flags::OFFLINE),
        "a file marked offline was reported as ordinary"
    );
    assert!(!tree.node(local).flags.contains(Flags::OFFLINE));

    // Its length is an upper bound, not a measurement, so it must not be
    // rounded up to a cluster the way an ordinary file is.
    assert_eq!(tree.node(offline).allocated, tree.node(offline).logical);

    // Still a file worth reporting. It is a reparse point underneath, and
    // treating it as a symlink would drop it from the tree entirely.
    assert!(!tree.node(offline).flags.contains(Flags::SYMLINK));
    assert!(!tree.node(offline).is_dir());
}

#[test]
fn allocated_is_at_least_logical_for_ordinary_files() {
    let dir = fixture();
    let tree = scan(dir.path());

    for node in tree.nodes() {
        if node.is_dir() || node.flags.contains(Flags::SPARSE) {
            continue;
        }
        assert!(
            node.allocated >= node.logical,
            "an ordinary file cannot occupy less than its length: {node:?}"
        );
    }
}

#[test]
fn children_occupy_a_contiguous_range() {
    let dir = fixture();
    let tree = scan(dir.path());

    // This is the invariant the whole data model is built on: a directory's
    // children are a slice, so `first_child + i` addresses the i-th child.
    for (index, node) in tree.nodes().iter().enumerate() {
        for offset in 0..node.child_count {
            let child_index = node.first_child + offset;
            assert_eq!(
                tree.nodes()[child_index as usize].parent as usize,
                index,
                "child {child_index} of node {index} points elsewhere"
            );
        }
    }
}

#[test]
fn arena_is_in_breadth_first_order() {
    let dir = fixture();
    let tree = scan(dir.path());

    // Aggregation is a single reverse pass, which is only correct if every
    // child sits at a higher index than its parent.
    for (index, node) in tree.nodes().iter().enumerate().skip(1) {
        assert!(
            (node.parent as usize) < index,
            "node {index} has parent {} at or after it",
            node.parent
        );
    }
    // Depth is monotonically non-decreasing across the arena.
    let mut depth = 0;
    for node in tree.nodes() {
        assert!(node.depth >= depth);
        depth = node.depth;
    }
}

#[test]
fn paths_round_trip_through_the_arena() {
    let dir = fixture();
    let tree = scan(dir.path());

    let a = child(&tree, tree.root(), "a");
    let deep = child(&tree, a, "deep");
    let big = child(&tree, deep, "big.bin");

    let path = tree.path_of(big);
    assert!(path.ends_with("a/deep/big.bin") || path.ends_with(r"a\deep\big.bin"));
    assert!(path.exists(), "reconstructed path must name a real file");

    assert_eq!(tree.find(&path), Some(big), "and must resolve back");
    assert_eq!(tree.path_of(tree.root()), tree.root_path());
}

#[test]
fn children_sort_largest_first() {
    let dir = fixture();
    let tree = scan(dir.path());

    let ordered = tree.children_by_size(tree.root());
    let sizes: Vec<u64> = ordered.iter().map(|&id| tree.node(id).allocated).collect();
    assert!(
        sizes.windows(2).all(|w| w[0] >= w[1]),
        "not descending: {sizes:?}"
    );
    // `a` holds 4 KiB, `b` holds 1 KiB, so `a` must lead.
    assert_eq!(tree.name(ordered[0]), OsStr::new("a"));
}

#[test]
fn scans_are_deterministic() {
    let dir = fixture();
    let first = scan(dir.path());
    let second = scan(dir.path());

    assert_eq!(first.len(), second.len());
    for id in 0..first.len() {
        let id = NodeId::from_index(id);
        assert_eq!(first.name(id), second.name(id));
        assert_eq!(first.node(id).logical, second.node(id).logical);
        assert_eq!(first.node(id).parent, second.node(id).parent);
        assert_eq!(first.node(id).first_child, second.node(id).first_child);
    }
}

#[test]
fn max_depth_stops_the_walk() {
    let dir = fixture();
    let options = ScanOptions {
        max_depth: Some(1),
        ..Default::default()
    };
    let tree = Scanner::new(options).scan(dir.path()).unwrap();

    // Root plus its three direct children, and nothing below them.
    assert_eq!(tree.len(), 4);
    let a = child(&tree, tree.root(), "a");
    assert_eq!(tree.node(a).child_count, 0);
}

#[test]
fn cancellation_yields_a_valid_partial_tree() {
    let dir = fixture();
    let flag = Arc::new(AtomicBool::new(true)); // already cancelled
    let tree = Scanner::new(ScanOptions::default())
        .with_cancel(Arc::clone(&flag))
        .scan(dir.path())
        .expect("cancellation is not an error");

    assert!(tree.stats().cancelled);
    assert!(
        tree.is_partial(),
        "a cancelled scan must not claim completeness"
    );
    assert_eq!(tree.len(), 1, "stopped before reading the first level");

    flag.store(false, Ordering::Relaxed);
}

/// Asking for the MFT and cancelling at the same time.
///
/// Two paths meet here and they disagree about what a cancel is. The walk stops
/// at a level boundary and hands back the valid prefix it built, so a cancel is
/// not an error. The MFT strategy has nothing to hand back until the whole
/// table is read, so a cancel during that read *is* `ScanError::Cancelled` —
/// and `Scanner::scan` must return it rather than treating it as a reason to
/// fall back and start the slow walk the user just interrupted.
///
/// What this can check anywhere is the other half: where the MFT is not
/// available at all — not Windows, not NTFS, or not elevated, which is the
/// usual case — the request falls back to the walk, and cancelling that still
/// yields a tree rather than an error.
#[test]
fn asking_for_the_mft_does_not_change_what_a_cancel_means() {
    let dir = fixture();
    let flag = Arc::new(AtomicBool::new(true)); // already cancelled
    let outcome = Scanner::new(ScanOptions {
        use_mft: true,
        ..ScanOptions::default()
    })
    .with_cancel(Arc::clone(&flag))
    .scan(dir.path());

    match outcome {
        Ok(tree) => {
            // The fallback ran, and a cancelled walk is a partial tree.
            assert!(tree.stats().cancelled);
            assert!(tree.is_partial());
        }
        // The MFT strategy was available and was interrupted, which only
        // happens on an elevated Windows run against an NTFS volume. What
        // matters is that it came back as a cancel at all, rather than as a
        // fallback that quietly ran the walk the user had just stopped — and
        // arriving in this arm is that. There is nothing further to assert:
        // `cfg!(windows)` is a constant, so a check on it would test the
        // compiler rather than the code.
        Err(ScanError::Cancelled) => {}
        Err(other) => panic!("a cancel became an error: {other}"),
    }

    flag.store(false, Ordering::Relaxed);
}

#[test]
fn empty_directory_scans_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let tree = scan(dir.path());

    assert_eq!(tree.len(), 1);
    assert_eq!(tree.total_logical(), 0);
    assert_eq!(tree.node(tree.root()).child_count, 0);
    assert!(tree.children(tree.root()).next().is_none());
}

#[test]
fn a_file_is_not_a_valid_scan_root() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lonely.txt");
    fs::write(&file, b"x").unwrap();

    match Scanner::new(ScanOptions::default()).scan(&file) {
        Err(ScanError::NotADirectory(_)) => {}
        other => panic!("expected NotADirectory, got {other:?}"),
    }
}

/// A disk image *is* a file, so "not a directory" is the answer somebody gets
/// for pointing kosha at exactly the file it was taught to read. It is true of
/// every disk image ever made and tells nobody anything, and it is the one
/// refusal where the reason is both knowable and was being thrown away.
mod a_file_that_is_a_disk_image {
    use super::*;

    /// The first eight bytes of every VHDX, and nothing else — the signature
    /// is what is checked, so a stub is enough to be recognised as one.
    fn vhdx_stub(at: &Path) {
        fs::write(at, b"vhdxfile").unwrap();
    }

    /// An NTFS volume says so in its boot sector, eight bytes in.
    fn ntfs_stub(at: &Path) {
        let mut bytes = vec![0u8; 512];
        bytes[0x03..0x0B].copy_from_slice(b"NTFS    ");
        fs::write(at, bytes).unwrap();
    }

    /// An ext4 superblock sits 1024 bytes in and carries its magic 0x38
    /// further. Nothing else here is a filesystem, and nothing else needs to
    /// be: this is about what kosha *says*, not what it can read.
    fn raw_ext4_stub(at: &Path) {
        let mut bytes = vec![0u8; 2048];
        bytes[1024 + 0x38] = 0x53;
        bytes[1024 + 0x39] = 0xEF;
        fs::write(at, bytes).unwrap();
    }

    /// A whole disk: a partition table, and **nothing recognisable at offset
    /// zero**, which is what `dd if=/dev/sda` produces as against
    /// `dd if=/dev/sda2`. Every other check here looks at the front of the file
    /// and finds boot code, so a disk image kosha reads completely with
    /// `--image` was turned away as "not a directory" without it.
    ///
    /// One entry, type 0x83, starting a megabyte in. The table is read by the
    /// same parser the image reader uses, and a table with no usable entry in it
    /// is not a table — so a file with a stray 0xAA55 at the end of its first
    /// sector is still not a disk.
    fn partitioned_stub(at: &Path) {
        let mut bytes = vec![0u8; 4096];
        let entry = 0x1BE;
        bytes[entry + 4] = 0x83;
        bytes[entry + 8..entry + 12].copy_from_slice(&2048u32.to_le_bytes());
        bytes[entry + 12..entry + 16].copy_from_slice(&(64u32 * 2048).to_le_bytes());
        bytes[510] = 0x55;
        bytes[511] = 0xAA;
        fs::write(at, bytes).unwrap();
    }

    /// exFAT shares FAT's first three bytes and nothing after them, and is
    /// recognised by a *different branch* of the same parser — so a stub of its
    /// own, rather than trusting that `d.fat` covers it. Assuming two things
    /// share a path because they share a function is how two fixtures in the UDF
    /// reader came to pass without testing anything.
    ///
    /// A 64 MiB volume: 512-byte sectors, 8 sectors to a cluster, one table of
    /// 128 sectors at sector 128, and a heap of 16,000 clusters after it. The
    /// numbers have to agree with each other, because the parser checks that
    /// everything the boot sector names fits inside the volume it names.
    fn exfat_stub(at: &Path) {
        let mut bytes = vec![0u8; 512];
        bytes[0x03..0x0B].copy_from_slice(b"EXFAT   ");
        // 0x0B to 0x40 stay zero: the field exFAT uses to make anything that
        // reads a BPB refuse the volume rather than read a sector size of
        // nothing.
        bytes[0x48..0x50].copy_from_slice(&131_072u64.to_le_bytes()); // volume sectors
        bytes[0x50..0x54].copy_from_slice(&128u32.to_le_bytes()); // table at
        bytes[0x54..0x58].copy_from_slice(&128u32.to_le_bytes()); // table length
        bytes[0x58..0x5C].copy_from_slice(&256u32.to_le_bytes()); // heap at
        bytes[0x5C..0x60].copy_from_slice(&16_000u32.to_le_bytes()); // clusters
        bytes[0x60..0x64].copy_from_slice(&2u32.to_le_bytes()); // the root's
        bytes[0x6C] = 9; // 512-byte sectors
        bytes[0x6D] = 3; // eight sectors to a cluster
        bytes[0x6E] = 1; // one table
        // Both formats end the sector with it, and it is the one byte pair that
        // says somebody meant this to be bootable rather than that it happens to
        // start with plausible numbers.
        bytes[0x1FE] = 0x55;
        bytes[0x1FF] = 0xAA;
        fs::write(at, bytes).unwrap();
    }

    /// XFS says so in the first four bytes of its superblock, which is the
    /// front of the volume.
    fn xfs_stub(at: &Path) {
        let mut bytes = vec![0u8; 512];
        bytes[..4].copy_from_slice(b"XFSB");
        fs::write(at, bytes).unwrap();
    }

    /// An ISO 9660 disc says so thirty-two kilobytes in, because the sixteen
    /// blocks in front of that belong to whoever wrote the disc.
    fn iso_stub(at: &Path) {
        let mut bytes = vec![0u8; 17 * 2048];
        bytes[16 * 2048] = 1;
        bytes[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
        bytes[16 * 2048 + 6] = 1;
        fs::write(at, bytes).unwrap();
    }

    /// A UDF volume announces itself in a *list* of headers starting at the same
    /// block, and a disc carrying ISO 9660 too pushes the list past its own three
    /// descriptors. So this stub puts the announcement where a bridge disc has
    /// it — the fourth block of the sequence — rather than where a pure volume
    /// does, because the fixed-offset version of this check would pass on the
    /// easy one and fail on the shape a Windows installer has.
    fn udf_stub(at: &Path) {
        let mut bytes = vec![0u8; 24 * 2048];
        for (step, magic) in [b"BEA01", b"NSR03", b"TEA01"].into_iter().enumerate() {
            let block = (19 + step) * 2048;
            bytes[block + 1..block + 6].copy_from_slice(magic);
            bytes[block + 6] = 1;
        }
        fs::write(at, bytes).unwrap();
    }

    /// FAT is the one with no magic number: it is recognised by a boot sector
    /// whose numbers agree with each other, which is why this stub has to be a
    /// real one rather than a string in the right place.
    ///
    /// A megabyte at 512 bytes to a sector, one sector to a cluster, with a
    /// root of 512 entries — which works out at 200 clusters, and 200 clusters
    /// is what makes a volume FAT12.
    fn fat_stub(at: &Path) {
        let mut bytes = vec![0u8; 512];
        bytes[0] = 0xEB;
        bytes[1] = 0x58;
        bytes[2] = 0x90;
        bytes[3..11].copy_from_slice(b"MSDOS5.0");
        bytes[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        bytes[0x0D] = 1; // sectors to a cluster
        bytes[0x0E..0x10].copy_from_slice(&1u16.to_le_bytes()); // reserved
        bytes[0x10] = 2; // two tables
        bytes[0x11..0x13].copy_from_slice(&512u16.to_le_bytes()); // root entries
        bytes[0x13..0x15].copy_from_slice(&235u16.to_le_bytes()); // total sectors
        bytes[0x15] = 0xF8; // a fixed disk
        bytes[0x16..0x18].copy_from_slice(&1u16.to_le_bytes()); // a one-sector table
        bytes[0x1FE] = 0x55;
        bytes[0x1FF] = 0xAA;
        fs::write(at, bytes).unwrap();
    }

    #[test]
    fn is_named_as_one_rather_than_refused_as_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("ext4.vhdx");
        vhdx_stub(&image);

        // Reading images is off by default, which is the case somebody hits
        // first: the answer has to be "this is an image" rather than "this is
        // not a directory", because the thing to do about it is a flag.
        match Scanner::new(ScanOptions::default()).scan(&image) {
            Err(ScanError::ImageNotRead(path)) => {
                assert_eq!(path, kosha_core::path::canonical(&image));
            }
            other => panic!("expected ImageNotRead, got {other:?}"),
        }
    }

    /// The extension is not the question, in either direction. A name is not
    /// evidence, and the message becomes an instruction — sending somebody to
    /// ask for images on a file that is not one is a step further from the
    /// truth, and refusing a real image because it is not called `.vhdx` would
    /// turn away the very thing `mkfs` on a file produces.
    #[test]
    fn is_recognised_by_its_signature_and_not_by_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let liar = dir.path().join("notes.txt");
        vhdx_stub(&liar);
        let impostor = dir.path().join("looks-like.vhdx");
        fs::write(&impostor, vec![0u8; 4096]).unwrap();

        assert!(
            matches!(
                Scanner::new(ScanOptions::default()).scan(&liar),
                Err(ScanError::ImageNotRead(_))
            ),
            "a VHDX called `notes.txt` is still a VHDX"
        );
        assert!(
            matches!(
                Scanner::new(ScanOptions::default()).scan(&impostor),
                Err(ScanError::NotADirectory(_))
            ),
            "four kilobytes of zeros called `.vhdx` is not a disk image"
        );
    }

    /// **The list of what looks like an image has to grow with the list of
    /// what can be read**, or a file kosha could have measured is turned away
    /// with the wrong sentence. It has happened once already: NTFS was read
    /// from an image for a whole afternoon before anybody noticed that a
    /// volume handed over *without* asking for images still came back "not a
    /// directory".
    #[test]
    fn every_filesystem_that_can_be_read_is_recognised_as_an_image() {
        let dir = tempfile::tempdir().unwrap();
        for (name, write) in [
            ("a.vhdx", vhdx_stub as fn(&Path)),
            ("b.ntfs", ntfs_stub),
            ("c.ext4", raw_ext4_stub),
            ("d.fat", fat_stub),
            ("e.iso", iso_stub),
            ("f.udf", udf_stub),
            ("g.xfs", xfs_stub),
            ("h.exfat", exfat_stub),
            ("i.disk", partitioned_stub),
        ] {
            let image = dir.path().join(name);
            write(&image);
            match Scanner::new(ScanOptions::default()).scan(&image) {
                Err(ScanError::ImageNotRead(path)) => {
                    assert_eq!(path, kosha_core::path::canonical(&image));
                }
                other => panic!("{name} was not recognised as an image: {other:?}"),
            }
        }
    }

    /// An image with no container is still an image, and kosha reads it. What
    /// it must not do is refuse it as "not a directory", which is the sentence
    /// that tells nobody anything.
    ///
    /// **Every filesystem read from an image belongs in this list**, and the
    /// reason is that it went wrong once: NTFS was being read out of images
    /// while one handed over *without* asking for images still came back "not
    /// a directory", because the reader had learned about it and the sentence
    /// had not. Adding a row here is the cheapest way to notice.
    #[test]
    fn a_bare_filesystem_is_an_image_too() {
        let dir = tempfile::tempdir().unwrap();
        for (name, write) in [
            ("disk.img", raw_ext4_stub as fn(&Path)),
            ("windows.img", ntfs_stub),
            ("disc.img", iso_stub),
            ("volume.img", udf_stub),
        ] {
            let image = dir.path().join(name);
            write(&image);
            match Scanner::new(ScanOptions::default()).scan(&image) {
                Err(ScanError::ImageNotRead(path)) => {
                    assert_eq!(path, kosha_core::path::canonical(&image));
                }
                other => panic!("{name}: expected ImageNotRead, got {other:?}"),
            }
        }
    }

    /// An ordinary file has to keep the ordinary message. Sniffing two places
    /// in every failed scan root would be a poor trade for a sentence about
    /// disk images on somebody's mistyped `README.md`.
    #[test]
    fn an_ordinary_file_still_says_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("empty", Vec::new()),
            ("tiny", b"x".to_vec()),
            ("ordinary", vec![b'#'; 8192]),
        ] {
            let file = dir.path().join(name);
            fs::write(&file, &bytes).unwrap();
            match Scanner::new(ScanOptions::default()).scan(&file) {
                Err(ScanError::NotADirectory(_)) => {}
                other => panic!("{name}: expected NotADirectory, got {other:?}"),
            }
        }
    }

    /// With reading asked for, the strategy's own reason is what comes out —
    /// which is the whole point, since it is the reason that was being lost.
    #[test]
    fn says_what_stopped_the_read_when_reading_was_asked_for() {
        use kosha_core::Unavailable;

        let dir = tempfile::tempdir().unwrap();
        let options = ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        };

        // Four kilobytes of zeros: no container, and nothing inside it either.
        let impostor = dir.path().join("impostor.vhdx");
        fs::write(&impostor, vec![0u8; 4096]).unwrap();
        match Scanner::new(options).scan(&impostor) {
            Err(ScanError::Image {
                reason: Unavailable::NotAnImage,
                ..
            }) => {}
            other => panic!("expected NotAnImage, got {other:?}"),
        }

        // A VHDX signature and nothing behind it: the container itself is
        // unreadable, which is a different thing to go and look at.
        let broken = dir.path().join("broken.vhdx");
        vhdx_stub(&broken);
        match Scanner::new(options).scan(&broken) {
            Err(ScanError::Image {
                reason: Unavailable::ImageUnreadable,
                ..
            }) => {}
            other => panic!("expected ImageUnreadable, got {other:?}"),
        }
    }

    /// A file with a superblock's magic and nothing behind it is not a
    /// filesystem, and the sentence has to say that rather than blaming the
    /// container — there is no container, and demanding one would be advice
    /// that leads nowhere now that a bare image is read.
    #[test]
    fn a_magic_number_alone_is_not_a_filesystem() {
        use kosha_core::Unavailable;

        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("exported.img");
        raw_ext4_stub(&image);

        let options = ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        };
        match Scanner::new(options).scan(&image) {
            Err(ScanError::Image {
                reason: Unavailable::NotAnImage,
                ..
            }) => {}
            other => panic!("expected NotAnImage, got {other:?}"),
        }
    }
}

#[test]
fn missing_root_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");

    match Scanner::new(ScanOptions::default()).scan(&missing) {
        Err(ScanError::Root { .. }) => {}
        other => panic!("expected Root error, got {other:?}"),
    }
}

#[test]
fn progress_is_published_per_level() {
    let dir = fixture();
    let (tx, rx) = crossbeam_channel::bounded(64);
    let tree = Scanner::new(ScanOptions::default())
        .with_progress(tx)
        .scan(dir.path())
        .unwrap();

    let updates: Vec<_> = rx.try_iter().collect();
    assert!(!updates.is_empty(), "at least one level was completed");
    let last = updates.last().unwrap();
    assert_eq!(last.entries, tree.stats().entries);
    assert_eq!(last.dirs_pending, 0, "the final level queues nothing");
}

#[test]
fn a_broad_level_reports_progress_before_the_level_finishes() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..256 {
        fs::create_dir(dir.path().join(format!("dir-{n}"))).unwrap();
    }
    let (tx, rx) = crossbeam_channel::bounded(64);
    let tree = Scanner::new(ScanOptions::default())
        .with_progress(tx)
        .scan(dir.path())
        .unwrap();
    let updates: Vec<_> = rx.try_iter().collect();
    assert!(updates.iter().any(|p| p.depth == 1 && p.dirs_pending > 0));
    for pair in updates.windows(2) {
        assert!(pair[1].entries >= pair[0].entries);
        assert!(pair[1].dirs_scanned >= pair[0].dirs_scanned);
    }
    assert_eq!(
        updates.last().unwrap().dirs_scanned,
        Some(tree.stats().dirs)
    );
    assert_eq!(updates.last().unwrap().dirs_pending, 0);
}

/// A total that stopped at a ceiling is a floor, and has to say so.
///
/// `--depth 1` on a tree holding 884 KiB reported 4.00 KiB with no mark at all,
/// and `du --max-depth=1` — which is what people know — counts everything and
/// shows one level. Worse than a number on a screen: a snapshot of such a scan
/// called itself complete, and comparing a full scan against it reported every
/// directory below the ceiling as *new*, 880 KiB of growth that never happened.
#[test]
fn a_scan_that_stopped_at_its_depth_ceiling_says_its_totals_are_a_floor() {
    let dir = fixture();

    let whole = Scanner::new(ScanOptions::default())
        .scan(dir.path())
        .unwrap();
    assert!(!whole.is_partial(), "the fixture itself is unreadable");
    assert!(!whole.stats().depth_limited);

    let shallow = Scanner::new(ScanOptions {
        max_depth: Some(1),
        ..ScanOptions::default()
    })
    .scan(dir.path())
    .unwrap();

    assert!(
        shallow.total_allocated() < whole.total_allocated(),
        "the fixture has nothing below depth 1, so this proves nothing"
    );
    assert!(
        shallow.stats().depth_limited,
        "the ceiling was not recorded"
    );
    assert!(
        shallow.is_partial(),
        "a total that covers part of a tree is not marked as a floor"
    );
}

/// And a ceiling nothing reached leaves no mark.
///
/// The flag has to mean "directories were left unread", not "a limit was given":
/// a `--depth` larger than the tree is a complete scan, and marking it a floor
/// would put a tilde on a number that is exact.
#[test]
fn a_ceiling_deeper_than_the_tree_is_not_a_floor() {
    let dir = fixture();
    let tree = Scanner::new(ScanOptions {
        max_depth: Some(200),
        ..ScanOptions::default()
    })
    .scan(dir.path())
    .unwrap();

    assert!(!tree.stats().depth_limited, "{:?}", tree.stats());
    assert!(!tree.is_partial());
    assert_eq!(
        tree.total_allocated(),
        Scanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap()
            .total_allocated()
    );
}

#[test]
fn no_depth_option_does_not_stop_a_tree_at_255_levels() {
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    #[cfg(windows)]
    {
        deep = std::path::PathBuf::from(format!("\\\\?\\{}", deep.display()));
    }
    let cleanup = deep.clone();
    for _ in 0..300 {
        deep.push("d");
        fs::create_dir(&deep).unwrap();
    }
    fs::write(deep.join("bottom.bin"), b"x").unwrap();
    let tree = scan(dir.path());
    let limited = Scanner::new(ScanOptions {
        max_depth: Some(255),
        ..Default::default()
    })
    .scan(dir.path())
    .unwrap();
    fs::remove_dir_all(&cleanup).unwrap();

    assert_eq!(tree.len(), 302);
    assert_eq!(tree.total_logical(), 1, "the bottom file was reached");
    assert!(!tree.is_incomplete());
    assert_eq!(tree.stats().max_depth, 255, "stored depth saturates");
    assert!(
        limited.is_incomplete(),
        "an explicitly requested ceiling still applies"
    );
}

/// A deeper tree than the fixture, so there are levels to compare.
fn layered() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut here = dir.path().to_path_buf();
    for level in 0..6 {
        here = here.join(format!("level-{level}"));
        fs::create_dir_all(&here).unwrap();
        for n in 0..4 {
            fs::create_dir_all(here.join(format!("branch-{n}"))).unwrap();
            fs::write(here.join(format!("branch-{n}/leaf.bin")), vec![0u8; 512]).unwrap();
        }
        fs::write(here.join("file.txt"), b"contents").unwrap();
    }
    dir
}

/// `architecture/concurrency.md` rests the whole channel design on this: progress
/// is "a *sample* of a monotonically increasing quantity — missing one costs
/// nothing, since the next one carries the same information". That is only true
/// while the quantity really does increase. A count that ever went backwards
/// would make a dropped update a lie rather than an omission, and the window —
/// which takes `rx.try_iter().last()` and nothing else — would have no way to
/// tell the difference.
#[test]
fn progress_only_ever_moves_forward() {
    let dir = layered();
    let (tx, rx) = crossbeam_channel::bounded(256);
    let tree = Scanner::new(ScanOptions::default())
        .with_progress(tx)
        .scan(dir.path())
        .unwrap();

    let updates: Vec<_> = rx.try_iter().collect();
    assert!(updates.len() > 3, "only {} levels published", updates.len());

    let mut entries = 0u64;
    let mut depth: Option<u8> = None;
    for update in &updates {
        assert!(
            update.entries >= entries,
            "entries went backwards: {entries} then {}",
            update.entries
        );
        entries = update.entries;
        if let Some(last) = depth {
            assert!(
                update.depth >= last,
                "depth {} was published after {last}",
                update.depth
            );
        }
        depth = Some(update.depth);
        assert!(
            update.entries <= tree.stats().entries,
            "a level reported {} entries out of a tree that ended with {}",
            update.entries,
            tree.stats().entries
        );
    }
}

/// And the other half of that decision: a receiver that cannot keep up, or is
/// not there at all, must change nothing about the scan.
///
/// `try_send` and a discarded error are what make that true, and what a test can
/// see is the result: the same tree, byte for byte in its totals, whether the
/// channel is roomy, one deep and unread, or gone entirely. A blocking send would
/// deadlock the second of those three.
#[test]
fn a_full_or_dropped_progress_channel_changes_nothing() {
    let dir = layered();
    let plain = Scanner::new(ScanOptions::default())
        .scan(dir.path())
        .unwrap();

    // One slot, nobody reading: every send after the first fails.
    let (tx, rx) = crossbeam_channel::bounded(1);
    let full = Scanner::new(ScanOptions::default())
        .with_progress(tx)
        .scan(dir.path())
        .unwrap();

    // And a receiver that has gone away: `try_send` reports disconnected.
    let (tx, rx_gone) = crossbeam_channel::bounded(16);
    drop(rx_gone);
    let gone = Scanner::new(ScanOptions::default())
        .with_progress(tx)
        .scan(dir.path())
        .unwrap();

    for (what, tree) in [("a full channel", &full), ("no receiver", &gone)] {
        assert_eq!(tree.len(), plain.len(), "{what} changed the entry count");
        assert_eq!(
            tree.total_allocated(),
            plain.total_allocated(),
            "{what} changed the total"
        );
        assert_eq!(
            tree.stats().entries,
            plain.stats().entries,
            "{what} changed what the scan reported about itself"
        );
    }
    // The one slot was filled and nothing else got in, which is the case the
    // bound exists for.
    assert_eq!(rx.len(), 1, "a bounded channel held more than its bound");
}

/// A tree deeper than `MAX_PATH`, which a real one often is.
///
/// 260 characters is the limit on a Win32 path that is not spelled `\\?\…`, and
/// `node_modules`, nested build outputs and anything unpacked from a deep archive
/// go past it without trying. `std` handles it — which is why the sizes here were
/// right all along — but the calls this crate makes to Win32 *by path* did not,
/// and the one in the walk reads a file's owner. Measured on a tree 306 characters
/// deep before the fix: the file and the two directories past the limit came back
/// as `(owner unreadable)`, and the five above them resolved to this account.
///
/// This violates the owner-reporting contract: an owner
/// that could not be read is a real and different thing from one that could not be
/// *reached*, and the length of a path is not a fact about who owns a file.
#[cfg(windows)]
#[test]
fn a_path_past_max_path_is_scanned_and_its_owner_read() {
    let dir = tempfile::tempdir().unwrap();
    // Long segments rather than many, so the depth ceiling is nowhere near.
    let mut deep = dir.path().to_path_buf();
    while deep.as_os_str().len() < 300 {
        deep = deep.join("a-segment-of-some-thirty-chars");
    }
    fs::create_dir_all(&deep).unwrap();
    let payload = deep.join("payload.bin");
    fs::write(&payload, vec![0u8; 100_000]).unwrap();
    assert!(
        payload.as_os_str().len() > 260,
        "the fixture is only {} characters deep",
        payload.as_os_str().len()
    );

    let tree = Scanner::new(ScanOptions {
        collect_owners: true,
        ..ScanOptions::default()
    })
    .scan(dir.path())
    .unwrap();

    // The bytes are there, which they were before this test existed.
    assert_eq!(
        tree.total_logical(),
        100_000,
        "a file past MAX_PATH was not measured"
    );

    // And every entry has an owner this account can name. `(owner unreadable)`
    // is what the share table calls the bucket with no account, and on a
    // tempdir nothing legitimately lands in it.
    for share in kosha_core::owner::shares(&tree) {
        assert!(
            share.owner.is_some(),
            "{} entries past MAX_PATH came back with no owner at all",
            share.files + share.dirs
        );
    }
}

/// Windows' answer to `st_dev` is the reparse point: volume mount points,
/// junctions and symlinks are all reparse points, and the scanner descends
/// none of them. That one rule is both the containment guarantee and the
/// loop guard, and until now nothing tested it on the platform that relies
/// on it — the symlink test below is `#[cfg(unix)]`.
///
/// A junction pointing at an ancestor is the classic way to make a walker
/// spin forever, and `mklink /J` needs no elevation, so it is easy to create
/// by accident. A disk tool that hangs is worse than one that reports a wrong
/// number: the wrong number at least comes back.
#[cfg(windows)]
#[test]
fn a_junction_is_measured_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("inner");
    fs::create_dir(&inner).unwrap();
    fs::write(inner.join("payload.bin"), vec![0u8; 4096]).unwrap();

    // Aimed at the root of the scan, so following it would never terminate.
    let loop_back = inner.join("loop");
    let linked = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&loop_back)
        .arg(dir.path())
        // `mklink` announces itself on success; test output stays quiet.
        .stdout(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !linked {
        return; // No `mklink` here; nothing to assert.
    }

    let tree = scan(dir.path());
    let inner_node = child(&tree, tree.root(), "inner");
    let junction = child(&tree, inner_node, "loop");

    assert!(
        tree.node(junction).is_symlink(),
        "a junction is a reparse point and has to be flagged as a link"
    );
    assert_eq!(
        tree.node(junction).child_count,
        0,
        "the junction was descended, which is how a walk loops forever"
    );
    assert_eq!(
        tree.total_logical(),
        4096,
        "the payload behind the junction was counted more than once"
    );

    // And `cross_filesystems` still cannot change it — which is the point now
    // that the option does something on Windows. It admits *mounted volumes*,
    // which are a filesystem boundary; a junction is a link to a directory on
    // the same volume, it shares its reparse tag with a mounted volume, and
    // following one is how a walk spins forever. The setting must tell them
    // apart, and this is where that is checked.
    let crossing = Scanner::new(ScanOptions {
        cross_filesystems: true,
        ..Default::default()
    })
    .scan(dir.path())
    .unwrap();
    assert_eq!(
        crossing.total_logical(),
        4096,
        "`cross_filesystems` made the walk follow a junction, which loops"
    );
    assert!(
        kosha_core::can_cross_filesystems(),
        "the option is offered on both platforms now"
    );

    // Remove the junction itself before the fixture is dropped, so cleanup
    // cannot depend on how `remove_dir_all` treats a reparse point.
    let _ = fs::remove_dir(&loop_back);
}

/// The first volume on this machine that has a GUID path, or `None`.
///
/// `mountvol` with no arguments only *lists*, so it needs no elevation —
/// unlike creating a mount point with it, which is why these tests build one
/// with `mklink /J` instead. A junction whose target is `\??\Volume{...}\` is
/// byte-for-byte the reparse point that `mountvol` would have written; the
/// difference is only in which tool wrote it.
#[cfg(windows)]
fn any_volume_guid() -> Option<String> {
    let out = std::process::Command::new("mountvol").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| line.starts_with(r"\\?\Volume{") && line.ends_with('\\'))
        .map(str::to_string)
}

#[cfg(windows)]
fn junction(link: &Path, target: &str) -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// A mounted volume is a filesystem boundary; a junction is a link. Windows
/// writes both as the same reparse tag and they differ only in what they point
/// at, so this is the distinction the whole feature rests on.
///
/// Before it existed, a mounted 2 TB drive was reported as a link with no
/// bytes, and `cross_filesystems` was withheld on Windows because turning it on
/// could not have changed that.
#[cfg(windows)]
#[test]
fn a_mounted_volume_is_a_boundary_and_a_junction_is_a_link() {
    let Some(volume) = any_volume_guid() else {
        return; // No `mountvol` here; nothing to assert.
    };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("ordinary")).unwrap();
    fs::write(root.join("ordinary/payload.bin"), vec![0u8; 4096]).unwrap();

    let mounted = root.join("mounted");
    let link = root.join("plain-junction");
    if !junction(&mounted, &volume) || !junction(&link, &root.join("ordinary").to_string_lossy()) {
        return; // No `mklink`; nothing to assert.
    }

    let tree = scan(root);
    let mounted_node = child(&tree, tree.root(), "mounted");
    let link_node = child(&tree, tree.root(), "plain-junction");

    assert!(
        tree.node(mounted_node).flags.contains(Flags::OTHER_DEVICE),
        "a mounted volume was not reported as another filesystem"
    );
    assert!(
        !tree.node(mounted_node).is_symlink(),
        "a mounted volume is a filesystem, not a link"
    );
    assert_eq!(
        tree.node(mounted_node).child_count,
        0,
        "another filesystem is not descended by default"
    );

    assert!(
        tree.node(link_node).is_symlink(),
        "a junction to a directory is still a link"
    );
    assert!(
        !tree.node(link_node).flags.contains(Flags::OTHER_DEVICE),
        "a junction stays on the same filesystem and must not claim otherwise"
    );

    // Removed before the fixture drops, so cleanup cannot depend on how
    // `remove_dir_all` treats a reparse point.
    let _ = fs::remove_dir(&mounted);
    let _ = fs::remove_dir(&link);
}

/// One volume, walked once.
///
/// A volume can be mounted in more than one place, and can be mounted inside
/// itself — both are one `mountvol` away and neither is exotic. Without a guard
/// the second mount is walked again, which at best doubles the total and at
/// worst runs until the depth ceiling with a tree that stopped meaning anything
/// long before.
///
/// Bounded with `max_depth` so this reads one level of a real volume rather
/// than all of it.
#[cfg(windows)]
#[test]
fn a_volume_mounted_twice_is_walked_once() {
    let Some(volume) = any_volume_guid() else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let first = root.join("a-mount");
    let second = root.join("b-mount");
    if !junction(&first, &volume) || !junction(&second, &volume) {
        return;
    }

    let tree = Scanner::new(ScanOptions {
        cross_filesystems: true,
        max_depth: Some(2),
        ..Default::default()
    })
    .scan(root)
    .unwrap();

    let a = tree.node(child(&tree, tree.root(), "a-mount"));
    let b = tree.node(child(&tree, tree.root(), "b-mount"));

    // Sorted by name, so the first alphabetically is the one that gets in.
    assert!(
        a.child_count > 0,
        "`cross_filesystems` did not descend into a mounted volume"
    );
    assert_eq!(
        b.child_count, 0,
        "the same volume was walked twice, which is how a scan runs away"
    );
    assert!(
        b.flags.contains(Flags::OTHER_DEVICE),
        "the refused mount should still say what it is"
    );

    let _ = fs::remove_dir(&first);
    let _ = fs::remove_dir(&second);
}

#[cfg(unix)]
#[test]
fn symlinks_are_measured_not_followed() {
    let dir = fixture();
    std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("link-to-a")).unwrap();

    let tree = scan(dir.path());
    let link = child(&tree, tree.root(), "link-to-a");

    assert!(tree.node(link).is_symlink());
    assert_eq!(
        tree.node(link).child_count,
        0,
        "a symlink is never descended"
    );
    // The link contributes only its own tiny size, not the 4 KiB target.
    assert!(tree.node(link).logical < 4096);
}

/// Deliberately not `#[cfg(unix)]`. It was, and that is exactly how Windows
/// came to hand back `link_key: None` unconditionally while the settings panel
/// advertised the feature: the one test that would have caught it never ran on
/// the platform that was broken. `fs::hard_link` works on both.
#[test]
fn hardlinked_bytes_are_counted_once() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("original.bin"), vec![0u8; 8192]).unwrap();
    fs::hard_link(
        dir.path().join("original.bin"),
        dir.path().join("same-file.bin"),
    )
    .unwrap();

    // Asked for, because this is a *walk* of a real directory and on Windows
    // that is the one strategy which pays an open per file to find out. The
    // default leaves it to what the strategy costs, which is what
    // `a_walk_dedups_hardlinks_where_the_platform_gives_them_away` covers.
    let tree = Scanner::new(ScanOptions {
        dedup_hardlinks: Some(true),
        ..Default::default()
    })
    .scan(dir.path())
    .unwrap();

    assert_eq!(tree.stats().hardlinks_deduped, 1);
    assert_eq!(
        tree.total_logical(),
        8192,
        "two names, one set of bytes, one contribution"
    );

    // The other half of the promise: with the setting off, both names count.
    let counted_twice = Scanner::new(ScanOptions {
        dedup_hardlinks: Some(false),
        ..Default::default()
    })
    .scan(dir.path())
    .unwrap();

    assert_eq!(counted_twice.stats().hardlinks_deduped, 0);
    assert_eq!(counted_twice.total_logical(), 16384);
}

/// With neither flag, a *walk* dedups exactly where the platform gives the
/// answer away — and the platform, not a `cfg`, is what says so.
///
/// This is the half of the default that costs something. The other half is that
/// every strategy which already holds a link count dedups on both platforms,
/// which each reader's own tests assert against a synthetic image; before this
/// they did not, and the same ext4 image totalled 4 KiB per hardlink more on
/// Windows than on Linux with everything else in the export byte-identical.
#[test]
fn a_walk_dedups_hardlinks_where_the_platform_gives_them_away() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("original.bin"), vec![0u8; 8192]).unwrap();
    fs::hard_link(
        dir.path().join("original.bin"),
        dir.path().join("same-file.bin"),
    )
    .unwrap();

    let tree = Scanner::new(ScanOptions::default())
        .scan(dir.path())
        .unwrap();

    if kosha_core::hardlinks_cost_extra() {
        assert_eq!(tree.stats().hardlinks_deduped, 0);
        assert_eq!(tree.total_logical(), 16384);
    } else {
        assert_eq!(tree.stats().hardlinks_deduped, 1);
        assert_eq!(tree.total_logical(), 8192);
    }
}

/// Turning the setting on must not change the answer for a tree that has no
/// hardlinks in it. On Windows the identity query returns `None` for a file
/// with a single link, so nothing reaches the scanner's set; getting that
/// wrong would make every file collide with the first one on its volume.
#[test]
fn unshared_files_are_unaffected_by_hardlink_accounting() {
    let dir = fixture();

    let plain = scan(dir.path());
    let deduped = Scanner::new(ScanOptions {
        dedup_hardlinks: Some(true),
        ..Default::default()
    })
    .scan(dir.path())
    .unwrap();

    assert_eq!(deduped.stats().hardlinks_deduped, 0);
    assert_eq!(deduped.stats().entries, plain.stats().entries);
    assert_eq!(deduped.total_logical(), plain.total_logical());
}

/// What kosha says about an image it recognises and cannot read.
///
/// **"There is no filesystem kosha reads in this file" is true and it reads
/// like "your file is broken".** Somebody who has just handed over a `dd` of a
/// Fedora laptop has a good image of a good filesystem; the fact they can act
/// on is *btrfs*. Naming costs a magic number and reading costs a reader, so
/// the list kosha can name is far longer than the list it can read.
mod a_filesystem_kosha_cannot_read {
    use super::*;

    /// btrfs keeps its superblock 64 KiB in, which is why a file has to be at
    /// least that long before there is anything to find.
    fn btrfs_stub(at: &Path) {
        let mut bytes = vec![0u8; 0x1_1000];
        bytes[0x1_0040..0x1_0048].copy_from_slice(b"_BHRfS_M");
        fs::write(at, bytes).unwrap();
    }

    /// The same, a megabyte into a disk with a table in front of it: a `dd` of
    /// a whole disk rather than of one partition.
    fn partitioned_btrfs_stub(at: &Path) {
        const START: usize = 2048 * 512;
        let mut bytes = vec![0u8; START + 0x1_1000];
        bytes[START + 0x1_0040..START + 0x1_0048].copy_from_slice(b"_BHRfS_M");
        // One primary partition of type 0x83 at sector 2048.
        bytes[446] = 0x80;
        bytes[446 + 4] = 0x83;
        bytes[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        bytes[446 + 12..446 + 16].copy_from_slice(&0x88u32.to_le_bytes());
        bytes[510] = 0x55;
        bytes[511] = 0xAA;
        fs::write(at, bytes).unwrap();
    }

    /// **One step, not two.** Without this somebody is told to ask for images,
    /// asks, and is then told the filesystem is btrfs — two steps to a fact
    /// that was knowable at the first, and the first step was an instruction
    /// that did not help.
    #[test]
    fn is_named_without_being_asked_for_images() {
        let dir = tempfile::tempdir().unwrap();
        for (name, write) in [
            ("bare.img", btrfs_stub as fn(&Path)),
            ("whole-disk.img", partitioned_btrfs_stub),
        ] {
            let image = dir.path().join(name);
            write(&image);
            let error = Scanner::new(ScanOptions::default())
                .scan(&image)
                .expect_err("btrfs is not a directory");
            let said = error.to_string();
            assert!(said.contains("btrfs"), "{name} was not named: {said}");
        }
    }

    /// And the same answer when images *were* asked for, because asking was
    /// never going to change it.
    #[test]
    fn is_named_the_same_way_when_images_are_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("fedora.img");
        btrfs_stub(&image);
        let error = Scanner::new(ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        })
        .scan(&image)
        .expect_err("btrfs is not read");
        assert!(error.to_string().contains("btrfs"), "{error}");
    }

    /// A file that is not anything keeps the old message. The table is a list
    /// of answers, not a licence to guess.
    #[test]
    fn a_file_that_is_nothing_is_still_just_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let ordinary = dir.path().join("notes.txt");
        fs::write(&ordinary, vec![0u8; 0x1_1000]).unwrap();
        assert!(
            matches!(
                Scanner::new(ScanOptions::default()).scan(&ordinary),
                Err(ScanError::NotADirectory(_))
            ),
            "a file of zeros was named as something"
        );
    }
}

/// A folder nothing could be read from is not a folder with nothing in it.
///
/// **Measured on `C:\System Volume Information`**, which an ordinary account
/// cannot open: the window drew *"This folder is empty."* in the middle of the
/// treemap and the command line printed `~0 B on disk` and exited 0. Each had
/// one line to the contrary — `1 unreadable` in the status bar, "1 entry could
/// not be read" above the table — and neither is an answer to "what is in
/// here".
///
/// The flag was already on the root node; nothing asked it. `root_unreadable`
/// is the question, and it is deliberately not `is_partial`: partial means the
/// totals are a floor, and a floor of nothing is not a number worth showing.
#[test]
fn a_root_that_could_not_be_read_says_so_rather_than_reading_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("inside.bin"), vec![0u8; 4096]).unwrap();
    let _guard = deny_reading(&locked);

    let tree = Scanner::new(ScanOptions::default()).scan(&locked).unwrap();
    assert!(
        tree.root_unreadable(),
        "the scan read a folder that cannot be read: {} entries",
        tree.len()
    );
    assert_eq!(tree.len(), 1, "nothing but the root itself is in the tree");
    assert!(tree.is_partial(), "and the totals are not a total");

    // An ordinary folder is not mistaken for one, including an empty one —
    // which is the case this has to stay distinct from.
    let ordinary = dir.path().join("ordinary");
    std::fs::create_dir(&ordinary).unwrap();
    let scanned = Scanner::new(ScanOptions::default())
        .scan(&ordinary)
        .unwrap();
    assert!(!scanned.root_unreadable(), "an empty folder is readable");
    assert!(!scanned.is_partial());
}

/// Make a directory unopenable, and put it back afterwards.
///
/// Both platforms, because the defect was found on one and the code is shared.
/// On Windows that means an explicit deny for this account — the temporary
/// directory is ours, so no privilege is needed — and on Unix a mode with no
/// bits on it. Either way the guard restores it, or `tempfile` cannot clean up
/// and the test leaves a folder nobody can delete.
fn deny_reading(path: &Path) -> impl Drop {
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            #[cfg(windows)]
            {
                let who = std::env::var("USERNAME").unwrap_or_default();
                let _ = std::process::Command::new("icacls.exe")
                    .args([&self.0.to_string_lossy() as &str, "/remove:d", &who, "/q"])
                    .output();
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
    }

    #[cfg(windows)]
    {
        let who = std::env::var("USERNAME").expect("an account name");
        let done = std::process::Command::new("icacls.exe")
            .args([
                &path.to_string_lossy() as &str,
                "/deny",
                &format!("{who}:(RX)"),
                "/q",
            ])
            .output()
            .expect("icacls runs");
        assert!(
            done.status.success(),
            "icacls: {}",
            String::from_utf8_lossy(&done.stderr)
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    Restore(path.to_path_buf())
}
