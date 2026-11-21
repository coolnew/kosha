// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::{NodeId, ScanOptions, Scanner};

/// ```text
/// root/
///   big/deep/x.bin   8 KiB
///   mid/y.bin        4 KiB
///   small.txt        16 bytes
///   empty/
/// ```
fn fixture() -> (tempfile::TempDir, SizeTree) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("big/deep")).unwrap();
    std::fs::create_dir_all(root.join("mid")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("big/deep/x.bin"), vec![0u8; 8192]).unwrap();
    std::fs::write(root.join("mid/y.bin"), vec![0u8; 4096]).unwrap();
    std::fs::write(root.join("small.txt"), b"sixteen bytes.. ").unwrap();

    let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();
    (dir, tree)
}

fn taken_at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn creation_times_round_trip_and_version_one_remains_readable() {
    let (_dir, tree) = fixture();
    let mut times = vec![u64::MAX; tree.len()];
    times[0] = 0; // A real epoch timestamp is distinct from unknown.
    times[1] = 1_700_000_123;
    let tree = tree.with_creation_times(times.clone());
    let meta = Meta::of(&tree, taken_at(1_700_000_000), "birth times");
    let mut bytes = Vec::new();
    write(&tree, &meta, &mut bytes).unwrap();
    let (_, restored) = read(&bytes[..]).unwrap();
    assert_eq!(restored.creation_times(), times);
    assert_eq!(restored.created(NodeId::ROOT), Some(0));
    assert_eq!(restored.created(NodeId::from_index(2)), None);

    // Genuine v1 layout: no creation trailer, original node/name/owner bytes.
    let (mut header, mut body) = header_and_body(&bytes);
    body.truncate(body.len() - 8 - times.len() * 8);
    header[8..12].copy_from_slice(&1u32.to_le_bytes());
    let old = padded(&header, &body, 0);
    let (_, restored) = read(&old[..]).unwrap();
    assert!(restored.creation_times().is_empty());
    assert_eq!(restored.created(NodeId::ROOT), None);
    assert_eq!(restored.len(), tree.len());
}

#[test]
fn damaged_creation_table_is_refused_before_allocating_its_claim() {
    let (_dir, tree) = fixture();
    let count = tree.len();
    let tree = tree.with_creation_times(vec![1_700_000_000; count]);
    let meta = Meta::of(&tree, taken_at(1_700_000_000), "birth times");
    let mut bytes = Vec::new();
    write(&tree, &meta, &mut bytes).unwrap();
    let (header, body) = header_and_body(&bytes);
    let length_at = body.len() - 8 - count * 8;
    let mut invalid = body.clone();
    invalid[length_at..length_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        read(&padded(&header, &invalid, 0)[..]),
        Err(ReadError::Damaged("creation times do not match the tree"))
    ));
    let short = padded(&header, &body[..body.len() - 1], 0);
    assert!(read(&short[..]).is_err());
}

#[cfg(windows)]
#[test]
fn windows_walk_collects_birth_time_from_cached_metadata() {
    let (dir, tree) = fixture();
    let expected = std::fs::metadata(dir.path())
        .unwrap()
        .created()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(tree.created(tree.root()), Some(expected));
    for id in 0..tree.len() {
        assert!(tree.created(NodeId::from_index(id)).is_some());
    }
}

#[test]
fn keeping_an_automatic_record_preserves_its_original_date_and_header() {
    let (_source, tree) = fixture();
    let data = tempfile::tempdir().unwrap();
    let automatic = data.path().join("drive-cache");
    let explicit = data.path().join("snapshots");
    let taken = taken_at(1_700_000_000) + Duration::from_millis(789);
    let path = store::save_automatic_in(&automatic, &tree, taken).unwrap();
    let (mut meta, cached_tree) = store::open(&path).unwrap();
    let cache_bytes = std::fs::read(&path).unwrap();
    meta.label = "Kept drive scan".into();
    // Historical provenance must survive too, even for an older measurement.
    meta.tool = "original scan build".into();
    meta.exact_allocated = None;
    let kept = store::save_record_in(&explicit, &cached_tree, &meta).unwrap();
    let second = store::save_record_in(&explicit, &cached_tree, &meta).unwrap();
    assert_ne!(
        kept, second,
        "keeping twice never overwrites an explicit record"
    );
    let (saved_meta, saved_tree) = store::open(&kept).unwrap();
    assert_eq!(encode_header(&saved_meta), encode_header(&meta));
    assert_eq!(saved_meta.taken_ms, 789);
    assert_eq!(saved_tree.total_allocated(), tree.total_allocated());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        cache_bytes,
        "the automatic slot is untouched"
    );
    store::save_automatic_in(&automatic, &tree, taken_at(1_800_000_000)).unwrap();
    assert_eq!(
        store::read_meta_at(&kept).unwrap().at_ms(),
        meta.at_ms(),
        "later drive scans cannot change a kept historical record"
    );
}

#[test]
fn finished_scan_with_access_errors_survives_switching_drive_records() {
    let (_first_source, mut first) = fixture();
    // Access errors make totals partial, but do not mean traversal was stopped.
    first.stats.partial = true;
    first.stats.unreadable = 1;
    first.nodes[0].flags.insert(crate::Flags::PARTIAL);
    assert!(first.is_partial());
    assert!(!first.is_incomplete());

    let (_second_source, second) = fixture();
    let cache = tempfile::tempdir().unwrap();
    store::save_automatic_in(cache.path(), &first, taken_at(20)).unwrap();
    store::save_automatic_in(cache.path(), &second, taken_at(30)).unwrap();
    let (_, meta, reopened) = store::open_automatic_in(cache.path(), first.root_path()).unwrap();
    assert!(meta.partial);
    assert_eq!(reopened.stats().unreadable, 1);
    assert!(!reopened.is_incomplete());
    assert_eq!(reopened.stats().entries, first.stats().entries);
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 2);
}

#[test]
fn stopped_scans_keep_their_incomplete_status_when_reopened() {
    let (_source, mut tree) = fixture();
    tree.mark_cancelled();
    let (meta, reopened) = round_trip(&tree);
    assert!(meta.cancelled);
    assert!(reopened.is_incomplete());
    assert!(reopened.stats().cancelled);
    assert_eq!(
        reopened.stats().unreadable,
        0,
        "stopping is not an access error"
    );
    let cache = tempfile::tempdir().unwrap();
    assert!(store::save_automatic_in(cache.path(), &reopened, taken_at(30)).is_err());
}

#[test]
fn an_older_background_save_does_not_replace_a_newer_drive_record() {
    let (_source, tree) = fixture();
    let cache = tempfile::tempdir().unwrap();
    let path = store::save_automatic_in(cache.path(), &tree, taken_at(30)).unwrap();
    store::save_automatic_in(cache.path(), &tree, taken_at(20)).unwrap();
    assert_eq!(store::read_meta_at(&path).unwrap().taken, 30);
}

#[test]
fn automatic_scans_replace_one_slot_without_changing_explicit_snapshots() {
    let (_source, tree) = fixture();
    let data = tempfile::tempdir().unwrap();
    let explicit_dir = data.path().join("snapshots");
    let automatic_dir = data.path().join("drive-cache");
    let explicit = store::save_in(&explicit_dir, &tree, "Keep me", taken_at(10)).unwrap();
    let original = std::fs::read(&explicit).unwrap();
    let first = store::save_automatic_in(&automatic_dir, &tree, taken_at(20)).unwrap();
    let second = store::save_automatic_in(&automatic_dir, &tree, taken_at(30)).unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read_dir(&automatic_dir).unwrap().count(), 1);
    assert_eq!(store::open(&second).unwrap().0.taken, 30);
    assert_eq!(std::fs::read(explicit).unwrap(), original);
    assert_eq!(
        store::open_automatic_in(&automatic_dir, tree.root_path())
            .unwrap()
            .1
            .taken,
        30
    );
}

#[test]
fn corrupt_or_mismatched_drive_records_are_refused() {
    let (_source, tree) = fixture();
    let cache = tempfile::tempdir().unwrap();
    assert!(store::open_automatic_in(cache.path(), tree.root_path()).is_err());
    let path = store::automatic_path_in(cache.path(), tree.root_path());
    std::fs::write(&path, b"broken").unwrap();
    assert!(store::open_automatic_in(cache.path(), tree.root_path()).is_err());
    let (_other_source, other_tree) = fixture();
    let staged = store::save_in(cache.path(), &other_tree, "wrong root", taken_at(20)).unwrap();
    std::fs::rename(staged, path).unwrap();
    assert!(store::open_automatic_in(cache.path(), tree.root_path()).is_err());
}

#[test]
fn an_unfinished_scan_preserves_the_previous_automatic_record() {
    let (source, tree) = fixture();
    let cache = tempfile::tempdir().unwrap();
    let path = store::save_automatic_in(cache.path(), &tree, taken_at(20)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let partial = Scanner::new(ScanOptions {
        max_depth: Some(1),
        ..Default::default()
    })
    .scan(source.path())
    .unwrap();
    assert!(store::save_automatic_in(cache.path(), &partial, taken_at(30)).is_err());
    let cancelled = Scanner::new(ScanOptions::default())
        .with_cancel(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )))
        .scan(source.path())
        .unwrap();
    assert!(store::save_automatic_in(cache.path(), &cancelled, taken_at(40)).is_err());
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn automatic_slots_are_separate_for_different_roots() {
    let dir = std::path::Path::new("cache");
    assert_ne!(
        store::automatic_path_in(dir, std::path::Path::new("/one")),
        store::automatic_path_in(dir, std::path::Path::new("/two"))
    );
    assert!(store::same_root(
        std::path::Path::new("/one/"),
        std::path::Path::new("/one")
    ));
    if cfg!(windows) {
        assert!(store::same_root(
            std::path::Path::new("C:/"),
            std::path::Path::new("c:\\")
        ));
    }
}

#[cfg(windows)]
#[test]
fn a_failed_windows_cache_replacement_preserves_the_old_file() {
    let (_source, tree) = fixture();
    let cache = tempfile::tempdir().unwrap();
    let path = store::save_automatic_in(cache.path(), &tree, taken_at(20)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let original_permissions = std::fs::metadata(&path).unwrap().permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    let outcome = store::save_automatic_in(cache.path(), &tree, taken_at(30));
    std::fs::set_permissions(&path, original_permissions).unwrap();
    assert!(outcome.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);
}

fn round_trip(tree: &SizeTree) -> (Meta, SizeTree) {
    let meta = Meta::of(tree, taken_at(1_700_000_000), "");
    let mut buf = Vec::new();
    write(tree, &meta, &mut buf).expect("a snapshot writes");
    read(buf.as_slice()).expect("and reads back")
}

/// Every path through the window walks the arena by index. A snapshot that
/// came back with one node different would be a tree that browses wrongly,
/// and there is no later check that would catch it.
#[test]
fn a_snapshot_round_trips_every_node() {
    let (_dir, original) = fixture();
    let (_meta, restored) = round_trip(&original);

    assert_eq!(restored.len(), original.len());
    for index in 0..original.len() {
        let id = NodeId::from_index(index);
        let before = original.node(id);
        let after = restored.node(id);
        assert_eq!(before.logical, after.logical, "at {index}");
        assert_eq!(before.allocated, after.allocated, "at {index}");
        assert_eq!(before.parent, after.parent, "at {index}");
        assert_eq!(before.first_child, after.first_child, "at {index}");
        assert_eq!(before.child_count, after.child_count, "at {index}");
        assert_eq!(before.subtree_files, after.subtree_files, "at {index}");
        assert_eq!(before.mtime, after.mtime, "at {index}");
        assert_eq!(before.depth, after.depth, "at {index}");
        assert_eq!(before.flags.bits(), after.flags.bits(), "at {index}");
        // The offset may legitimately differ if the arena were rebuilt; the
        // name it resolves to may not.
        assert_eq!(
            original.name_lossy(id),
            restored.name_lossy(id),
            "at {index}"
        );
    }
}

/// The numbers the window puts on screen have to survive, not just the shape.
#[test]
fn a_snapshot_round_trips_the_totals() {
    let (_dir, original) = fixture();
    let (meta, restored) = round_trip(&original);

    assert_eq!(restored.root_path(), original.root_path());
    assert_eq!(
        restored.total_allocated(),
        original.total_allocated(),
        "the headline number changed"
    );
    assert_eq!(meta.entries, original.stats().entries);
    assert_eq!(meta.dirs, original.stats().dirs);
    assert_eq!(meta.files, original.stats().files);
    assert_eq!(meta.root, original.root_path());
    assert_eq!(meta.taken, 1_700_000_000);
}

/// A snapshot that keeps "this scan was incomplete" and drops "and here is
/// how much it could not read" contradicts itself on screen. Everything the
/// scan knew about itself travels with the tree.
#[test]
fn a_snapshot_keeps_what_the_scan_knew_about_itself() {
    let (_dir, mut original) = fixture();
    // **What was found inside a disk image**, which is the one thing here that
    // is not a number and the only one that says *which* filesystem, and which
    // partition of how many, a total came out of. A snapshot that kept the
    // total and dropped that would be a record nobody can trace back to a
    // decision, which is the thing this note exists to prevent. The walk that
    // built the fixture has none, so one is put on it.
    original.stats.image = Some("UDF · revision 2.50, metadata partition".into());
    let (meta, restored) = round_trip(&original);
    let before = original.stats();
    let after = restored.stats();

    assert_eq!(after.image, before.image);
    assert_eq!(meta.image, "UDF · revision 2.50, metadata partition");
    assert_eq!(after.unreadable, before.unreadable);
    assert_eq!(after.skipped_other_device, before.skipped_other_device);
    assert_eq!(after.hardlinks_deduped, before.hardlinks_deduped);
    assert_eq!(after.max_depth, before.max_depth);
    assert_eq!(after.partial, before.partial);
    assert_eq!(after.strategy, before.strategy);
    assert!(
        before.max_depth > 0,
        "the fixture is too flat to test depth"
    );

    // Milliseconds, because that is what the header stores — a scan timed in
    // microseconds is not a fact worth a wider field.
    assert_eq!(
        meta.duration_ms,
        before.duration.as_millis().min(u128::from(u64::MAX)) as u64
    );
}

/// A path with a directory tree in it is a path; names have to come back
/// byte-identical, including the ones no encoding will accept.
#[test]
fn names_survive_whatever_they_are() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Spaces, dots, unicode, and a name that is all of them.
    std::fs::write(root.join("a file with spaces.txt"), b"x").unwrap();
    std::fs::write(root.join("\u{5b57}\u{5178}.bin"), b"x").unwrap();
    std::fs::write(root.join(".hidden"), b"x").unwrap();
    let original = Scanner::new(ScanOptions::default()).scan(root).unwrap();

    let (_meta, restored) = round_trip(&original);

    let mut before: Vec<String> = (0..original.len())
        .map(|i| original.name_lossy(NodeId::from_index(i)).to_string())
        .collect();
    let mut after: Vec<String> = (0..restored.len())
        .map(|i| restored.name_lossy(NodeId::from_index(i)).to_string())
        .collect();
    before.sort();
    after.sort();
    assert_eq!(before, after);
}

/// Reading a snapshot back is meant to be as good as having scanned: the
/// window builds its rows by walking children, so that walk has to work.
#[test]
fn a_restored_tree_browses() {
    let (_dir, original) = fixture();
    let (_meta, restored) = round_trip(&original);

    let mut names: Vec<String> = restored
        .children(restored.root())
        .map(|id| restored.name_lossy(id).to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["big", "empty", "mid", "small.txt"]);

    // And all the way down, which is what the arena invariants are for.
    let big = restored
        .children(restored.root())
        .find(|&id| restored.name_lossy(id) == "big")
        .expect("big is there");
    let deep = restored.children(big).next().expect("big has a child");
    assert_eq!(restored.name_lossy(deep), "deep");
    assert_eq!(
        restored.node(deep).allocated,
        original
            .children(
                original
                    .children(original.root())
                    .find(|&id| original.name_lossy(id) == "big")
                    .unwrap()
            )
            .next()
            .map(|id| original.node(id).allocated)
            .unwrap()
    );
}

/// Listing a directory of snapshots must not inflate the trees in it. That is
/// the whole reason the header is outside the compressed body.
#[test]
fn the_header_reads_without_the_body() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1_700_000_000), "before the cleanup");
    let mut buf = Vec::new();
    write(&tree, &meta, &mut buf).unwrap();

    // Only the header is handed over. If reading it needed the body, this
    // would fail rather than pass — which is the point of the test.
    let header_end = 8 + 4 + 4 + u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
    let read_back = read_meta(&buf[..header_end]).expect("the header stands alone");

    assert_eq!(read_back, meta);
    assert_eq!(read_back.label, "before the cleanup");
}

/// Both front ends ask this before deciding what a path is. A `.kosha` that is
/// not one must not be browsed as a tree, and a snapshot somebody renamed must
/// still open.
#[test]
fn a_snapshot_is_recognised_by_its_contents() {
    let dir = tempfile::tempdir().unwrap();
    let (_fixture, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1), "");

    let renamed = dir.path().join("holiday-photos.txt");
    let mut buf = Vec::new();
    write(&tree, &meta, &mut buf).unwrap();
    std::fs::write(&renamed, &buf).unwrap();
    assert!(
        is_snapshot(&renamed),
        "a renamed snapshot was not recognised"
    );

    let liar = dir.path().join("not-really.kosha");
    std::fs::write(
        &liar,
        b"path,name,kind,on_disk
",
    )
    .unwrap();
    assert!(!is_snapshot(&liar), "a CSV named .kosha was taken for one");

    let missing = dir.path().join("nothing-here.kosha");
    assert!(!is_snapshot(&missing));

    // Shorter than the magic, which is the case a naive read_exact gets wrong.
    let stub = dir.path().join("stub.kosha");
    std::fs::write(&stub, b"KOSH").unwrap();
    assert!(!is_snapshot(&stub));
}

#[test]
fn a_file_that_is_not_a_snapshot_is_refused() {
    assert!(matches!(
        read(b"not a snapshot at all".as_slice()),
        Err(ReadError::NotASnapshot)
    ));
    assert!(matches!(read(b"".as_slice()), Err(ReadError::NotASnapshot)));
    // A PNG, because a file picker will hand us one eventually.
    assert!(matches!(
        read([0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0].as_slice()),
        Err(ReadError::NotASnapshot)
    ));
}

/// A format from the future is a different sentence from a damaged file, and
/// the message has to say which — one of them is fixed by updating kosha.
#[test]
fn a_newer_format_says_so() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1), "");
    let mut buf = Vec::new();
    write(&tree, &meta, &mut buf).unwrap();
    buf[8..12].copy_from_slice(&(FORMAT + 7).to_le_bytes());

    match read(buf.as_slice()) {
        Err(ReadError::Newer { found }) => assert_eq!(found, FORMAT + 7),
        other => panic!("{other:?}"),
    }
}

/// The arena's accessors index without bounds checks because the scanner
/// cannot produce a bad one. A file can, so a snapshot is checked before it
/// becomes a tree — otherwise a corrupt byte is a panic several screens later,
/// in the treemap or the query engine.
#[test]
fn a_damaged_arena_is_refused_rather_than_browsed() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1), "");
    let mut good = Vec::new();
    write(&tree, &meta, &mut good).unwrap();

    // Truncation, which is what a full disk during a write leaves behind.
    for keep in [20, 40, good.len() / 2, good.len() - 1] {
        assert!(
            read(&good[..keep]).is_err(),
            "a snapshot truncated to {keep} bytes was accepted"
        );
    }
}

/// The other half of what the checksum is for. A flipped bit in the middle of
/// the body would otherwise decode into a tree with one wrong number in it,
/// which is worse than a file that will not open: nothing downstream would
/// ever question it.
#[test]
fn a_flipped_bit_is_caught() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1), "");
    let mut good = Vec::new();
    write(&tree, &meta, &mut good).unwrap();

    let header_end = 8 + 4 + 4 + u32::from_le_bytes(good[12..16].try_into().unwrap()) as usize;
    let mut caught = 0;
    // Every byte of the compressed body, one at a time. Some corruptions are
    // caught by the decompressor and some only by the checksum; what matters
    // is that none of them is caught by nobody.
    for at in header_end..good.len() {
        let mut damaged = good.clone();
        damaged[at] ^= 0b0001_0000;
        match read(damaged.as_slice()) {
            Err(_) => caught += 1,
            Ok((_, other)) => assert_eq!(
                other.total_allocated(),
                tree.total_allocated(),
                "byte {at} changed a number and nothing noticed"
            ),
        }
    }
    assert!(caught > 0, "not one corruption was detected");
}

#[test]
fn a_snapshot_is_smaller_than_the_tree_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Enough entries, with repetitive names, for compression to have something
    // to work with — which is the shape of every real tree.
    for group in 0..40 {
        let sub = root.join(format!("package-{group:03}"));
        std::fs::create_dir_all(&sub).unwrap();
        for file in 0..25 {
            std::fs::write(sub.join(format!("module-{file:03}.rs")), b"x").unwrap();
        }
    }
    let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();
    let meta = Meta::of(&tree, taken_at(1), "");
    let mut buf = Vec::new();
    write(&tree, &meta, &mut buf).unwrap();

    let arena = tree.len() * 48;
    assert!(
        buf.len() < arena / 2,
        "{} entries: {} bytes on disk against {arena} of arena",
        tree.len(),
        buf.len()
    );
}

/// Two snapshots of an unchanged tree have to be byte-identical, or a
/// comparison between them reports changes that never happened. The same rule
/// the CSV export follows, for the same reason.
#[test]
fn the_same_tree_writes_the_same_bytes() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1_700_000_000), "");

    let mut first = Vec::new();
    write(&tree, &meta, &mut first).unwrap();
    let mut second = Vec::new();
    write(&tree, &meta, &mut second).unwrap();

    assert_eq!(first, second);
}

#[test]
fn an_age_needs_a_clock_that_makes_sense() {
    let meta = Meta {
        taken: 1_000,
        taken_ms: 0,
        root: PathBuf::from("/x"),
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
        import: None,
    };

    assert_eq!(meta.age(taken_at(1_600)), Some(Duration::from_secs(600)));
    // Stamped in the future: a clock somewhere is wrong, and "in -3 days" is
    // not an improvement on saying nothing.
    assert_eq!(meta.age(taken_at(900)), None);
}

/// Snapshots sort by name into the order they were taken, so the window can
/// list a directory without opening a single file to order it.
#[test]
fn file_names_sort_by_when_they_were_taken() {
    let root = Path::new("/home/me/projects");
    let mut names = [
        file_name(root, taken_at(1_700_000_200)),
        file_name(root, taken_at(1_699_000_000)),
        file_name(root, taken_at(1_700_000_100)),
    ];
    names.sort();

    let seconds: Vec<&str> = names.iter().map(|n| &n[..12]).collect();
    assert_eq!(seconds, ["001699000000", "001700000100", "001700000200"]);
    assert!(names[0].ends_with(".kosha"), "{names:?}");
    // Named for the end of the path, not the start. Taking the beginning and
    // truncating gave every snapshot under one home directory the same name.
    assert!(names[0].contains("me-projects"), "{names:?}");
    assert!(!names[0].contains("home"), "{names:?}");
}

/// Two folders deep in the same tree must not produce the same filename, which
/// is what a slug built from the front of the path did.
#[test]
fn snapshots_of_different_folders_are_named_differently() {
    let a = file_name(
        Path::new("/home/me/Library/Application Support/one"),
        taken_at(1),
    );
    let b = file_name(
        Path::new("/home/me/Library/Application Support/two"),
        taken_at(1),
    );
    assert_ne!(a, b, "both folders got the same name");
    assert!(a.contains("one"), "{a}");
    assert!(b.contains("two"), "{b}");
}

/// A root that slugs to nothing still has to produce a filename.
#[test]
fn every_root_has_a_file_name() {
    for root in ["/", "C:\\", "//server/share", "..."] {
        let name = file_name(Path::new(root), taken_at(1));
        assert!(name.ends_with(".kosha"), "{root} -> {name}");
        assert!(
            !name.contains("..") && !name.contains('/') && !name.contains('\\'),
            "{root} -> {name}"
        );
    }
}

/// The bug this whole pair of changes exists for.
///
/// Two snapshots of one folder in the same second used to be one snapshot: the
/// filename was built from whole seconds, the second write landed on the
/// first's name, and the listing afterwards showed a single row. Nothing said
/// anything had been lost. A person clicking "Keep a snapshot" twice, which is
/// what somebody does when they are not sure the first click took, kept the
/// second one and destroyed the first.
#[test]
fn a_second_snapshot_never_replaces_the_first() {
    let (_dir, tree) = fixture();
    let store = tempfile::tempdir().unwrap();

    // The same instant, to the millisecond, which is stricter than anything a
    // pair of button presses can manage.
    let moment = taken_at(1_700_000_000);
    let first = store::save_in(store.path(), &tree, "before", moment).unwrap();
    let second = store::save_in(store.path(), &tree, "after", moment).unwrap();

    assert_ne!(first, second, "the second snapshot took the first's name");
    assert!(first.exists(), "the first snapshot is gone");
    assert!(second.exists());

    // And both are readable, which is the part a unique name alone would not
    // prove: the loser of the name could have been left half-written.
    assert_eq!(store::read_meta_at(&first).unwrap().label, "before");
    assert_eq!(store::read_meta_at(&second).unwrap().label, "after");
}

/// Ten in a row, from a loop, which is the case where nobody is watching.
#[test]
fn every_snapshot_taken_is_a_snapshot_kept() {
    let (_dir, tree) = fixture();
    let store = tempfile::tempdir().unwrap();

    let moment = taken_at(1_700_000_000);
    for _ in 0..10 {
        store::save_in(store.path(), &tree, "", moment).unwrap();
    }

    let kept = std::fs::read_dir(store.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == EXTENSION))
        .count();
    assert_eq!(kept, 10);
}

/// Nothing is left behind when a snapshot is written.
///
/// The temporary file is how a half-written snapshot never appears in the
/// listing, and a temporary file that outlives its write is a snapshot's worth
/// of disk that nothing will ever clean up — in the directory somebody opened
/// this tool to stop growing.
#[test]
fn writing_a_snapshot_leaves_no_scratch_file() {
    let (_dir, tree) = fixture();
    let store = tempfile::tempdir().unwrap();
    store::save_in(store.path(), &tree, "", taken_at(1_700_000_000)).unwrap();

    let strays: Vec<_> = std::fs::read_dir(store.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter(|name| !name.to_string_lossy().ends_with(EXTENSION))
        .collect();
    assert!(strays.is_empty(), "left behind {strays:?}");
}

/// Milliseconds survive the round trip, or two snapshots one after the other
/// are back to being the same age with nothing to order them by.
#[test]
fn the_millisecond_is_kept() {
    let (_dir, tree) = fixture();
    let taken = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    let meta = Meta::of(&tree, taken, "");
    assert_eq!(meta.taken, 1_700_000_000);
    assert_eq!(meta.taken_ms, 123);

    let mut buf = Vec::new();
    write(&tree, &meta, &mut buf).unwrap();
    let back = read_meta(buf.as_slice()).unwrap();
    assert_eq!(back.taken_ms, 123);
    assert_eq!(back.at_ms(), 1_700_000_000_123);
}

/// A snapshot written before this field existed has no millisecond in it, and
/// reads back at the start of its second rather than failing.
#[test]
fn a_snapshot_without_a_millisecond_still_reads() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1_700_000_000), "");
    let mut buf = Vec::new();
    write(&tree, &meta, &mut buf).unwrap();

    // Rewrite the header exactly as an older kosha would have: without the
    // key at all.
    let header_len = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
    let header = &buf[16..16 + header_len];
    let mut stripped = Vec::with_capacity(header.len());
    let mut at = 0usize;
    while at < header.len() {
        let start = at;
        let key_len = u16::from_le_bytes(header[at..at + 2].try_into().unwrap()) as usize;
        at += 2;
        let key = std::str::from_utf8(&header[at..at + key_len])
            .unwrap()
            .to_string();
        at += key_len;
        let value_len = u32::from_le_bytes(header[at..at + 4].try_into().unwrap()) as usize;
        at += 4 + value_len;
        if key != "taken-ms" {
            stripped.extend_from_slice(&header[start..at]);
        }
    }

    let mut older = Vec::new();
    older.extend_from_slice(&buf[..12]);
    older.extend_from_slice(&(stripped.len() as u32).to_le_bytes());
    older.extend_from_slice(&stripped);
    older.extend_from_slice(&buf[16 + header_len..]);

    let back = read_meta(older.as_slice()).expect("an older snapshot still reads");
    assert_eq!(back.taken, 1_700_000_000);
    assert_eq!(back.taken_ms, 0);
    // And the body is still there, untouched by the header surgery.
    assert!(read(older.as_slice()).is_ok());
}

/// The other half of that sweep: the header, where the lengths live.
///
/// [`a_flipped_bit_is_caught`] starts after the header and checks that a
/// damaged *body* is noticed. The header is the more dangerous half and was
/// not covered at all: it is uncompressed, it is parsed before anything is
/// checksummed, and every number in it is a length or a count that something
/// downstream trusts. A byte there does not corrupt data — it tells the reader
/// how much memory to reserve.
///
/// The same sweep over an ext4 superblock found a field that asked for a 68 GB
/// allocation, and a failed allocation aborts the process rather than
/// returning an error. This one is at least kosha's own format, written by
/// kosha — but a snapshot lives on disk for months, which is long enough for a
/// disk to rot a byte, and it is the one file this application asks people to
/// keep.
#[test]
fn no_flipped_bit_in_the_header_can_panic_the_reader() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1_700_000_000), "a label");
    let mut good = Vec::new();
    write(&tree, &meta, &mut good).unwrap();

    let header_end = 8 + 4 + 4 + u32::from_le_bytes(good[12..16].try_into().unwrap()) as usize;
    assert!(header_end > 16, "the fixture wrote no header");

    let mut tried = 0usize;
    for at in 0..header_end {
        for bit in 0..8u8 {
            let mut damaged = good.clone();
            damaged[at] ^= 1 << bit;
            // Both answers are correct — a header that still parses describes
            // a tree that still reads, and one that does not is refused. What
            // must not happen is a panic, or an allocation nobody can serve.
            let _ = read(damaged.as_slice());
            let _ = read_meta(damaged.as_slice());
            tried += 1;
        }
    }
    assert_eq!(tried, header_end * 8);
    assert!(tried > 1_000, "only {tried} mutations");
}

/// A snapshot of a shallow scan records that it was shallow.
///
/// Snapshots carry `partial` to warn that comparisons against an incomplete
/// baseline may report changes that never happened,
/// and a depth-limited scan was the one way of being partial that never set it.
/// Compared against a full scan, such a snapshot reported every directory below
/// the ceiling as new.
#[test]
fn a_snapshot_of_a_depth_limited_scan_says_so() {
    let (dir, _whole) = fixture();
    let shallow = Scanner::new(ScanOptions {
        max_depth: Some(1),
        ..ScanOptions::default()
    })
    .scan(dir.path())
    .unwrap();
    assert!(shallow.stats().depth_limited, "the fixture is too flat");

    let meta = Meta::of(&shallow, taken_at(1), "");
    assert!(meta.partial);
    assert!(meta.depth_limited);

    let mut buf = Vec::new();
    write(&shallow, &meta, &mut buf).unwrap();
    let (read_back, tree) = read(buf.as_slice()).unwrap();
    assert!(
        read_back.partial,
        "the floor did not survive the round trip"
    );
    assert!(read_back.depth_limited);
    assert!(tree.is_partial(), "the tree it came back as is not a floor");
}

// ------------------------------------------------- a small file, a large read

/// The plain body of a snapshot, and the header that goes in front of it.
fn header_and_body(good: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let header_end = 8 + 4 + 4 + u32::from_le_bytes(good[12..16].try_into().unwrap()) as usize;
    let mut body = Vec::new();
    ZlibDecoder::new(&good[header_end..])
        .read_to_end(&mut body)
        .expect("the fixture's own body inflates");
    (good[..header_end].to_vec(), body)
}

/// A header, a body, and `padding` bytes of nothing, all in one zlib stream.
///
/// The zeros go in a megabyte at a time: the point of the exercise is that a
/// quarter of a gigabyte of stream costs a quarter of a megabyte of file, and a
/// test that held the inflated form would be the very thing it is checking.
fn padded(header: &[u8], body: &[u8], padding: usize) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(header.to_vec(), Compression::default());
    encoder.write_all(body).unwrap();
    let chunk = vec![0u8; 1024 * 1024];
    let mut left = padding;
    while left > 0 {
        let take = left.min(chunk.len());
        encoder.write_all(&chunk[..take]).unwrap();
        left -= take;
    }
    encoder.finish().unwrap()
}

/// How many bytes of the file the reader actually pulled.
struct Counted<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Read for Counted<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let take = out.len().min(self.bytes.len() - self.at);
        out[..take].copy_from_slice(&self.bytes[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }
}

/// Read a file, and say how much of it was read.
fn consumed(file: &[u8]) -> (Result<(), ReadError>, usize) {
    let mut source = Counted { bytes: file, at: 0 };
    let outcome = read(&mut source).map(|_| ());
    (outcome, source.at)
}

/// A quarter of a gigabyte of stream, which is a quarter of a megabyte of file.
const PADDING: usize = 256 * 1024 * 1024;

/// What a reader that refuses on the numbers is allowed to have pulled: the
/// decompressor reads the file in 32 KiB bites, so one or two of those.
const BITE: usize = 128 * 1024;

/// A snapshot is a record somebody keeps, copies to another machine, and sends
/// to a colleague — which makes its body somebody else's bytes. zlib turns a
/// megabyte of those into a gigabyte of stream, so a file that claims a name
/// arena nothing in it could reference has to be refused by the claim, before a
/// `Vec` is grown to hold what the claim asked for.
#[test]
fn a_name_arena_larger_than_the_tree_could_reference_is_refused_unread() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1), "");
    let mut good = Vec::new();
    write(&tree, &meta, &mut good).unwrap();
    let (header, body) = header_and_body(&good);

    // The body is a count, that many records, and then the arena's length.
    let count = u64::from_le_bytes(body[0..8].try_into().unwrap()) as usize;
    let at = 8 + count * RECORD;
    let mut claim = body[..at].to_vec();
    claim.extend_from_slice(&(1u64 << 40).to_le_bytes());

    let file = padded(&header, &claim, PADDING);
    let (outcome, pulled) = consumed(&file);

    assert!(outcome.is_err(), "a terabyte of names was accepted");
    assert!(
        pulled < BITE,
        "the reader inflated {pulled} of {} bytes before refusing a claim it \
         could have refused on sight",
        file.len()
    );
}

/// The other end of the same file. A tree that reads perfectly and then has a
/// gigabyte of padding behind it is already refused — "there is more here than
/// a tree" — but refusing it after reading all of it is refusing it by filling
/// memory with it.
#[test]
fn padding_behind_the_tree_is_refused_at_the_first_byte_of_it() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1), "");
    let mut good = Vec::new();
    write(&tree, &meta, &mut good).unwrap();
    let (header, body) = header_and_body(&good);

    let file = padded(&header, &body, PADDING);
    let (outcome, pulled) = consumed(&file);

    assert!(outcome.is_err(), "a padded snapshot was accepted");
    assert!(
        pulled < BITE,
        "the reader inflated {pulled} of {} bytes of padding before saying \
         there was more here than a tree",
        file.len()
    );
}

/// Every length a copy can stop at, and what each one says.
///
/// **Truncation is the likeliest damage a snapshot suffers** — a copy
/// interrupted, a download that died, a file pulled off a failing disk — and it
/// was the one kind of damage that answered in somebody else's words. Measured
/// over all 612 lengths of one snapshot, in three stretches:
///
/// ```text
///     0    not a kosha snapshot                  the format's own
///     8    failed to fill whole buffer           `read_exact`'s
///   412    incomplete deflate stream             zlib's
///   607    the snapshot is damaged: ...          the format's own
/// ```
///
/// Opening a damaged snapshot should produce a sentence about the snapshot,
/// and the middle two are not sentences about a snapshot. They are also the two
/// stretches that cover almost the whole file.
///
/// The bit-flip sweeps beside this one never reached them: a flipped bit leaves
/// the length alone, so every read still had all the bytes it asked for.
#[test]
fn a_snapshot_cut_anywhere_says_so_in_this_format_s_own_words() {
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1_700_000_000), "a label");
    let mut good = Vec::new();
    write(&tree, &meta, &mut good).unwrap();
    assert!(good.len() > 64, "the fixture is too small to cut usefully");

    let mut seen: Vec<String> = Vec::new();
    for at in 0..good.len() {
        let cut = &good[..at];
        let error = match read(cut) {
            Ok(_) => panic!("{at} of {} bytes read back as a whole tree", good.len()),
            Err(error) => error,
        };
        // Never a bare I/O error: this input is a slice, so end-of-file is the
        // *file's* problem and has to be described as one.
        assert!(
            !matches!(error, ReadError::Io(_)),
            "cut at {at} answered with a raw I/O error: {error}"
        );
        let said = error.to_string();
        if !seen.contains(&said) {
            seen.push(said);
        }

        // And the header reader alone, which is what the sidebar calls on every
        // file it lists. It succeeds once the header is whole — that is the
        // point of the header sitting outside the compression — and until then
        // it is held to the same rule.
        if let Err(error) = read_meta(cut) {
            assert!(
                !matches!(error, ReadError::Io(_)),
                "cut at {at} answered `read_meta` with a raw I/O error: {error}"
            );
        }
    }

    // The three stretches, named. Each is a different thing to have gone wrong
    // and each tells the reader something different about the file in hand.
    assert_eq!(
        seen,
        vec![
            "not a kosha snapshot",
            "the snapshot is damaged: it ends before its header does",
            "the snapshot is damaged: it ends before its tree does",
            "the snapshot is damaged: it ends before its checksum does",
        ],
        "the sentences a cut snapshot gives, in the order a cut reaches them"
    );
}

/// Nothing that can block is asked whether it is a snapshot.
///
/// **`kosha CON` hung.** No output, no progress, no way out but Ctrl-C — and
/// the debugger put the whole process in `is_snapshot`, on the `read_exact`
/// that takes a path's first eight bytes to see whether they are the magic:
///
/// ```text
/// 00  ntdll!NtReadFile
/// 01  std::sys::pal::windows::handle::Handle::synchronous_read
/// 03  std::io::default_read_exact<std::fs::File>
/// 05  kosha_core::snapshot::is_snapshot
/// 06  kosha::run
/// ```
///
/// Reading a console waits for somebody to type. A serial port waits the same
/// way, and on Unix a FIFO blocks in `File::open` before any read: it waits for
/// a writer that may never come. This question is asked of every path kosha is
/// given, so every one of those is a hang at the first thing the program does.
///
/// Both halves run under a clock, because the failure being tested for is *no
/// answer at all* and a test that simply calls it would hang the suite instead
/// of failing it.
#[test]
fn nothing_that_can_block_is_asked_whether_it_is_a_snapshot() {
    fn within(what: &str, path: std::path::PathBuf) -> bool {
        let (tell, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tell.send(is_snapshot(&path));
        });
        match heard.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(answer) => answer,
            Err(_) => panic!("`is_snapshot` never came back from {what}"),
        }
    }

    #[cfg(windows)]
    {
        // The console, which answers a read only when a person types into it.
        assert!(!within("the console", std::path::PathBuf::from("CON")));
        assert!(!within(
            "the console input",
            std::path::PathBuf::from("CONIN$")
        ));
    }

    #[cfg(unix)]
    {
        // A FIFO with no writer, which blocks in `open` and not in `read`.
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo said {made}");
        assert!(!within("a fifo", fifo));
    }

    // And a real one is still recognised, which is the whole point of asking.
    let (_dir, tree) = fixture();
    let meta = Meta::of(&tree, taken_at(1), "");
    let mut bytes = Vec::new();
    write(&tree, &meta, &mut bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let kept = dir.path().join("one.kosha");
    std::fs::write(&kept, &bytes).unwrap();
    assert!(within("a snapshot", kept));
    assert!(!within("a directory", dir.path().to_path_buf()));
}

/// A snapshot records the counting rule, not only what it counted.
///
/// **Two rules are two different questions**, and `hardlinks_deduped` cannot
/// tell them apart: a tree with no hardlinks folds none under either. Measured
/// with five names for one 8 MiB file — snapshotted with folding on, scanned
/// with it off — the comparison reported `+32.0 MiB` of growth across four
/// files that nothing had touched.
///
/// On Windows the rule belongs to the strategy, so this is not a flag somebody
/// has to go out of their way to set: an elevated `--mft` scan folds them and
/// the ordinary walk does not.
#[test]
fn the_rule_a_scan_counted_hardlinks_by_survives_the_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("original.bin"), vec![0u8; 64 * 1024]).unwrap();
    std::fs::hard_link(root.join("original.bin"), root.join("second.bin")).unwrap();

    for folding in [true, false] {
        let tree = Scanner::new(ScanOptions {
            dedup_hardlinks: Some(folding),
            ..ScanOptions::default()
        })
        .scan(root)
        .unwrap();
        assert_eq!(
            tree.stats().dedup_hardlinks,
            folding,
            "the scan did not record the rule it used"
        );

        let meta = Meta::of(&tree, taken_at(1), "");
        assert_eq!(meta.dedup_hardlinks, folding);

        let mut bytes = Vec::new();
        write(&tree, &meta, &mut bytes).unwrap();
        let (read_back, tree_again) = read(bytes.as_slice()).unwrap();
        assert_eq!(read_back.dedup_hardlinks, folding, "the header lost it");
        assert_eq!(
            tree_again.stats().dedup_hardlinks,
            folding,
            "the tree rebuilt from the file lost it"
        );
    }

    // The other rule that decides what is in the answer travels with it.
    for crossing in [true, false] {
        let tree = Scanner::new(ScanOptions {
            cross_filesystems: crossing,
            ..ScanOptions::default()
        })
        .scan(root)
        .unwrap();
        assert_eq!(tree.stats().cross_filesystems, crossing);
        let meta = Meta::of(&tree, taken_at(1), "");
        let mut bytes = Vec::new();
        write(&tree, &meta, &mut bytes).unwrap();
        let (read_back, tree_again) = read(bytes.as_slice()).unwrap();
        assert_eq!(read_back.cross_filesystems, crossing, "the header lost it");
        assert_eq!(tree_again.stats().cross_filesystems, crossing);
    }

    // And the two rules really do disagree about this tree, which is what
    // makes carrying the rule worth anything at all.
    let folded = Scanner::new(ScanOptions {
        dedup_hardlinks: Some(true),
        ..ScanOptions::default()
    })
    .scan(root)
    .unwrap();
    let every = Scanner::new(ScanOptions {
        dedup_hardlinks: Some(false),
        ..ScanOptions::default()
    })
    .scan(root)
    .unwrap();
    assert!(
        every.total_allocated() > folded.total_allocated(),
        "the fixture has no hardlink to disagree about: {} vs {}",
        every.total_allocated(),
        folded.total_allocated()
    );
}

/// The third rule of the same kind travels with the record.
///
/// **On Windows an on-disk size is estimated unless it was asked for.** The
/// exact answer costs `GetCompressedFileSizeW` and an open per file, so an
/// ordinary file's size is rounded up to the cluster — and for one small
/// enough to live inside the Master File Table that is not an approximation,
/// it is a different number:
///
/// ```text
///            estimated     exact
/// f1.bin          4096         8
/// f64.bin         4096        64
/// f300.bin        4096       304
/// f600.bin        4096       600
/// ```
///
/// So a record taken one way and a scan taken the other disagree about every
/// small file. Measured end to end: a snapshot with `--exact`, held against
/// an ordinary scan of the same untouched directory, reported four files as
/// having *grown* and `Net +15.0 KiB across files`. Two rules were already
/// carried for exactly this failure; this was the third and was not.
///
/// **The effective rule, not the flag.** On Unix `st_blocks` arrives with the
/// `lstat` the scan already does, so the figure is exact whether or not
/// anybody asked, and that is what is recorded there. A Linux record held
/// against an estimating Windows scan is then flagged too, which is right:
/// those two really do disagree.
#[test]
fn whether_a_scan_measured_or_estimated_survives_the_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("tiny.bin"), vec![0u8; 64]).unwrap();

    for asked in [true, false] {
        let tree = Scanner::new(ScanOptions {
            exact_allocated: asked,
            ..ScanOptions::default()
        })
        .scan(root)
        .unwrap();

        // Asked for, or free on this platform.
        let effective = asked || !crate::platform::exact_size_costs_extra();
        assert_eq!(
            tree.stats().exact_allocated,
            effective,
            "the scan did not record what it actually did"
        );

        let meta = Meta::of(&tree, taken_at(1), "");
        assert_eq!(meta.exact_allocated, Some(effective));

        let mut bytes = Vec::new();
        write(&tree, &meta, &mut bytes).unwrap();
        let (read_back, tree_again) = read(bytes.as_slice()).unwrap();
        assert_eq!(
            read_back.exact_allocated,
            Some(effective),
            "the header lost it"
        );
        assert_eq!(tree_again.stats().exact_allocated, effective);
    }
}

/// A record written before the rule was carried says nothing about it.
///
/// `None` rather than a guess. Assuming "estimated" would warn about every
/// older record on Unix, where every scan is exact; assuming "measured"
/// would warn about every older record on Windows, where the default is not.
/// Neither is a fact about the file, and the comparison stays quiet on one
/// that did not say.
#[test]
fn a_record_from_before_the_rule_does_not_claim_one() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.bin"), vec![0u8; 64]).unwrap();
    let tree = Scanner::new(ScanOptions::default())
        .scan(dir.path())
        .unwrap();
    let meta = Meta::of(&tree, taken_at(1), "");

    // The header an older kosha wrote: every key this one writes except the
    // new one. Decoded directly, because the point is the absence of a key
    // and not the framing around it.
    fn pair(header: &mut Vec<u8>, key: &str, value: &str) {
        header.extend_from_slice(&(key.len() as u16).to_le_bytes());
        header.extend_from_slice(key.as_bytes());
        header.extend_from_slice(&(value.len() as u32).to_le_bytes());
        header.extend_from_slice(value.as_bytes());
    }
    let mut header = Vec::new();
    pair(&mut header, "taken", &meta.taken.to_string());
    pair(&mut header, "root", &meta.root.to_string_lossy());
    pair(&mut header, "entries", &meta.entries.to_string());
    pair(&mut header, "cross-fs", "0");

    let older = super::decode_header(2, &header).unwrap();
    assert_eq!(older.exact_allocated, None, "a guess was made");
    assert_eq!(
        older.entries, meta.entries,
        "the rest of the header did not survive"
    );

    // And a record that does say keeps what it said.
    pair(&mut header, "exact", "1");
    assert_eq!(
        super::decode_header(2, &header).unwrap().exact_allocated,
        Some(true)
    );
}

// ------------------------------------------- names from another platform

/// A tree whose names are exactly these bytes, built without `from_parts`
/// so that nothing checks them: the bytes a snapshot from another system
/// carries. Only written, never read through a name accessor.
fn foreign_tree(children: &[&[u8]]) -> SizeTree {
    use crate::node::Flags;

    let mut raw = Vec::new();
    let mut nodes = Vec::new();
    let node = |name_off: u32, name_len: u16, parent: u32, depth: u8, flags: Flags| Node {
        logical: 1,
        allocated: 1,
        parent,
        first_child: 0,
        child_count: 0,
        subtree_files: u32::from(depth > 0),
        name_off,
        mtime: 0,
        name_len,
        flags,
        depth,
    };
    let mut root = node(0, 0, 0, 0, Flags::DIR);
    root.first_child = 1;
    root.child_count = children.len() as u32;
    root.logical = children.len() as u64;
    root.allocated = children.len() as u64;
    root.subtree_files = children.len() as u32;
    nodes.push(root);
    for name in children {
        let off = raw.len() as u32;
        raw.extend_from_slice(name);
        nodes.push(node(off, name.len() as u16, 0, 1, Flags::default()));
    }
    let stats = ScanStats {
        entries: nodes.len() as u64,
        files: children.len() as u64,
        dirs: 1,
        ..ScanStats::default()
    };
    SizeTree {
        nodes,
        names: NameArena::from_bytes(raw),
        owners: Owners::default(),
        created: Vec::new(),
        root_path: PathBuf::from("recorded-elsewhere"),
        stats,
        replaced_names: Vec::new(),
    }
}

const PLAIN: &[u8] = b"plain.txt";
/// Latin-1, as Linux will store a name if that is what it was given.
const LATIN1: &[u8] = b"caf\xe9.txt";
/// `odd-` and a lone high surrogate: a legal NTFS name, in WTF-8.
const LONE_SURROGATE: &[u8] = b"odd-\xed\xa0\x80";
/// A surrogate pair written as two halves: a legal Linux name, and not
/// something Windows can hold.
const SPLIT_PAIR: &[u8] = b"\xed\xa0\x80\xed\xb0\x80";

/// What opening a snapshot from another system does on *this* one: every name
/// valid here comes back byte for byte; on Unix that is every name. Where a
/// name is not valid (Windows), it is shown lossily, marked and counted.
#[test]
fn names_from_another_system_are_kept_or_replaced_and_counted() {
    let original = foreign_tree(&[PLAIN, LATIN1, LONE_SURROGATE, SPLIT_PAIR]);
    let (_meta, restored) = round_trip(&original);
    let name = |index: usize| {
        restored
            .name(NodeId::from_index(index))
            .as_encoded_bytes()
            .to_vec()
    };

    for index in 0..restored.len() {
        assert!(crate::is_valid_encoded_name(&name(index)), "at {index}");
    }
    assert_eq!(name(1), PLAIN);
    assert!(!restored.name_replaced(NodeId::from_index(1)));
    if cfg!(any(unix, windows)) {
        assert_eq!(name(3), LONE_SURROGATE, "valid on both, so kept exactly");
    }

    if crate::names::EVERY_BYTE_STRING_IS_A_NAME {
        assert_eq!(name(2), LATIN1);
        assert_eq!(name(4), SPLIT_PAIR);
        assert_eq!(restored.names_replaced(), 0);
        assert_eq!(replaced_names_note(&restored), None);
    } else {
        assert_eq!(name(2), "caf\u{FFFD}.txt".as_bytes());
        assert_eq!(name(4), String::from_utf8_lossy(SPLIT_PAIR).as_bytes());
        assert!(restored.name_replaced(NodeId::from_index(2)));
        assert!(restored.name_replaced(NodeId::from_index(4)));
        assert_eq!(restored.names_replaced(), 2);
        let note = replaced_names_note(&restored).unwrap();
        assert!(note.starts_with("2 names in this snapshot"), "{note}");
    }
    // The shape and the numbers are untouched either way.
    assert_eq!(restored.len(), original.len());
    assert_eq!(restored.total_allocated(), original.total_allocated());
}

/// The same snapshot under the Windows rule, on every platform: what a Linux
/// record looks like when it is opened on Windows.
#[test]
fn a_linux_snapshot_opened_under_the_windows_rule() {
    let original = foreign_tree(&[PLAIN, LATIN1, LONE_SURROGATE, SPLIT_PAIR]);
    let (_meta, restored) = round_trip(&original);
    let simulated = SizeTree::from_parts_with_name_check(
        restored.nodes_slice().to_vec(),
        NameArena::from_bytes(restored.names_bytes().to_vec()),
        Owners::default(),
        restored.root_path().to_path_buf(),
        restored.stats().clone(),
        crate::is_well_formed_wtf8,
    );
    let name = |index: usize| {
        simulated
            .name(NodeId::from_index(index))
            .as_encoded_bytes()
            .to_vec()
    };
    for index in 0..simulated.len() {
        assert!(crate::is_well_formed_wtf8(&name(index)), "at {index}");
    }
    assert_eq!(name(1), PLAIN);
    if cfg!(any(unix, windows)) {
        assert_eq!(name(3), LONE_SURROGATE);
    }
    assert_eq!(name(2), "caf\u{FFFD}.txt".as_bytes());
    assert_eq!(name(4), String::from_utf8_lossy(SPLIT_PAIR).as_bytes());
    // Counted once: by the reader where the platform refuses them, by the
    // simulation where it does not.
    assert_eq!(restored.names_replaced() + simulated.names_replaced(), 2);
}

/// `from_parts` is safe, so no arena and no span can make it hand back a name
/// this platform cannot hold. Every span of an arena of hostile bytes.
#[test]
fn from_parts_never_yields_a_name_this_platform_cannot_hold() {
    let hostile: Vec<u8> =
        b"a\xc3\xa9\xff\xed\xa0\x80\xed\xb0\x80\xf0\x9f\x98\x80\xe2\x82".to_vec();
    let original = foreign_tree(&[]);
    let mut nodes = original.nodes_slice().to_vec();
    nodes[0].child_count = 0;
    for start in 0..=hostile.len() {
        for end in start..=hostile.len() {
            let mut child = nodes[0];
            child.name_off = start as u32;
            child.name_len = (end - start) as u16;
            child.depth = 1;
            child.flags = crate::node::Flags::default();
            nodes.push(child);
        }
    }
    nodes[0].first_child = 1;
    nodes[0].child_count = (nodes.len() - 1) as u32;
    let tree = SizeTree::from_parts(
        nodes,
        NameArena::from_bytes(hostile),
        Owners::default(),
        PathBuf::from("hostile"),
        ScanStats::default(),
    );
    for index in 0..tree.len() {
        let id = NodeId::from_index(index);
        assert!(crate::is_valid_encoded_name(
            tree.name(id).as_encoded_bytes()
        ));
        let _ = tree.path_of(id);
    }
    if crate::names::EVERY_BYTE_STRING_IS_A_NAME {
        assert_eq!(tree.names_replaced(), 0);
    } else {
        assert!(tree.names_replaced() > 0);
    }
}

/// Children in the scan's byte order, the last of which (`b` and 0xFF) is
/// replaced under the Windows rule by `b` and U+FFFD — bytes EF BF BD, which
/// sort *before* the two siblings ahead of it. A binary search for the
/// replaced name, or past it, would miss.
const SORTED: [&[u8]; 6] = [
    b"a.txt",
    b"b\xc3\xa9",
    b"b\xf0\x9f\x98\x80",
    b"b\xf3\xa0\x80\x80",
    b"b\xf4\x80\x80\x80",
    b"b\xff",
];

/// Open `foreign`, a tree whose names are still the raw bytes another system
/// wrote, under the Windows rule. It must start from the raw tree and not from
/// one that was round-tripped through a snapshot: on Windows a snapshot read
/// has already replaced the name and recorded it, so a second pass would find
/// nothing to replace and record nothing, and the folder would look sorted.
fn under_the_windows_rule(foreign: &SizeTree) -> SizeTree {
    SizeTree::from_parts_with_name_check(
        foreign.nodes_slice().to_vec(),
        NameArena::from_bytes(foreign.names_bytes().to_vec()),
        Owners::default(),
        foreign.root_path().to_path_buf(),
        foreign.stats().clone(),
        crate::is_well_formed_wtf8,
    )
}

#[test]
fn every_sibling_of_a_replaced_name_is_still_found_by_name() {
    let tree = under_the_windows_rule(&foreign_tree(&SORTED));
    assert_eq!(tree.names_replaced(), 1);
    let replaced = tree
        .name(NodeId::from_index(SORTED.len()))
        .as_encoded_bytes()
        .to_vec();
    assert_eq!(replaced, "b\u{FFFD}".as_bytes());
    assert!(
        replaced.as_slice() < SORTED[2],
        "the fixture must put the replacement out of order"
    );
    assert!(!tree.children_sorted(tree.node(tree.root()).child_range()));

    for index in 1..tree.len() {
        let id = NodeId::from_index(index);
        assert_eq!(
            tree.child_named(tree.root(), tree.name(id)),
            Some(id),
            "at {index}"
        );
        assert!(tree.has_sibling_named(NodeId::from_index(1), tree.name(id)));
    }
    assert_eq!(
        tree.child_named(tree.root(), std::ffi::OsStr::new("missing")),
        None
    );
}

/// The fast path stays: a tree with no replaced names, and a directory with
/// none in it, are searched in order.
#[test]
fn directories_without_a_replaced_name_keep_the_binary_search() {
    let (_dir, scanned) = fixture();
    assert_eq!(scanned.names_replaced(), 0);
    for index in 0..scanned.len() {
        let range = scanned.node(NodeId::from_index(index)).child_range();
        assert!(scanned.children_sorted(range));
    }

    // A replaced name at index 6 leaves ranges that do not include it alone.
    let tree = under_the_windows_rule(&foreign_tree(&SORTED));
    assert!(tree.children_sorted(1..6));
    assert!(tree.children_sorted(7..9));
    assert!(tree.children_sorted(0..0));
    assert!(!tree.children_sorted(6..7));
    assert!(!tree.children_sorted(1..7));
}

/// Comparing a record against itself opened under the Windows rule finds
/// nothing: comparison is by path text, and the replaced name's text is what
/// the original bytes always displayed as. Nothing else appears or vanishes.
#[test]
fn a_replaced_name_adds_no_spurious_changes_to_a_comparison() {
    let original = foreign_tree(&SORTED);
    let (_meta, restored) = round_trip(&original);
    let tree = under_the_windows_rule(&original);
    let baseline = crate::compare::Baseline::of_tree(&restored);
    let options = crate::compare::Options {
        min_delta: 0,
        directories: true,
        files: true,
    };
    let report = crate::compare::compare(&baseline, &tree, &options);
    assert!(report.changes.is_empty(), "{:?}", report.changes);
    assert_eq!(report.net, 0);
}

// ------------------------------------------------------------ imported records

fn imported_tree() -> (Meta, SizeTree) {
    use crate::builder::{Entry, TreeBuilder};
    let dir = Entry {
        flags: Flags::DIR,
        ..Entry::default()
    };
    let mut builder = TreeBuilder::new("srv", dir).unwrap();
    let data = builder.push(0, "data", dir).unwrap();
    builder
        .push(
            data,
            "big.bin",
            Entry {
                logical: 5000,
                allocated: 8192,
                ..Entry::default()
            },
        )
        .unwrap();
    let stats = ScanStats {
        strategy: Strategy::Imported,
        dedup_hardlinks: true,
        ..ScanStats::default()
    };
    let (tree, _) = builder.finish(PathBuf::from("/srv"), stats).unwrap();
    let mut meta = Meta::of(&tree, taken_at(1_763_647_320), "build-box /srv");
    meta.exact_allocated = None;
    meta.import = Some(Provenance {
        format: "ncdu-json".into(),
        format_version: "1.2".into(),
        tool: "ncdu 2.9".into(),
        observed: Some(1_763_647_320),
        observed_basis: "tool-timestamp".into(),
        imported_at: 1_763_700_000,
        source_name: "build-box.json".into(),
        allocated_known: true,
        mtime_known: false,
        unknown_size: 2,
        excluded_by_source: 3,
        synthesized_dirs: 0,
        lossy_names: 0,
        duplicate_paths: 1,
    });
    (meta, tree)
}

#[test]
fn an_imported_record_is_format_three_and_keeps_its_provenance() {
    let (meta, tree) = imported_tree();
    let mut bytes = Vec::new();
    write(&tree, &meta, &mut bytes).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);

    let (back, restored) = read(&bytes[..]).unwrap();
    assert_eq!(back, meta, "every provenance key and counter survives");
    assert_eq!(back.strategy, "Imported");
    assert_eq!(restored.stats().strategy, Strategy::Imported);
    assert_eq!(restored.total_logical(), 5000);
    assert_eq!(restored.total_allocated(), 8192);
    assert_eq!(
        back.import_sentence().unwrap(),
        "Imported from ncdu 2.9 (export 2025-11-20 14:02 UTC). Not measured by Kosha."
    );
    assert_eq!(back.import.as_ref().unwrap().tool_short(), "ncdu");
    assert_eq!(read_meta(&bytes[..]).unwrap(), meta);

    // Native snapshots are unchanged: still format 2, no provenance.
    let (_dir, scanned) = fixture();
    let mut native = Vec::new();
    let plain = Meta::of(&scanned, taken_at(1), "");
    write(&scanned, &plain, &mut native).unwrap();
    assert_eq!(
        u32::from_le_bytes(native[8..12].try_into().unwrap()),
        FORMAT
    );
    assert!(read_meta(&native[..]).unwrap().import.is_none());
}

#[test]
fn an_older_reader_refuses_an_imported_record() {
    let (meta, tree) = imported_tree();
    let mut bytes = Vec::new();
    write(&tree, &meta, &mut bytes).unwrap();
    // Every released kosha before format 3 reads up to 2.
    let mut input = &bytes[..];
    match read_header_bytes_up_to(&mut input, 2) {
        Err(ReadError::Newer { found }) => assert_eq!(found, 3),
        other => panic!("an older reader accepted it: {:?}", other.map(|(f, _)| f)),
    }
    let message = ReadError::Newer { found: 3 }.to_string();
    assert!(message.contains("A newer kosha wrote it"), "{message}");
}

#[test]
fn format_three_without_provenance_is_refused_and_an_unknown_observation_says_so() {
    let (meta, tree) = imported_tree();
    // A format 3 header that does not say it was imported.
    let mut bytes = Vec::new();
    let mut plain = meta.clone();
    plain.import = None;
    write(&tree, &plain, &mut bytes).unwrap();
    bytes[8..12].copy_from_slice(&3u32.to_le_bytes());
    assert!(matches!(
        read_meta(&bytes[..]),
        Err(ReadError::Damaged(
            "an imported record without its provenance"
        ))
    ));

    let mut unknown = meta.import.clone().unwrap();
    unknown.observed = None;
    unknown.observed_basis = "unknown".into();
    assert_eq!(
        unknown.sentence(),
        "Imported from ncdu 2.9 (observed: unknown, imported 2025-11-21 04:40 UTC). Not measured by Kosha."
    );
    unknown.tool = "ncdu\n\u{1b}[31m".into();
    assert!(!unknown.sentence().chars().any(char::is_control));
}

#[test]
fn a_strategy_label_cannot_hide_an_import() {
    let (mut meta, tree) = imported_tree();
    meta.strategy = "Directory walk".into();
    let mut bytes = Vec::new();
    write(&tree, &meta, &mut bytes).unwrap();
    let (back, restored) = read(&bytes[..]).unwrap();
    assert_eq!(back.strategy, "Imported");
    assert_eq!(restored.stats().strategy, Strategy::Imported);
}
