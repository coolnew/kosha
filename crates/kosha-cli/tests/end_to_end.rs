// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The command line, run for real against a tree on disk.
//!
//! Everything else in this workspace tests a layer: the scanner against a
//! fixture, the rules against a synthetic tree, the parser against strings.
//! None of that runs the binary, and the binary is what somebody types.
//!
//! **This is the only behavioural coverage macOS has.** Nobody has run kosha on
//! a Mac — it compiles, and CI runs the suite, and that was the whole of it. A
//! test that spawns the real binary against a real directory is the closest
//! thing to somebody sitting in front of one, and it costs a few hundred
//! kilobytes of temporary files to have it on every platform at once.
//!
//! Every test that could touch the snapshot store runs with its own `HOME` and
//! `LOCALAPPDATA`, so a test run leaves nothing in the actual one. Finding that
//! out the hard way would mean a developer's snapshot list quietly filling with
//! `/tmp` trees.

mod common;

use std::path::Path;
use std::process::{Command, Output};

/// A tree with something to say about it.
///
/// ```text
/// root/
///   media/holiday.bin      64 KiB
///   media/original.bin     32 KiB   ┐ identical contents,
///   cache/copy.bin         32 KiB   ┘ in different folders
///   cache/small.bin         1 KiB
///   docs/notes.txt          ~40 B
///   empty/
/// ```
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    for folder in ["media", "cache", "docs", "empty"] {
        std::fs::create_dir_all(root.join(folder)).expect("a folder");
    }
    // Deterministic contents rather than random, so a failure is reproducible
    // and the duplicate pair is identical by construction.
    let big = vec![b'h'; 64 * 1024];
    let twin = vec![b'o'; 32 * 1024];
    std::fs::write(root.join("media/holiday.bin"), &big).unwrap();
    std::fs::write(root.join("media/original.bin"), &twin).unwrap();
    std::fs::write(root.join("cache/copy.bin"), &twin).unwrap();
    std::fs::write(root.join("cache/small.bin"), vec![b's'; 1024]).unwrap();
    std::fs::write(
        root.join("docs/notes.txt"),
        b"a short note, for the extension\n",
    )
    .unwrap();
    settled(root);
    dir
}

/// Wait until the tree has finished being written down.
///
/// **A directory's modified time is kept twice**: in the directory's own
/// record, and as a copy inside its parent's entry. An enumeration hands back
/// the copy — it is the whole reason enumerating is fast, since nothing is
/// opened — so the copy is what the scanner reads. Measured on NTFS, the copy
/// trails the record by a millisecond or two immediately after a file is
/// created in the directory, and catches up in about a hundred microseconds:
/// 10 rounds in 20 were behind when asked at once, none still behind a
/// moment later.
///
/// A millisecond does not matter until it straddles a second, and kosha keeps
/// whole seconds. Then two scans of a tree nobody touched report a directory
/// one second apart, `the_export_is_the_same_twice` fails, and the difference
/// is a `docs` row reading `21:29:19Z` against `21:29:20Z`. Measured at one
/// run in 300 — which is a test failing every few full runs, about a tree
/// that had not stopped being written when the first scan read it.
///
/// So this waits for the two to agree, which is what "unchanged" means here.
/// On Unix there is one record and `metadata` on an entry goes and asks for
/// it, so the first look already agrees and this costs nothing.
fn settled(root: &Path) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(5) {
        let mut lagging = false;
        for entry in std::fs::read_dir(root).expect("the tree").flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let copy = entry.metadata().and_then(|m| m.modified()).ok();
            let record = std::fs::metadata(entry.path())
                .and_then(|m| m.modified())
                .ok();
            lagging |= copy != record;
        }
        if !lagging {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the fixture never stopped changing");
}

/// Run `kosha`, with its own idea of where the user's files live.
///
/// The environment is redirected on every platform at once rather than by
/// `cfg!`: `dirs` reads `LOCALAPPDATA` on Windows, `HOME` on macOS and
/// `XDG_DATA_HOME` on Linux, and a test that sets only the one it happens to be
/// running on is a test that writes into a real profile everywhere else.
fn run(args: &[&str], home: &Path) -> Output {
    Command::new(common::kosha())
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("roaming"))
        .env("LOCALAPPDATA", home.join("local"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .output()
        .expect("kosha runs")
}

fn text(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exited {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_scan_reports_the_tree_it_was_given() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let out = text(&run(&[&tree.path().to_string_lossy()], home.path()));

    // Five directories counting the root, and five files.
    assert!(out.contains("10 entries"), "{out}");
    // The largest folder first, and every folder named.
    for folder in ["media", "cache", "docs"] {
        assert!(out.contains(folder), "no {folder} in:\n{out}");
    }
    let media = out.find("media").expect("media");
    let cache = out.find("cache").expect("cache");
    assert!(media < cache, "largest first:\n{out}");
}

#[test]
fn stats_count_what_is_there() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let out = text(&run(
        &[&tree.path().to_string_lossy(), "--stats"],
        home.path(),
    ));

    assert!(out.contains("directories"), "{out}");
    assert!(out.contains("files"), "{out}");
    assert!(out.contains("unreadable"), "{out}");
    // Nothing in a tree this test just built should be unreadable, on any
    // platform. A `1` here on Windows would mean a file was locked by the
    // writer, which is worth failing over rather than tolerating.
    let unreadable = out
        .lines()
        .find(|line| line.contains("unreadable"))
        .expect("an unreadable line");
    assert!(unreadable.trim_end().ends_with('0'), "{unreadable}");
}

#[test]
fn the_export_is_the_same_twice() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = tree.path().to_string_lossy().to_string();

    let first = text(&run(&[&path, "--export", "csv"], home.path()));
    let second = text(&run(&[&path, "--export", "csv"], home.path()));
    assert_eq!(first, second, "two exports of one tree differ");

    // A header and a row per entry, the root excluded — relative paths, so the
    // root would have nothing to be called. Nine rows under it, ten lines.
    assert!(first.starts_with("path,name,kind,"), "{}", &first[..60]);
    assert_eq!(first.lines().count(), 10, "{first}");
    // Forward slashes on every platform, which is what makes two exports of
    // the same tree from different machines comparable at all.
    assert!(first.contains("media/holiday.bin"), "{first}");
    assert!(
        !first.contains('\\'),
        "a backslash reached the export:\n{first}"
    );
}

#[test]
fn a_query_matches_the_files_it_should() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = tree.path().to_string_lossy().to_string();

    let out = text(&run(
        &[&path, "--query", "file and size > 20k"],
        home.path(),
    ));
    assert!(out.contains("holiday.bin"), "{out}");
    assert!(out.contains("original.bin"), "{out}");
    assert!(
        !out.contains("small.bin"),
        "1 KiB matched `size > 20k`:\n{out}"
    );

    // And the flags, which are how a person asks for one kind of entry.
    let ext = text(&run(&[&path, "--query", "ext = txt"], home.path()));
    assert!(ext.contains("notes.txt"), "{ext}");
    assert!(!ext.contains("holiday.bin"), "{ext}");
}

#[test]
fn identical_files_are_found_in_different_folders() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let out = text(&run(
        &[
            &tree.path().to_string_lossy(),
            "--duplicates",
            "--dupe-min",
            "1000",
        ],
        home.path(),
    ));

    assert!(out.contains("DUPLICATES"), "{out}");
    // The pair, and not the singleton of the same size.
    assert!(out.contains("original.bin"), "{out}");
    assert!(out.contains("copy.bin"), "{out}");
    assert!(
        !out.contains("holiday.bin"),
        "a unique file was called a copy:\n{out}"
    );
}

#[test]
fn a_scan_says_who_owns_it() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let out = text(&run(
        &[&tree.path().to_string_lossy(), "--owners"],
        home.path(),
    ));

    assert!(out.contains("OWNERS"), "{out}");
    // Whoever is running the test owns everything in a tree they just made, so
    // exactly one account holds the whole total. The name differs per platform
    // and per machine; that it is a single row does not.
    let rows = out
        .lines()
        .skip_while(|line| !line.contains("OWNERS"))
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count();
    assert_eq!(rows, 1, "expected one owner:\n{out}");
}

/// The whole snapshot round trip, through the store, in a profile of its own.
#[test]
fn a_snapshot_is_kept_listed_browsed_and_compared() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = tree.path().to_string_lossy().to_string();

    let kept = text(&run(
        &[&path, "--snapshot", "--label", "before"],
        home.path(),
    ));
    assert!(kept.contains("SNAPSHOT"), "{kept}");

    // Listed, with the label it was given.
    let listed = text(&run(&["--snapshots"], home.path()));
    assert!(listed.contains("before"), "{listed}");
    assert!(listed.contains("1 snapshot"), "{listed}");

    // The file itself, found by looking rather than by parsing the output
    // above — where a snapshot goes is the store's business.
    let store = find_snapshot(home.path());

    // Browsing one gives the tree it held, and says it is a snapshot.
    let browsed = text(&run(&[&store.to_string_lossy()], home.path()));
    assert!(browsed.contains("SNAPSHOT"), "{browsed}");
    assert!(browsed.contains("media"), "{browsed}");

    // Now the disk moves on, and the comparison has to see it.
    std::fs::write(tree.path().join("cache/copy.bin"), vec![b'o'; 512 * 1024]).unwrap();
    std::fs::remove_file(tree.path().join("media/holiday.bin")).unwrap();

    let since = text(&run(
        &[
            &path,
            "--since",
            &store.to_string_lossy(),
            "--since-min",
            "1",
        ],
        home.path(),
    ));
    assert!(since.contains("SINCE"), "{since}");
    assert!(
        since.contains("copy.bin") || since.contains("cache"),
        "{since}"
    );
    assert!(
        since.contains("holiday.bin") || since.contains("media"),
        "the deleted file is missing from:\n{since}"
    );
}

/// The one snapshot in a profile built for this test.
fn find_snapshot(home: &Path) -> std::path::PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![home.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kosha") {
                found.push(path);
            }
        }
    }
    assert_eq!(found.len(), 1, "expected one snapshot, found {found:?}");
    found.pop().unwrap()
}

/// A snapshot must not land in the real profile just because a test ran.
#[test]
fn the_store_stays_inside_the_profile_it_was_given() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    run(&[&tree.path().to_string_lossy(), "--snapshot"], home.path());

    let snapshot = find_snapshot(home.path());
    assert!(
        snapshot.starts_with(home.path()),
        "{} escaped {}",
        snapshot.display(),
        home.path().display()
    );
}

/// A depth ceiling makes every total a floor, and the program says so twice:
/// once about the scan, and once about a record of it that something is being
/// compared against.
///
/// `--depth 1` on the fixture reported a fraction of it with no mark at all, and
/// a snapshot of that scan called itself complete — so a later full scan compared
/// against it reported everything below the ceiling as *new*. That is the reading
/// the snapshot's `partial` flag prevents, and a depth-limited scan was the one way
/// of being partial that never set it.
#[test]
fn a_depth_limited_scan_is_a_floor_and_says_so_everywhere() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = tree.path().to_string_lossy().to_string();

    // The scan itself: a tilde on the totals and a line saying why.
    let shallow = text(&run(&[&path, "--depth", "1"], home.path()));
    assert!(
        shallow.contains("stopped at depth 1"),
        "a depth-limited scan did not say so:\n{shallow}"
    );
    assert!(
        shallow.contains('~'),
        "a depth-limited total is not marked as a floor:\n{shallow}"
    );

    // And a full scan of the same tree is not marked, or the mark means nothing.
    let whole = text(&run(&[&path], home.path()));
    assert!(!whole.contains("stopped at depth"), "{whole}");
    assert!(!whole.contains('~'), "{whole}");

    // The record, and the comparison that would otherwise invent the difference.
    run(&[&path, "--depth", "1", "--snapshot"], home.path());
    let snapshot = find_snapshot(home.path());
    let since = run(
        &[
            &path,
            "--since",
            &snapshot.to_string_lossy(),
            "--since-min",
            "1",
        ],
        home.path(),
    );
    let said = String::from_utf8_lossy(&since.stdout).to_string();
    assert!(
        said.contains("the record is incomplete") && said.contains("depth ceiling"),
        "a comparison against a shallow record gave no warning:\n{said}"
    );

    // **And the same thing the other way round, which nothing said.** A
    // record that did not see everything reports what it missed as created;
    // a scan that did not see everything reports what it missed as gone. Only
    // the first half was ever spoken. Measured on a complete record held
    // against a scan of the same untouched tree stopped at depth 1: every
    // file below the ceiling listed as `gone`, with no warning anywhere.
    run(&[&path, "--snapshot"], home.path());
    let whole_record = newest_snapshot(home.path());
    let shallow_now = run(
        &[
            &path,
            "--since",
            &whole_record.to_string_lossy(),
            "--depth",
            "1",
            "--since-min",
            "1",
        ],
        home.path(),
    );
    let said = String::from_utf8_lossy(&shallow_now.stdout).to_string();
    assert!(
        said.contains("gone"),
        "the fixture stopped inventing:\n{said}"
    );
    assert!(
        said.contains("this scan is incomplete") && said.contains("depth ceiling"),
        "a shallow scan against a whole record gave no warning:\n{said}"
    );

    // A complete scan against the same record says neither of those.
    let both_whole = text(&run(
        &[
            &path,
            "--since",
            &whole_record.to_string_lossy(),
            "--since-min",
            "1",
        ],
        home.path(),
    ));
    assert!(!both_whole.contains("incomplete"), "{both_whole}");
}

/// The most recently written snapshot in a test's own store.
///
/// `find_snapshot` insists there is exactly one, which is the right assertion
/// for every test that keeps one. This test keeps two — a shallow record and
/// a whole one — and wants the one it just asked for. The walk is the same,
/// because where the store lives is the platform's business and no test
/// should need to know it.
fn newest_snapshot(home: &Path) -> std::path::PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![home.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kosha") {
                found.push(path);
            }
        }
    }
    found.sort_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
    found.pop().expect("a snapshot")
}

/// One of anything reads as one, in the line every run prints.
///
/// An empty folder scanned as "1 entries in 1 ms", which is the first line of
/// output this tool has and the one a person reads every single time. The
/// count and the noun were being formatted separately, so `format::plural` —
/// which this codebase has, and uses in a dozen other places — was never
/// asked. The same slip was in the query summary, the unreadable line, the
/// duplicate hunt's candidates and, in the window, the two progress lines, the
/// snapshot rail's detail, the status bar and every sentence in the
/// confirmation dialog: "Delete 1 items".
#[test]
fn one_entry_is_not_one_entries() {
    let home = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();

    let out = text(&run(&[&empty.path().to_string_lossy()], home.path()));
    assert!(out.contains("1 entry in"), "{out}");
    assert!(!out.contains("1 entries"), "{out}");

    // And the query summary counts what it searched the same way.
    let one = text(&run(
        &[&empty.path().to_string_lossy(), "--query", "size > 0"],
        home.path(),
    ));
    assert!(one.contains("searched 1 entry in"), "{one}");

    // More than one still reads as more than one, which is the other half of
    // getting this right and the half a naive fix breaks.
    let tree = fixture();
    let many = text(&run(&[&tree.path().to_string_lossy()], home.path()));
    assert!(many.contains("10 entries in"), "{many}");
}

/// A file in the store that cannot be read is named, not dropped.
///
/// Measured: with a half-copied snapshot and two files that were never
/// snapshots sitting beside one good one, `kosha --snapshots` said "1 snapshot"
/// and nothing else, and the window's rail showed one row. The skipping is
/// right — one damaged file must not hide the nine good ones — but a snapshot
/// that disappears from the listing reads exactly like one that was never
/// taken, while the file sits there occupying its megabytes.
///
/// `store::list`'s own note had said "the window is where somebody would be
/// told about it". Nothing carried the news that far.
#[test]
fn a_snapshot_that_cannot_be_read_is_reported_rather_than_skipped() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    run(&[&tree.path().to_string_lossy(), "--snapshot"], home.path());
    let good = find_snapshot(home.path());
    let store = good.parent().unwrap().to_path_buf();

    // A copy that stopped part way, and a file that was never a snapshot.
    let whole = std::fs::read(&good).unwrap();
    std::fs::write(store.join("cut-short.kosha"), &whole[..whole.len() / 3]).unwrap();
    std::fs::write(store.join("notes.kosha"), b"this is not a snapshot").unwrap();

    let out = text(&run(&["--snapshots"], home.path()));

    // The good one is still listed, and still counted alone.
    assert!(out.contains("1 snapshot occupying"), "{out}");
    // And the two that are not are named, each with its own reason.
    assert!(
        out.contains("2 files here could not be read"),
        "the listing says nothing about what it skipped:\n{out}"
    );
    assert!(
        out.contains("cut-short.kosha") && out.contains("ends before its"),
        "a truncated snapshot is not named as one:\n{out}"
    );
    assert!(
        out.contains("notes.kosha") && out.contains("not a kosha snapshot"),
        "a file that was never a snapshot is not told apart from a damaged one:\n{out}"
    );

    // And a store with nothing *but* unreadable files does not read as empty.
    let empty = tempfile::tempdir().unwrap();
    let bare = store
        .strip_prefix(home.path())
        .expect("the store is under the profile it was given")
        .to_path_buf();
    std::fs::create_dir_all(empty.path().join(&bare)).unwrap();
    std::fs::write(empty.path().join(&bare).join("notes.kosha"), b"nope").unwrap();
    let none = text(&run(&["--snapshots"], empty.path()));
    assert!(none.contains("No snapshots kept yet"), "{none}");
    assert!(
        none.contains("1 file here could not be read"),
        "a store holding one unreadable file read as an empty one:\n{none}"
    );
}

/// A comparison across two counting rules says so.
///
/// **Measured, and it is the pair of commands somebody is most likely to run
/// in that order.** Five names for one 8 MiB file: snapshotted with hardlink
/// folding on, then scanned with it off, and the comparison reported
/// `+32.0 MiB` of growth across four files while nothing on disk had changed.
/// Only the rule had.
///
/// On Windows the rule belongs to the strategy — an elevated `--mft` scan
/// folds hardlinks because the record number is free there, and the ordinary
/// walk does not because the identity costs a syscall per file — so this
/// happens without anybody passing a flag at all.
#[test]
fn a_comparison_across_two_hardlink_rules_says_so() {
    let home = tempfile::tempdir().unwrap();
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path().to_string_lossy().to_string();
    std::fs::write(tree.path().join("original.bin"), vec![3u8; 256 * 1024]).unwrap();
    for name in ["second.bin", "third.bin"] {
        std::fs::hard_link(tree.path().join("original.bin"), tree.path().join(name)).unwrap();
    }

    run(&[&root, "--hardlinks", "--snapshot"], home.path());
    let snapshot = find_snapshot(home.path()).to_string_lossy().to_string();

    let crossed = text(&run(
        &[
            &root,
            "--no-hardlinks",
            "--since",
            &snapshot,
            "--since-min",
            "1",
        ],
        home.path(),
    ));
    assert!(
        crossed.contains("counted a hardlinked file's bytes once")
            && crossed.contains("differs for that reason alone"),
        "a comparison across two rules said nothing about it:\n{crossed}"
    );

    // Under the same rule it is silent, because there is nothing to say.
    let same = text(&run(
        &[
            &root,
            "--hardlinks",
            "--since",
            &snapshot,
            "--since-min",
            "1",
        ],
        home.path(),
    ));
    assert!(
        !same.contains("hardlinked file's bytes"),
        "a comparison under one rule warned about two:\n{same}"
    );
    assert!(
        same.contains("Nothing changed"),
        "the same tree under the same rule changed:\n{same}"
    );
}

/// And a comparison across the other rule that decides what is counted.
///
/// Measured on Linux against `/run`, which has a handful of mounts inside it:
/// a record taken with `--cross-fs`, held against an ordinary scan, reported
/// **every file on every one of them as gone**. Nothing had been unmounted;
/// the second scan simply stopped where the first one had not.
///
/// The fixture here has no mount in it, which is deliberate: the sentence is
/// about the rules being different, and a test that needed a second filesystem
/// could only run where somebody can make one. What it pins is that the
/// difference is *stated* — the rule is knowable from the record, and this is
/// the only place it can be said.
#[test]
fn a_comparison_across_two_crossing_rules_says_so() {
    let home = tempfile::tempdir().unwrap();
    let tree = fixture();
    let root = tree.path().to_string_lossy().to_string();

    run(&[&root, "--cross-fs", "--snapshot"], home.path());
    let snapshot = find_snapshot(home.path()).to_string_lossy().to_string();

    let crossed = text(&run(
        &[&root, "--since", &snapshot, "--since-min", "1"],
        home.path(),
    ));
    assert!(
        crossed.contains("descended into other filesystems")
            && crossed.contains("differs for that reason alone"),
        "a comparison across two crossing rules said nothing:\n{crossed}"
    );

    // Under the same rule, nothing is said.
    let same = text(&run(
        &[
            &root,
            "--cross-fs",
            "--since",
            &snapshot,
            "--since-min",
            "1",
        ],
        home.path(),
    ));
    assert!(
        !same.contains("other filesystems"),
        "a comparison under one rule warned about two:\n{same}"
    );
}

/// Scanning a folder that cannot be read ends in a sentence, not in a zero.
///
/// It exited **0** with `~0 B on disk` over "1 entry could not be read" — a
/// number a script would take about a folder kosha never looked into. A path
/// that is a file, or is not there at all, already ends in a sentence and a
/// non-zero exit; this was the third way a root can be unusable, and the only
/// one that answered.
///
/// Unix-only for the fixture's sake: a mode with no bits on it is the cheapest
/// unreadable directory there is. The defect was found on Windows, on
/// `C:\System Volume Information`, and the code is the same on both — the core
/// test beside this one makes the folder on either platform.
#[test]
#[cfg(unix)]
fn a_folder_that_cannot_be_read_is_not_reported_as_empty() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("inside.bin"), vec![0u8; 4096]).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = run(&[&locked.to_string_lossy()], home.path());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        !out.status.success(),
        "it exited 0 for a folder it never read"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("could not be read"), "{said}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("0 B on disk"),
        "it printed a total for a folder it never read:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A comparison says which folder the record is of, and how much of it is here.
///
/// Measured: a snapshot of one folder held against another reported both of the
/// first folder's files as `gone` and the second folder's file as `new`, with
/// nothing to say the two were unrelated — files sitting exactly where they
/// always were, listed as deletions. The window refused the same pair outright,
/// which is the other half of the defect: comparing two roots is what
/// `kosha_core::compare` is written for, a directory against the backup that is
/// supposed to mirror it.
///
/// So neither a refusal nor silence. The mirror below is the case that has to
/// keep working, and it gets the root line without the warning.
#[test]
fn a_comparison_against_another_folder_says_so() {
    let home = tempfile::tempdir().unwrap();
    let tree = fixture();
    let root = tree.path().to_string_lossy().to_string();
    run(&[&root, "--snapshot"], home.path());
    let snapshot = find_snapshot(home.path()).to_string_lossy().to_string();

    // A folder with nothing in common with the record.
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("unrelated.bin"), vec![7u8; 128 * 1024]).unwrap();
    let elsewhere = text(&run(
        &[
            &other.path().to_string_lossy(),
            "--since",
            &snapshot,
            "--since-min",
            "1",
        ],
        home.path(),
    ));
    assert!(
        elsewhere.contains(&format!("the record is of {root}")),
        "the comparison does not say which folder the record is of:\n{elsewhere}"
    );
    assert!(
        elsewhere.contains("the difference between two folders rather than a change"),
        "a record with nothing in common reads as a list of deletions:\n{elsewhere}"
    );

    // A copy of the recorded folder, which is the comparison the contract is
    // for: named, because the root really is another one, and not warned about,
    // because all of the record is there.
    let mirror = tempfile::tempdir().unwrap();
    for (from, to) in [("docs", "docs"), ("media", "media")] {
        std::fs::create_dir_all(mirror.path().join(to)).unwrap();
        for entry in std::fs::read_dir(tree.path().join(from)).unwrap().flatten() {
            std::fs::copy(entry.path(), mirror.path().join(to).join(entry.file_name())).unwrap();
        }
    }
    for folder in ["cache", "empty"] {
        std::fs::create_dir_all(mirror.path().join(folder)).unwrap();
        for entry in std::fs::read_dir(tree.path().join(folder))
            .unwrap()
            .flatten()
        {
            std::fs::copy(
                entry.path(),
                mirror.path().join(folder).join(entry.file_name()),
            )
            .unwrap();
        }
    }
    let copied = text(&run(
        &[
            &mirror.path().to_string_lossy(),
            "--since",
            &snapshot,
            "--since-min",
            "1",
        ],
        home.path(),
    ));
    assert!(
        copied.contains(&format!("the record is of {root}")),
        "{copied}"
    );
    assert!(
        !copied.contains("two folders rather than a change"),
        "a faithful copy was called a different folder:\n{copied}"
    );

    // And the folder the record is actually of says neither thing.
    let itself = text(&run(
        &[&root, "--since", &snapshot, "--since-min", "1"],
        home.path(),
    ));
    assert!(!itself.contains("the record is of"), "{itself}");
    assert!(
        !itself.contains("two folders rather than a change"),
        "{itself}"
    );
}

/// A label cannot forge a row in the listing, or colour the terminal.
///
/// Measured: `--label` with an escape and a newline in it gave
/// `kosha --snapshots` a second row — a plausible date, `999.9 GiB`, a folder
/// nobody scanned — under a line that still said "1 snapshot". The same label
/// came back out of the header when the snapshot was opened, and one raw escape
/// reached the terminal with it.
///
/// A label is typed by whoever takes the snapshot, so on their own machine this
/// is a foot-gun rather than an attack. A snapshot file is not: `--since` exists
/// so that one can be handed to somebody else, and its label and its root come
/// out of that file.
#[test]
fn a_label_with_control_characters_stays_on_its_own_line() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = tree.path().to_string_lossy().to_string();
    let label = format!(
        "red{}[31m{}2026-01-01 00:00:00  9,999  999.9 GiB  /elsewhere  (forged)",
        char::from(27),
        char::from(10)
    );
    run(&[&path, "--snapshot", "--label", &label], home.path());

    let listing = text(&run(&["--snapshots"], home.path()));
    let header = {
        let snapshot = find_snapshot(home.path());
        text(&run(&[&snapshot.to_string_lossy()], home.path()))
    };

    for (where_, out) in [("the listing", &listing), ("the header", &header)] {
        assert!(
            !out.chars().any(|c| c.is_control() && c != '\n'),
            "a control character from a label reached {where_}:\n{out:?}"
        );
        assert!(
            out.contains("red?[31m?2026-01-01"),
            "the label is missing from {where_}:\n{out}"
        );
    }

    // One snapshot means one row: everything between the column headings and the
    // blank line that follows them.
    let rows = listing
        .lines()
        .skip_while(|line| !line.starts_with("TAKEN"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .count();
    assert_eq!(
        rows, 1,
        "the table grew a row it has no snapshot for:\n{listing}"
    );
    assert!(listing.contains("1 snapshot"), "{listing}");
}

/// A duplicate hunt against a record is refused: the record holds no bytes.
///
/// Measured on one snapshot, three times over, while the disk beneath it
/// changed: with both files as they were it reported the pair; with one of
/// them rewritten it reported none; with one deleted it reported "1 could not
/// be read". Three answers from one record, and none of them about the
/// record — the header above them said `SNAPSHOT`, which is the past.
///
/// The window has refused it from the start and says why in
/// `find_duplicates`: a snapshot records how big every file was, not a byte
/// of any of them, so the only way to answer "are these identical" is to read
/// the disk as it is now.
#[test]
fn a_duplicate_hunt_against_a_snapshot_is_refused() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    run(&[&tree.path().to_string_lossy(), "--snapshot"], home.path());
    let snapshot = find_snapshot(home.path()).to_string_lossy().to_string();

    for flags in [vec!["--duplicates"], vec!["--dupe-min", "1"]] {
        let mut args = vec![snapshot.as_str()];
        args.extend(flags.iter().copied());
        let out = run(&args, home.path());
        assert!(!out.status.success(), "`{flags:?}` hunted through a record");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("is a snapshot"), "{said}");
        assert!(said.contains("read the files themselves"), "{said}");
    }

    // Against a disk it is what it always was.
    let hunted = text(&run(
        &[
            &tree.path().to_string_lossy(),
            "--duplicates",
            "--dupe-min",
            "1",
        ],
        home.path(),
    ));
    assert!(hunted.contains("DUPLICATES"), "{hunted}");
}

/// Keeping a snapshot of a snapshot is refused, because it would be a lie.
///
/// Measured: `kosha <snapshot> --snapshot` wrote a second file holding the
/// same tree with this minute's stamp on it, and `--snapshots` then listed a
/// scan that never happened — the same folder, the same two entries, a date
/// fourteen minutes later. Worse than a wrong row: a comparison against the
/// copy takes that later stamp as "then", so everything that happened in
/// between is invisible.
///
/// The window has refused it from the start, in its own words: "Snapshotting
/// a snapshot is a copy of a file with a new date on it, which would be a lie
/// about when it was taken." The command line did it anyway, which is two
/// front ends of one program disagreeing about whether something is allowed.
#[test]
fn keeping_a_snapshot_of_a_snapshot_is_refused() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    run(&[&tree.path().to_string_lossy(), "--snapshot"], home.path());
    let first = find_snapshot(home.path()).to_string_lossy().to_string();

    let out = run(&[&first, "--snapshot", "--label", "a copy"], home.path());
    assert!(!out.status.success(), "it kept a copy of a record");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("already a snapshot"), "{said}");
    assert!(
        said.contains("never happened") || said.contains("stamped with now"),
        "the refusal does not say why: {said}"
    );

    // And the store still holds exactly the one that was taken of a disk.
    let _ = find_snapshot(home.path());

    // Browsing it is still what a snapshot is for.
    let browsed = text(&run(&[&first], home.path()));
    assert!(browsed.contains("SNAPSHOT"), "{browsed}");
}

/// A flag about how to scan a disk is refused when the path is a record of one.
///
/// Six of them were accepted and did nothing: `--depth`, `--cross-fs`, `--exact`,
/// `--mft`, `--image` and either hardlink flag. `kosha snapshot.kosha --mft` reads
/// like a promise to go and read a Master File Table, and opened a file. The rule
/// is the one above `--label` — a flag taken and ignored is worse than one refused
/// — and it had been applied to `--since` here and to nothing else.
///
/// `--owners` is the control: a snapshot carries the owners it was scanned with,
/// so asking for that table is asking the record something it knows.
#[test]
fn a_scan_flag_on_a_snapshot_is_refused_and_a_reporting_flag_is_not() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = tree.path().to_string_lossy().to_string();
    run(&[&path, "--snapshot", "--owners"], home.path());
    let snapshot = find_snapshot(home.path());
    let snapshot = snapshot.to_string_lossy().to_string();

    for flag in [
        "--depth 1",
        "--cross-fs",
        "--exact",
        "--mft",
        "--image",
        "--hardlinks",
        "--no-hardlinks",
    ] {
        let mut args = vec![snapshot.as_str()];
        args.extend(flag.split(' '));
        let out = run(&args, home.path());
        assert!(
            !out.status.success(),
            "`{flag}` on a snapshot exited 0 and did nothing"
        );
        let complaint = String::from_utf8_lossy(&out.stderr);
        let named = flag.split(' ').next().unwrap();
        assert!(
            complaint.contains(named) && complaint.contains("snapshot"),
            "the refusal of `{flag}` does not say what is wrong:\n{complaint}"
        );
    }

    // And the record still answers what it can answer.
    let owners = text(&run(&[&snapshot, "--owners"], home.path()));
    assert!(owners.contains("OWNERS"), "{owners}");
}

#[test]
fn a_path_that_is_not_there_says_so_and_fails() {
    let home = tempfile::tempdir().unwrap();
    let out = run(&["a-directory-that-does-not-exist"], home.path());

    assert!(!out.status.success(), "a missing path exited 0");
    let complaint = String::from_utf8_lossy(&out.stderr);
    assert!(complaint.contains("error"), "{complaint}");
    assert!(
        !complaint.contains("panicked"),
        "a missing path panicked:\n{complaint}"
    );
}

/// A hunt for duplicates over a disk image is refused, not answered wrongly.
///
/// **kosha said an image with an identical pair in it held no duplicates.**
/// A hunt reads bytes, and an image's paths name parts of one file rather
/// than files, so every read failed and the report came out as
///
/// ```text
/// examined 2 candidates, read 0 B
/// 2 files could not be read and were excluded
/// no identical files found
/// ```
///
/// over an ISO built with exactly one identical pair and one other file. The
/// last line is the false one and is the line a person reads.
///
/// Needs an image to point at. Build one —
///
/// ```text
/// mkdir -p t/a t/b && dd if=/dev/urandom of=t/a/twin.bin bs=1M count=4
/// cp t/a/twin.bin t/b/twin.bin
/// xorrisofs -rational-rock -o dupes.iso t
/// ```
///
/// — then `KOSHA_IMAGE_PROBE=/path/to/dupes.iso cargo test -p kosha-cli --
/// --ignored a_hunt_over_an_image`. It stands down rather than failing when
/// that is unset, for the reason the share probe in `kosha-fsops` gives.
#[test]
#[ignore = "needs a disk image; set KOSHA_IMAGE_PROBE"]
fn a_hunt_over_an_image_is_refused() {
    let Ok(image) = std::env::var("KOSHA_IMAGE_PROBE") else {
        println!("no KOSHA_IMAGE_PROBE: no image to point at, so nothing was checked");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let out = text(&run(&[&image, "--image", "--duplicates"], home.path()));

    assert!(out.contains("disk image"), "{out}");
    assert!(
        !out.contains("no identical files found"),
        "an answer about an image's contents: {out}"
    );
    assert!(
        !out.contains("could not be read"),
        "reported as unreadable files rather than as the wrong question: {out}"
    );
}

/// What can be refused without reading a disk is refused before the scan.
///
/// **A three-letter typo used to cost a whole scan.** Each of these was found
/// by the code that needed it, and all of that code runs after the tree is
/// built, so the disk was read to the end and *then* the mistake was
/// reported. Measured against a tree of 1.28 million entries, beside a plain
/// scan of the same tree at the same cache warmth:
///
/// ```text
/// plain scan        3.5 s
/// missing baseline  3.8 s
/// bad query         4.5 s
/// bad --out         4.6 s
/// ```
///
/// Cold, that scan is 32 seconds. Afterwards all three answer in hundredths.
///
/// Asserted on output rather than on a clock: a run that scanned prints its
/// root and its totals on standard output, so an empty standard output is
/// the evidence that nothing was read. A timing assertion would fail on a
/// loaded machine and pass on a fast one for the wrong reason.
#[test]
fn a_mistake_that_needs_no_disk_is_caught_before_the_disk() {
    let tree = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = tree.path().to_string_lossy().to_string();
    // A file inside a directory that is not there: the parent is the
    // missing part, and a bare `no-such-directory` would be a perfectly good
    // name for the export to take.
    let missing = tree.path().join("no-such-directory").join("out.csv");

    let cases: [(&str, Vec<&str>, &str); 3] = [
        (
            "a query that does not parse",
            vec!["--query", "sze > 1mb"],
            "is not a field",
        ),
        (
            "a baseline that is not there",
            vec!["--since", "definitely-not-here.csv"],
            "could not read",
        ),
        (
            "an export with nowhere to go",
            vec!["--export", "csv", "--out", missing.to_str().unwrap()],
            "could not write",
        ),
    ];

    for (what, flags, expected) in cases {
        let mut argv = vec![path.as_str()];
        argv.extend_from_slice(&flags);
        let out = run(&argv, home.path());

        assert!(!out.status.success(), "{what}: exited 0");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains(expected), "{what}: {said}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "",
            "{what}: it scanned the tree first"
        );
    }

    // And the same three, spelled correctly, still do their work.
    let good = text(&run(&[&path, "--query", "size > 1"], home.path()));
    assert!(good.contains("matched"), "{good}");

    // The export's own "exported N entries to ..." is a status line and goes
    // to standard error, so that standard output stays the document when the
    // export is going there. The file is the better assertion anyway.
    let baseline = home.path().join("baseline.csv");
    let out = run(
        &[
            &path,
            "--export",
            "csv",
            "--out",
            baseline.to_str().unwrap(),
        ],
        home.path(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(baseline.is_file(), "the export wrote nothing");
    assert!(
        std::fs::read_to_string(&baseline)
            .unwrap()
            .contains("holiday.bin"),
        "the export is not of this tree"
    );

    let compared = text(&run(
        &[&path, "--since", baseline.to_str().unwrap()],
        home.path(),
    ));
    assert!(compared.contains("Nothing changed"), "{compared}");
}
