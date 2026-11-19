// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! kosha against `du`, which is the only independent implementation of this
//! arithmetic that is already on the machine.
//!
//! Every other differential check in this project compares kosha with *itself*:
//! one tree against eight filesystem readers, one image against two platform
//! builds, the MFT against the directory walk. Those find disagreements between
//! kosha's own paths and cannot find a rule that kosha applies consistently and
//! wrongly. `du` can, and it is the tool anybody would reach for to check a
//! number this program produced.
//!
//! The pairs are chosen so that a systematic error has somewhere to show:
//!
//! | `du` | kosha |
//! | --- | --- |
//! | `du -x -s -B1` | `total_allocated` — `st_blocks × 512`, the bytes on disk |
//! | `du -x -s -b` | `total_logical` — the lengths files report |
//!
//! On macOS the allocated comparison uses `/usr/bin/du -x -s` with
//! `BLOCKSIZE=512`, then converts its block count to bytes. BSD `-B` controls
//! allocation rounding, not the output units as GNU `-B` does.
//!
//! The fixture is built out of the cases where the two quantities come apart: a
//! sparse file, a hardlink pair, a symlink, an empty directory, and enough small
//! files that block rounding is a large fraction of the total. Both tools count a
//! hardlinked file once, count a symlink as itself, and count a directory's own
//! blocks, so agreement is the expectation rather than a coincidence to explain.
//!
//! Unix only, and skipped where there is no `du` — which is every Windows box,
//! and the reason this cannot be the only check of the same arithmetic.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use kosha_core::{ScanOptions, Scanner, SizeTree};

/// `du`'s answer in bytes, or `None` if absent or its requested mode is unsupported.
fn du(root: &Path, apparent: bool) -> Option<u64> {
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("/usr/bin/du");
    #[cfg(not(target_os = "macos"))]
    let mut cmd = Command::new("du");
    cmd.arg("-x").arg("-s");
    if apparent {
        cmd.arg("-b");
    } else if cfg!(target_os = "macos") {
        cmd.env("BLOCKSIZE", "512");
    } else {
        cmd.arg("-B1");
    }
    let out = cmd.arg(root).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let value: u64 = text.split_whitespace().next()?.parse().ok()?;
    let unit = if cfg!(target_os = "macos") && !apparent {
        512
    } else {
        1
    };
    value.checked_mul(unit)
}

/// A tree of the cases where on-disk and apparent size disagree.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Enough small files that block rounding is most of the total.
    fs::create_dir_all(root.join("many")).unwrap();
    for n in 0..200 {
        fs::write(root.join(format!("many/small-{n}.txt")), b"a few bytes").unwrap();
    }

    // Nested, so directories' own blocks are in the sum at several levels.
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::write(root.join("a/b/c/leaf.bin"), vec![0u8; 40_000]).unwrap();
    fs::write(root.join("a/b/mid.bin"), vec![0u8; 5_000]).unwrap();

    // An empty directory occupies blocks and holds nothing.
    fs::create_dir_all(root.join("empty")).unwrap();

    // Sparse: a megabyte of length and one block of contents.
    let sparse = root.join("sparse.bin");
    let file = fs::File::create(&sparse).unwrap();
    file.set_len(1024 * 1024).unwrap();
    drop(file);

    // A hardlink pair. Both tools count the bytes once.
    fs::write(root.join("linked.bin"), vec![0u8; 8_000]).unwrap();
    fs::hard_link(root.join("linked.bin"), root.join("linked-again.bin")).unwrap();

    // A symlink is counted as itself, never followed.
    std::os::unix::fs::symlink(root.join("a/b/c/leaf.bin"), root.join("points-at-leaf")).unwrap();

    dir
}

fn scan(root: &Path) -> SizeTree {
    Scanner::new(ScanOptions::default()).scan(root).unwrap()
}

#[test]
fn on_disk_bytes_agree_with_du() {
    let dir = fixture();
    let tree = scan(dir.path());

    let Some(theirs) = du(dir.path(), false) else {
        eprintln!("`du` allocated-byte comparison unavailable on this machine");
        return;
    };
    assert_eq!(
        tree.total_allocated(),
        theirs,
        "kosha says {} bytes on disk and platform `du` reports {theirs} bytes",
        tree.total_allocated()
    );
}

#[test]
fn apparent_bytes_agree_with_du() {
    let dir = fixture();
    let tree = scan(dir.path());

    let Some(theirs) = du(dir.path(), true) else {
        eprintln!("`du` apparent-byte mode unavailable on this platform");
        return;
    };
    // Exactly, with nothing adjusted. `scan.rs` leaves a directory's own
    // `st_size` out of the apparent total, and argues it from portability: Unix
    // reports a real length for a directory — 60 bytes on tmpfs, 4096 on ext4 —
    // while NTFS reports zero, so counting it would make one tree measure
    // differently on the two platforms.
    //
    // `du -b` makes the same choice, which was worth checking rather than
    // assuming: a directory of 60 bytes holding one 5-byte file reports 5, not
    // 65. So the two totals are comparable as they stand, and the first version
    // of this test was wrong for subtracting a difference it expected to find.
    assert_eq!(
        tree.total_logical(),
        theirs,
        "kosha says {} apparent bytes and `du -b` says {theirs}",
        tree.total_logical()
    );
}

/// The hardlink rule, isolated, because it is the one where the two tools could
/// agree on a total for opposite reasons.
#[test]
fn a_hardlinked_file_is_counted_once_by_both() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("one.bin"), vec![0u8; 64 * 1024]).unwrap();
    fs::hard_link(root.join("one.bin"), root.join("two.bin")).unwrap();

    let tree = scan(root);
    let Some(theirs) = du(root, false) else {
        return;
    };
    assert_eq!(tree.total_allocated(), theirs);

    // And counted twice when asked, which is `du -l`.
    let doubled = Scanner::new(ScanOptions {
        dedup_hardlinks: Some(false),
        ..ScanOptions::default()
    })
    .scan(root)
    .unwrap();
    assert!(
        doubled.total_allocated() > tree.total_allocated(),
        "counting both names gave no more than counting one"
    );
}
