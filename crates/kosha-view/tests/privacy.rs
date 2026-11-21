// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use kosha_core::{Flags, NameArena, Node, Owner, OwnerId, OwnerKind, Owners, ScanStats, SizeTree};
use kosha_query::Query;
use kosha_view::{Fields, Selection, View, encode, prepare, publish, read};
use std::{ffi::OsStr, path::PathBuf};

fn source(hidden: u64, folded: bool) -> SizeTree {
    let mut names = NameArena::default();
    let rows = [
        ("SOURCE_SECRET", 0, 1, 2, true, 90000),
        ("project", 0, 3, 2, true, 70000),
        ("HIDDEN_ROOT_SECRET", 0, 5, 1, true, hidden),
        ("a.md", 1, 0, 0, false, 10),
        ("private", 1, 6, 1, true, hidden),
        ("OUTSIDE_SECRET", 2, 0, 0, false, hidden),
        ("EXCLUDED_SECRET", 4, 0, 0, false, hidden),
    ];
    let nodes = rows
        .iter()
        .enumerate()
        .map(|(i, (name, parent, first, count, dir, bytes))| {
            let (name_off, name_len) = names.push(OsStr::new(name));
            let mut flags = if *dir { Flags::DIR } else { Flags::default() };
            if i == 3 && folded {
                flags.insert(Flags::HARDLINK_DUP);
            }
            Node {
                logical: *bytes,
                allocated: *bytes,
                parent: *parent,
                first_child: *first,
                child_count: *count,
                subtree_files: 400,
                name_off,
                name_len,
                flags,
                mtime: 12345,
                depth: 0,
            }
        })
        .collect();
    names.push(OsStr::new("UNREACHABLE_ARENA_SECRET"));
    let owners = Owners::from_parts(
        vec![Owner {
            id: OwnerId::Uid(77),
            name: Some("OWNER_SECRET".into()),
            kind: OwnerKind::Person,
        }],
        vec![0; 7],
    );
    SizeTree::from_parts(
        nodes,
        names,
        owners,
        PathBuf::from("/HOST_SECRET/drive"),
        ScanStats {
            image: Some("IMAGE_SECRET".into()),
            entries: hidden,
            ..ScanStats::default()
        },
    )
    .with_creation_times(vec![12345, 12345, 12345, u64::MAX, 12345, 12345, 12345])
}

fn selected(tree: &SizeTree, fields: Fields) -> View {
    publish(
        tree,
        10000,
        Selection {
            root: "project",
            alias: "shared",
            excludes: &["private".into()],
            fields,
        },
    )
    .unwrap()
}

#[test]
fn physically_omits_hidden_arenas_roots_owners_dates_and_directory_totals() {
    let view = selected(&source(99999, false), Fields::default());
    let bytes = encode(&view).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    for secret in [
        "SECRET", "private", "99999", "90000", "12345", "modified", "created",
    ] {
        // Field availability is public; individual timestamp values are absent.
        if matches!(secret, "modified" | "created") {
            continue;
        }
        assert!(!text.contains(secret), "leaked {secret}");
    }
    assert_eq!(view.entries.len(), 2);
    assert_eq!(view.entries[0].allocated, 10);
    assert_eq!(view.entries[0].apparent, 10);
    assert!(
        view.entries
            .iter()
            .all(|e| e.modified.is_none() && e.created.is_none())
    );
    let loaded = read(bytes.as_slice()).unwrap();
    assert_eq!(loaded.path(1), "shared/a.md");
    assert!(loaded.matches(&Query::parse("path ~ shared/a.md").unwrap(), 1, 20000));
    assert_eq!(
        bytes,
        encode(&selected(&source(55555, false), Fields::default())).unwrap(),
        "unshared sizes/stats changed artifact"
    );
}

#[test]
fn missing_fields_refuse_and_unknown_shared_dates_remain_unknown_under_not() {
    let no_dates = prepare(selected(&source(20, false), Fields::default())).unwrap();
    for q in [
        "owner = admin",
        "created > 1970-01-01",
        "not modified > 1970-01-01",
        "sparse",
        "hardlink",
        "partial",
        "file or not (owner = admin)",
    ] {
        assert!(
            no_dates.check_query(&Query::parse(q).unwrap()).is_err(),
            "accepted {q}"
        );
    }
    let dates = prepare(selected(
        &source(20, false),
        Fields {
            modified: true,
            created: true,
        },
    ))
    .unwrap();
    let q = Query::parse("not created > 1970-01-01").unwrap();
    dates.check_query(&q).unwrap();
    assert!(!dates.matches(&q, 1, 20000));
}

#[test]
fn selected_folded_hardlinks_are_not_misreported_as_zero_allocation() {
    assert!(
        publish(
            &source(10, true),
            0,
            Selection {
                root: "project",
                alias: "shared",
                excludes: &[],
                fields: Fields::default()
            }
        )
        .err()
        .unwrap()
        .starts_with("accounting_unsupported:")
    );
}

#[test]
fn imported_views_cannot_smuggle_fields_paths_graphs_or_hidden_aggregate_bytes() {
    let original = selected(&source(10, false), Fields::default());
    for edit in 0..7 {
        let mut view = original.clone();
        match edit {
            0 => view.entries[1].parent = 1,
            1 => view.entries[1].name = "../secret".into(),
            2 => view.entries[0].allocated += 999,
            3 => view.entries[1].modified = Some(12345),
            4 => view.alias = "/host".into(),
            5 => view.entries[1].created = Some(12345),
            _ => view.coverage.scope = "whole_drive".into(),
        }
        assert!(prepare(view).is_err(), "accepted mutation {edit}");
    }
    let mut json = serde_json::to_value(original).unwrap();
    json["original_root"] = serde_json::json!("HOST_SECRET");
    let error = read(serde_json::to_vec(&json).unwrap().as_slice())
        .err()
        .unwrap();
    assert!(!error.contains("HOST_SECRET"));
    assert!(error.starts_with("view_invalid:"));
}

#[test]
fn repeated_sibling_names_and_nonportable_selection_are_refused() {
    let mut view = selected(&source(10, false), Fields::default());
    view.entries.push(view.entries[1].clone());
    view.entries[0].allocated = 20;
    view.entries[0].apparent = 20;
    assert!(prepare(view).is_err());
    for path in [
        "../project",
        "/project",
        "project\\private",
        "C:project",
        "project//private",
    ] {
        assert!(
            publish(
                &source(10, false),
                0,
                Selection {
                    root: path,
                    alias: "shared",
                    excludes: &[],
                    fields: Fields::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn nested_depth_and_byte_budgets_are_enforced() {
    let mut view = selected(&source(10, false), Fields::default());
    view.entries.truncate(1);
    view.entries[0].allocated = 0;
    view.entries[0].apparent = 0;
    for i in 0..129 {
        let mut entry = view.entries[0].clone();
        entry.parent = i;
        entry.name = "d".into();
        view.entries.push(entry);
    }
    assert!(prepare(view).err().unwrap().starts_with("view_limit:"));
    struct Oversize;
    impl std::io::Read for Oversize {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            b.fill(b' ');
            Ok(b.len())
        }
    }
    assert!(read(Oversize).err().unwrap().starts_with("view_limit:"));
}

#[test]
fn mount_boundaries_are_not_empty_and_interruption_is_not_an_access_failure() {
    let base = source(100, false);
    let copy = |stats: ScanStats, boundary: bool| {
        let mut nodes = base.nodes_slice().to_vec();
        if boundary {
            nodes[4].flags.insert(Flags::OTHER_DEVICE);
            nodes[4].child_count = 0;
        }
        let mut names = NameArena::default();
        names.push_bytes(base.names_bytes());
        SizeTree::from_parts(
            nodes,
            names,
            Owners::default(),
            PathBuf::from("/HOST_SECRET"),
            stats,
        )
    };
    let tree = copy(
        ScanStats {
            skipped_other_device: 1,
            ..ScanStats::default()
        },
        true,
    );
    let view = publish(
        &tree,
        10000,
        Selection {
            root: "project",
            alias: "shared",
            excludes: &[],
            fields: Fields::default(),
        },
    )
    .unwrap();
    assert!(view.entries[2].boundary);
    assert!(view.coverage.lower_bound);
    assert!(!view.coverage.source_access_limited);
    assert!(!view.coverage.source_incomplete);
    let prepared = prepare(view).unwrap();
    for q in ["empty", "not empty"] {
        assert!(!prepared.matches(&Query::parse(q).unwrap(), 2, 20000));
    }
    let interrupted = selected(
        &copy(
            ScanStats {
                depth_limited: true,
                partial: true,
                ..ScanStats::default()
            },
            false,
        ),
        Fields::default(),
    );
    assert!(interrupted.coverage.source_incomplete);
    assert!(!interrupted.coverage.source_access_limited);
    assert!(interrupted.coverage.lower_bound);
}

#[test]
fn allocation_quality_and_unknown_boundaries_survive_validation_without_source_stats() {
    let original = source(123, false);
    for exact in [true, false] {
        let mut names = NameArena::default();
        names.push_bytes(original.names_bytes());
        let tree = SizeTree::from_parts(
            original.nodes_slice().to_vec(),
            names,
            Owners::default(),
            PathBuf::from("/HOST_SECRET"),
            ScanStats {
                exact_allocated: exact,
                ..ScanStats::default()
            },
        );
        let view = selected(&tree, Fields::default());
        let prepared = read(encode(&view).unwrap().as_slice()).unwrap();
        assert_eq!(prepared.view().coverage.allocation_exact, exact);
        assert_eq!(prepared.view().entries[0].allocated, 10);
    }
    let mut bad = selected(&original, Fields::default());
    bad.entries[1].boundary = true;
    assert!(
        prepare(bad).is_err(),
        "unknown boundary cannot claim complete totals"
    );
    let mut bidi = selected(&original, Fields::default());
    bidi.entries[1].name = "report\u{202e}txt".into();
    assert!(prepare(bidi).is_err());
}

#[test]
fn file_parents_backwards_parent_groups_and_numeric_overflow_are_refused() {
    let original = selected(&source(20, false), Fields::default());
    let mut file_parent = original.clone();
    let mut child = file_parent.entries[1].clone();
    child.parent = 1;
    child.name = "child".into();
    file_parent.entries.push(child);
    assert!(prepare(file_parent).err().unwrap().contains("graph"));
    let mut backwards = original.clone();
    let mut directory = backwards.entries[0].clone();
    directory.parent = 0;
    directory.name = "b".into();
    backwards.entries.push(directory);
    let mut child = backwards.entries[1].clone();
    child.parent = 2;
    child.name = "c".into();
    backwards.entries.push(child);
    let mut child = backwards.entries[1].clone();
    child.parent = 0;
    child.name = "z".into();
    backwards.entries.push(child);
    assert!(prepare(backwards).err().unwrap().contains("graph"));
    let mut overflow = original;
    overflow.entries[1].allocated = u64::MAX;
    let mut second = overflow.entries[1].clone();
    second.allocated = 1;
    second.name = "b".into();
    overflow.entries.push(second);
    assert!(prepare(overflow).err().unwrap().contains("overflow"));
}
