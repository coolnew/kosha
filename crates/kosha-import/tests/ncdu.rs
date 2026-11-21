// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The ncdu importer against synthetic exports, hostile ones included.
//!
//! Fixtures are synthetic. `fixtures/small.json` is hand-written; the `.zst`
//! files beside it were made from it by `scripts/import_fixtures.py` with the
//! zstd command-line tool 1.5.7. Everything else is built here.

use std::ffi::OsStr;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kosha_core::{Flags, NodeId, SizeTree, Strategy, snapshot};
use kosha_import::{Compression, ImportError, Imported, Limits, import_ncdu};

const SMALL: &[u8] = include_bytes!("fixtures/small.json");

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_763_700_000)
}

fn import_with(bytes: &[u8], limits: &Limits) -> Result<Imported, ImportError> {
    import_ncdu(bytes, "export.json", "", now(), limits)
}

fn import(bytes: &[u8]) -> Result<Imported, ImportError> {
    import_with(bytes, &Limits::default())
}

fn ok(bytes: &[u8]) -> Imported {
    match import(bytes) {
        Ok(imported) => imported,
        Err(error) => panic!("refused: {error}"),
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

/// A minimal export around a root directory's contents.
fn export(root_children: &str) -> Vec<u8> {
    format!(
        r#"[1,2,{{"progname":"ncdu","progver":"2.9","timestamp":1763647320}},[{{"name":"/r"}}{root_children}]]"#
    )
    .into_bytes()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

// ----------------------------------------------------------- field mapping

#[test]
fn every_documented_field_maps_onto_the_tree() {
    let imported = ok(SMALL);
    let tree = &imported.tree;
    let stats = tree.stats();

    assert_eq!(imported.compression, Compression::None);
    assert_eq!(stats.strategy, Strategy::Imported);
    assert_eq!(tree.root_path(), std::path::Path::new("/srv"));
    assert_eq!(tree.name_lossy(tree.root()), "srv");
    assert_eq!(tree.len(), 12);
    assert_eq!(stats.dirs, 4);
    assert_eq!(stats.files, 8);
    // Apparent: directories' own asize is ignored, the second hard link
    // contributes nothing.
    assert_eq!(tree.total_logical(), 1200 + 10_000 + 7 + 1);
    // On disk: directories' own dsize plus their contents.
    assert_eq!(
        tree.total_allocated(),
        4096 * 2 + 4096 + 12_288 + 4096 + 4096
    );
    assert_eq!(stats.max_depth, 2);

    let top: Vec<_> = tree
        .children(tree.root())
        .map(|id| tree.name_lossy(id).into_owned())
        .collect();
    assert_eq!(top, ["data", "empty.txt", "other.dat", "readme.txt"]);

    let a = tree.node(find(tree, &["data", "a.bin"]));
    let b = tree.node(find(tree, &["data", "b.bin"]));
    assert!(!a.flags.contains(Flags::HARDLINK_DUP));
    assert!(b.flags.contains(Flags::HARDLINK_DUP));
    assert_eq!((b.logical, b.allocated), (0, 0));
    assert_eq!(stats.hardlinks_deduped, 1);
    assert!(stats.dedup_hardlinks);
    // Same inode number, nlink 1: not a link, counted on its own.
    assert_eq!(tree.node(find(tree, &["other.dat"])).logical, 1);

    assert!(
        tree.node(find(tree, &["data", "link"]))
            .flags
            .contains(Flags::SYMLINK)
    );
    let locked = tree.node(find(tree, &["data", "locked"]));
    assert!(locked.is_dir() && locked.flags.contains(Flags::UNREADABLE));
    assert!(tree.node(find(tree, &["data"])).is_partial());
    assert!(
        tree.node(find(tree, &["data", "proc"]))
            .flags
            .contains(Flags::OTHER_DEVICE)
    );
    let mnt = tree.node(find(tree, &["data", "mnt"]));
    assert!(mnt.is_dir() && mnt.flags.contains(Flags::OTHER_DEVICE));
    assert_eq!(stats.unreadable, 1);
    assert_eq!(stats.skipped_other_device, 2);
    assert!(
        stats.partial,
        "excluded and unreadable entries make totals a floor"
    );

    assert_eq!(tree.node(tree.root()).mtime, 1_763_600_000);
    assert_eq!(
        tree.node(find(tree, &["data"])).mtime,
        1_763_600_200,
        "fraction dropped"
    );
    assert_eq!(
        tree.node(find(tree, &["empty.txt"])).mtime,
        0,
        "absent is unknown"
    );

    let meta = &imported.meta;
    assert_eq!(meta.taken, 1_763_647_320, "the export's own timestamp");
    assert_eq!(meta.strategy, "Imported");
    let import = meta.import.as_ref().unwrap();
    assert_eq!(import.format, "ncdu-json");
    assert_eq!(import.format_version, "1.2");
    assert_eq!(import.tool, "ncdu 2.9");
    assert_eq!(import.observed, Some(1_763_647_320));
    assert_eq!(import.observed_basis, "tool-timestamp");
    assert_eq!(import.imported_at, 1_763_700_000);
    assert_eq!(import.source_name, "export.json");
    assert!(import.allocated_known);
    assert!(import.mtime_known);
    assert_eq!(import.excluded_by_source, 1);
    assert_eq!(import.unknown_size, 2);
    assert_eq!(import.duplicate_paths, 0);
    assert_eq!(
        meta.import_sentence().unwrap(),
        "Imported from ncdu 2.9 (export 2025-11-20 14:02 UTC). Not measured by Kosha."
    );
}

#[test]
fn a_minimal_export_without_metadata_or_sizes_is_apparent_only() {
    let imported = ok(br#"[1,0,{},[{"name":"/"},{"name":"f","asize":10}]]"#);
    let import = imported.meta.import.as_ref().unwrap();
    assert_eq!(import.tool, "");
    assert_eq!(import.observed, None);
    assert_eq!(import.observed_basis, "unknown");
    assert_eq!(
        imported.meta.taken, 1_763_700_000,
        "import time when unknown"
    );
    assert!(!import.allocated_known);
    assert!(!import.mtime_known);
    // No dsize anywhere: on-disk stored equal to apparent, never zero.
    assert_eq!(imported.tree.total_allocated(), 10);
    assert_eq!(imported.tree.total_logical(), 10);
    assert_eq!(imported.tree.name_lossy(imported.tree.root()), "/");
    assert!(
        imported
            .meta
            .import_sentence()
            .unwrap()
            .contains("observed: unknown, imported 2025-11-21 04:40 UTC")
    );
}

#[test]
fn zero_size_files_and_empty_directories_are_kept() {
    let imported = ok(&export(
        r#",{"name":"z","asize":0,"dsize":0},[{"name":"d"}]"#,
    ));
    assert_eq!(imported.tree.len(), 3);
    assert_eq!(imported.tree.total_logical(), 0);
}

#[test]
fn hard_links_without_an_inode_group_on_their_device() {
    let imported = ok(&export(
        r#",{"name":"a","asize":5,"hlnkc":true},{"name":"b","asize":5,"hlnkc":true},{"name":"c","asize":5,"hlnkc":true,"dev":9}"#,
    ));
    assert_eq!(
        imported.tree.total_logical(),
        10,
        "a and b are one file, c another device"
    );
    assert_eq!(imported.tree.stats().hardlinks_deduped, 1);
}

#[test]
fn duplicate_names_are_kept_and_counted() {
    let imported = ok(&export(r#",{"name":"x","asize":1},{"name":"x","asize":2}"#));
    assert_eq!(imported.tree.len(), 3);
    assert_eq!(imported.tree.total_logical(), 3);
    assert_eq!(imported.meta.import.as_ref().unwrap().duplicate_paths, 1);
}

// ------------------------------------------------------------------ bounds

fn nested(depth: u32) -> Vec<u8> {
    let mut text = String::from(r#"[1,2,{},[{"name":"/r"}"#);
    for level in 0..depth {
        text.push_str(&format!(r#",[{{"name":"d{level}"}}"#));
    }
    for _ in 0..depth {
        text.push(']');
    }
    text.push_str("]]");
    text.into_bytes()
}

#[test]
fn nesting_at_the_limit_imports_and_one_more_is_refused() {
    let limit = kosha_import::MAX_DEPTH;
    let at = ok(&nested(limit));
    assert_eq!(at.tree.len() as u32, limit + 1);
    match import(&nested(limit + 1)) {
        Err(ImportError::TooDeep { limit: said, .. }) => assert_eq!(said, limit),
        other => panic!("{:?}", other.err()),
    }
    // A smaller limit is honoured, a larger one is clamped.
    let small = Limits {
        max_depth: 3,
        ..Limits::default()
    };
    assert!(import_with(&nested(3), &small).is_ok());
    assert!(matches!(
        import_with(&nested(4), &small),
        Err(ImportError::TooDeep { limit: 3, .. })
    ));
}

#[test]
fn a_very_deep_export_is_refused_by_the_limit_not_the_stack() {
    let mut text = String::from(r#"[1,2,{},[{"name":"/r"}"#);
    for _ in 0..100_000 {
        text.push_str(r#",[{"name":"d"}"#);
    }
    match import(text.as_bytes()) {
        Err(ImportError::TooDeep { .. }) => {}
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn names_at_and_over_the_length_limit() {
    let fits = "n".repeat(4096);
    assert!(import(&export(&format!(r#",{{"name":"{fits}"}}"#))).is_ok());
    let long = "n".repeat(4097);
    match import(&export(&format!(r#",{{"name":"{long}"}}"#))) {
        Err(error) => assert!(error.to_string().contains("4097"), "{error}"),
        Ok(_) => panic!("accepted a 4097-byte name"),
    }
}

#[test]
fn names_that_step_outside_their_directory_are_refused_with_the_entry() {
    for bad in [
        r#""""#,
        r#"".""#,
        r#""..""#,
        r#""a/b""#,
        r#""/etc""#,
        r#""a\u0000b""#,
        r#""..\\x""#,
    ] {
        let doc = export(&format!(r#",{{"name":"ok"}},{{"name":{bad}}}"#));
        match import(&doc) {
            Err(ImportError::Entry { entry, .. }) => assert_eq!(entry, 3, "{bad}"),
            other => panic!("{bad}: {:?}", other.err()),
        }
    }
    // The root may be a path; it may not hold a NUL.
    assert!(matches!(
        import(br#"[1,2,{},[{"name":"/r\u0000"}]]"#),
        Err(ImportError::Entry { entry: 1, .. })
    ));
}

#[test]
fn entries_and_input_bytes_are_capped() {
    let few = Limits {
        max_entries: 3,
        ..Limits::default()
    };
    assert!(import_with(&export(r#",{"name":"a"},{"name":"b"}"#), &few).is_ok());
    assert!(matches!(
        import_with(&export(r#",{"name":"a"},{"name":"b"},{"name":"c"}"#), &few),
        Err(ImportError::TooManyEntries(3))
    ));

    let tight = Limits {
        max_input_bytes: SMALL.len() as u64,
        ..Limits::default()
    };
    assert!(import_with(SMALL, &tight).is_ok(), "exactly at the limit");
    let tighter = Limits {
        max_input_bytes: SMALL.len() as u64 - 1,
        ..Limits::default()
    };
    assert!(matches!(
        import_with(SMALL, &tighter),
        Err(ImportError::InputTooLarge(_))
    ));
}

#[test]
fn sizes_outside_the_specification_are_refused() {
    assert!(matches!(
        import(&export(r#",{"name":"a","asize":9223372036854775808}"#)),
        Err(ImportError::Entry { entry: 2, .. })
    ));
    assert!(import(&export(r#",{"name":"a","asize":9223372036854775807}"#)).is_ok());
    assert!(matches!(
        import(&export(r#",{"name":"a","asize":-1}"#)),
        Err(ImportError::Malformed(_))
    ));
}

// ------------------------------------------------------- what is not ncdu

#[test]
fn the_wrong_kind_of_input_is_named() {
    assert!(matches!(import(b""), Err(ImportError::Empty)));
    assert!(matches!(import(b"  \n\t "), Err(ImportError::Empty)));
    assert!(matches!(
        import(b"\xbfncduEX1\x00\x00"),
        Err(ImportError::NcduBinary)
    ));
    assert!(matches!(
        import(br#"{"a":1}"#),
        Err(ImportError::NotNcdu(_))
    ));
    assert!(matches!(
        import(br#"[{"Path":"a","Name":"a","Size":1}]"#),
        Err(ImportError::NotNcdu(_))
    ));
    assert!(matches!(import(b"[]"), Err(ImportError::NotNcdu(_))));
    assert!(matches!(
        import(br#"[2,0,{},[{"name":"/"}]]"#),
        Err(ImportError::Version { major: 2, .. })
    ));
    assert!(matches!(
        import(br#"[1,10001,{},[{"name":"/"}]]"#),
        Err(ImportError::Version { minor: 10001, .. })
    ));
    assert!(import(br#"[1,10000,{},[{"name":"/"}]]"#).is_ok());
    assert!(matches!(
        import(br#"[1,0,{},{"name":"/file"}]"#),
        Err(ImportError::NotNcdu(_))
    ));
    // Binary format inside gzip is named too.
    assert!(matches!(
        import(&gzip(b"\xbfncduEX1rest")),
        Err(ImportError::NcduBinary)
    ));
}

#[test]
fn names_that_are_not_utf8_are_refused_with_the_fix() {
    let mut doc = export(r#",{"name":"caf_"}"#);
    let at = doc.iter().rposition(|&b| b == b'_').unwrap();
    doc[at] = 0xe9; // Latin-1 é, as a filesystem might hand it over
    match import(&doc) {
        Err(ImportError::NotUtf8(_)) => {}
        other => panic!("{:?}", other.err()),
    }
    let message = ImportError::NotUtf8(String::new()).to_string();
    assert!(message.contains("iconv"), "{message}");
}

#[test]
fn malformed_json_and_trailing_bytes_are_refused() {
    for doc in [
        &br#"[1,2,{},[{"name":"/r"},{"name":5}]]"#[..],
        br#"[1,2,{},[{"name":"/r"},7]]"#,
        br#"[1,2,{},[{"name":"/r"}]] trailing"#,
        br#"[1,2,{},[{"name":"/r","asize":1.5}]]"#,
        br#"[1,2,{},[[{"name":"/r"}]]]"#,
    ] {
        assert!(import(doc).is_err(), "{}", String::from_utf8_lossy(doc));
    }
}

#[test]
fn an_export_cut_anywhere_is_refused() {
    let full = SMALL;
    let end = full.iter().rposition(|&b| b == b']').unwrap() + 1;
    for cut in 0..end {
        let result = import(&full[..cut]);
        assert!(result.is_err(), "cut at {cut} was accepted");
        if cut > 0 {
            // Almost always "ends early"; a cut inside a number can also be
            // read as a malformed number. Never anything else.
            assert!(
                matches!(
                    result,
                    Err(ImportError::Truncated(_) | ImportError::Malformed(_))
                ),
                "cut at {cut}: {:?}",
                result.err()
            );
        }
    }
    assert!(import(&full[..end]).is_ok());
}

// ----------------------------------------------------------- compression

#[test]
fn gzip_input_imports_the_same_tree() {
    let plain = ok(SMALL);
    let zipped = ok(&gzip(SMALL));
    assert_eq!(zipped.compression, Compression::Gzip);
    assert_eq!(zipped.tree.total_logical(), plain.tree.total_logical());
    assert_eq!(zipped.tree.len(), plain.tree.len());
    assert_eq!(zipped.bytes_decoded, SMALL.len() as u64);
    // Concatenated members, as `cat a.gz b.gz` makes.
    let half = SMALL.len() / 2;
    let mut two = gzip(&SMALL[..half]);
    two.extend(gzip(&SMALL[half..]));
    assert_eq!(ok(&two).tree.len(), plain.tree.len());
}

#[test]
fn a_gzip_bomb_stops_at_the_decompressed_limit() {
    let mut bomb = vec![b'['];
    bomb.resize(8 << 20, b' ');
    let small = Limits {
        max_input_bytes: 1 << 20,
        ..Limits::default()
    };
    match import_with(&gzip(&bomb), &small) {
        Err(ImportError::DecompressedTooLarge(limit)) => assert_eq!(limit, 1 << 20),
        other => panic!("{:?}", other.err()),
    }
    let mut cut = gzip(SMALL);
    cut.truncate(cut.len() - 10);
    assert!(matches!(
        import(&cut),
        Err(ImportError::Compressed(_) | ImportError::Truncated(_))
    ));
}

const SMALL_ZST: &[u8] = include_bytes!("fixtures/small.json.zst");

#[test]
fn zstd_input_imports_the_same_tree() {
    let plain = ok(SMALL);
    for (name, bytes) in [
        ("small.json.zst", SMALL_ZST),
        (
            "small-nocheck.json.zst",
            &include_bytes!("fixtures/small-nocheck.json.zst")[..],
        ),
        (
            "multi.json.zst",
            &include_bytes!("fixtures/multi.json.zst")[..],
        ),
    ] {
        let imported = match import(bytes) {
            Ok(imported) => imported,
            Err(error) => panic!("{name}: {error}"),
        };
        assert_eq!(imported.compression, Compression::Zstd, "{name}");
        assert_eq!(imported.tree.len(), plain.tree.len(), "{name}");
        assert_eq!(
            imported.tree.total_allocated(),
            plain.tree.total_allocated(),
            "{name}"
        );
        assert_eq!(imported.bytes_decoded, SMALL.len() as u64, "{name}");
        assert_eq!(imported.bytes_read, bytes.len() as u64, "{name}");
    }
}

#[test]
fn zstd_bombs_stop_at_the_decompressed_limit() {
    let small = Limits {
        max_input_bytes: 1 << 20,
        ..Limits::default()
    };
    for bytes in [
        &include_bytes!("fixtures/bomb.zst")[..],
        &include_bytes!("fixtures/bomb-declared.zst")[..],
    ] {
        let started = std::time::Instant::now();
        match import_with(bytes, &small) {
            Err(ImportError::DecompressedTooLarge(limit)) => assert_eq!(limit, 1 << 20),
            other => panic!("{:?}", other.err()),
        }
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}

#[test]
fn an_oversized_zstd_window_is_refused_before_decoding() {
    match import(include_bytes!("fixtures/long-window.zst")) {
        Err(ImportError::ZstdWindow { requested, limit }) => {
            assert_eq!(requested, 1 << 30);
            assert_eq!(limit, 8 << 20);
        }
        other => panic!("{:?}", other.err()),
    }
    // Hand-built: window descriptor 0xFF (the largest the format can say).
    let header = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xff, 0, 0, 0];
    assert!(matches!(
        import(&header),
        Err(ImportError::ZstdWindow { .. } | ImportError::Compressed(_))
    ));
    // Hand-built: single segment declaring 100 GB of content (window = size).
    let mut declared = vec![0x28, 0xb5, 0x2f, 0xfd, 0xe0];
    declared.extend_from_slice(&100_000_000_000u64.to_le_bytes());
    declared.extend_from_slice(&[1, 0, 0]);
    assert!(matches!(
        import(&declared),
        Err(ImportError::ZstdWindow { .. } | ImportError::DecompressedTooLarge(_))
    ));
}

#[test]
fn damaged_zstd_is_refused_in_its_own_words() {
    // Content checksum flipped: a distinct sentence.
    let mut flipped = SMALL_ZST.to_vec();
    *flipped.last_mut().unwrap() ^= 0x01;
    match import(&flipped) {
        Err(ImportError::Compressed(message)) => assert!(message.contains("checksum"), "{message}"),
        other => panic!("{:?}", other.err()),
    }
    // A byte in the middle of the frame.
    for at in [6, SMALL_ZST.len() / 2] {
        let mut bad = SMALL_ZST.to_vec();
        bad[at] ^= 0x5a;
        assert!(import(&bad).is_err(), "flip at {at}");
    }
    // Cut at every length.
    for cut in 1..SMALL_ZST.len() {
        assert!(import(&SMALL_ZST[..cut]).is_err(), "cut at {cut}");
    }
    // Something after the last frame that is not a frame.
    let mut trailing = SMALL_ZST.to_vec();
    trailing.extend_from_slice(b"garbage!");
    assert!(matches!(import(&trailing), Err(ImportError::Compressed(_))));
    // A skippable frame and nothing else.
    let mut skippable = 0x184d_2a50u32.to_le_bytes().to_vec();
    skippable.extend_from_slice(&3u32.to_le_bytes());
    skippable.extend_from_slice(b"abc");
    match import(&skippable) {
        Err(ImportError::Compressed(message)) => {
            assert!(message.contains("no data frame"), "{message}")
        }
        other => panic!("{:?}", other.err()),
    }
    // A skippable frame that claims more than it holds.
    let mut short = 0x184d_2a5fu32.to_le_bytes().to_vec();
    short.extend_from_slice(&1000u32.to_le_bytes());
    assert!(import(&short).is_err());
    // zstd inside gzip.
    assert!(matches!(
        import(&gzip(SMALL_ZST)),
        Err(ImportError::CompressedTwice)
    ));
}

// ------------------------------------------------------------- the record

#[test]
fn an_import_round_trips_as_a_format_three_record() {
    let imported = import_ncdu(
        SMALL,
        "small.json",
        "build-box /srv",
        now(),
        &Limits::default(),
    )
    .unwrap();
    let mut bytes = Vec::new();
    snapshot::write(&imported.tree, &imported.meta, &mut bytes).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);
    let (meta, tree) = snapshot::read(&bytes[..]).unwrap();
    assert_eq!(meta, imported.meta);
    assert_eq!(meta.label, "build-box /srv");
    assert_eq!(tree.stats().strategy, Strategy::Imported);
    assert_eq!(tree.len(), imported.tree.len());
    assert_eq!(tree.total_logical(), imported.tree.total_logical());
    assert!(tree.is_partial());
    // Comparing the record with itself reports nothing, by the same engine
    // a comparison against a scan uses.
    let baseline = kosha_core::compare::Baseline::of_tree(&tree);
    let report = kosha_core::compare::compare(
        &baseline,
        &imported.tree,
        &kosha_core::compare::Options {
            min_delta: 0,
            ..Default::default()
        },
    );
    assert!(report.changes.is_empty());
}

/// The importer touches nothing but its reader: no filesystem, no network, no
/// process. Checked on the source, because "never opened" cannot be observed
/// from inside a test without a recording filesystem.
#[test]
fn the_importer_has_no_way_to_reach_outside_its_reader() {
    for (name, source) in [
        ("lib.rs", include_str!("../src/lib.rs")),
        ("ncdu.rs", include_str!("../src/ncdu.rs")),
        ("decompress.rs", include_str!("../src/decompress.rs")),
    ] {
        for word in [
            "std::fs",
            "File::",
            "Command",
            "TcpStream",
            "UdpSocket",
            "std::net",
            "std::env",
            "std::process",
        ] {
            assert!(!source.contains(word), "{name} mentions {word}");
        }
    }
}

// ------------------------------------------------------------------- fuzz

/// Random damage to valid inputs never panics and never exceeds the caps.
/// A short, deterministic run; the `fuzz/` target is the long one.
#[test]
fn random_damage_never_panics() {
    let limits = Limits {
        max_input_bytes: 4 << 20,
        max_entries: 10_000,
        ..Limits::default()
    };
    let seeds: Vec<Vec<u8>> = vec![
        SMALL.to_vec(),
        SMALL_ZST.to_vec(),
        include_bytes!("fixtures/multi.json.zst").to_vec(),
        gzip(SMALL),
        nested(20),
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let started = std::time::Instant::now();
    let mut runs = 0u64;
    while started.elapsed() < Duration::from_secs(3) || runs < 200 {
        let seed = &seeds[(next() % seeds.len() as u64) as usize];
        let mut input = seed.clone();
        for _ in 0..1 + next() % 4 {
            if input.is_empty() {
                break;
            }
            let at = (next() % input.len() as u64) as usize;
            match next() % 5 {
                0 => input[at] ^= 1 << (next() % 8),
                1 => input[at] = next() as u8,
                2 => input.truncate(at),
                3 => input.insert(at, next() as u8),
                _ => {
                    input.remove(at);
                }
            }
        }
        if let Err(ImportError::Internal) = import_with(&input, &limits) {
            panic!("the importer panicked on input {input:?}");
        }
        runs += 1;
    }
    assert!(runs >= 200);
}
