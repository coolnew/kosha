// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Authored match identities distinguish recorded file length from allocation.

use kosha_core::{Flags, NameArena, Node, NodeId, Owners, ScanStats, SizeTree};
use kosha_search::{SearchContext, SearchIndex, SearchMode, translate_with_mode};
use std::{
    ffi::OsStr,
    sync::{Arc, atomic::AtomicBool},
};

const MIB: u64 = 1 << 20;
const NOW: u32 = 1_795_003_200;

fn fixture() -> Arc<SizeTree> {
    let rows = [
        ("root", 31 * MIB + 23, 33 * MIB, Flags::DIR, 0, 1, 7),
        ("compressed.bin", 20 * MIB, MIB, Flags::default(), 0, 0, 0),
        ("padded.bin", MIB, 20 * MIB, Flags::default(), 0, 0, 0),
        ("resident.txt", 23, 0, Flags::default(), 0, 0, 0),
        ("empty.txt", 0, 0, Flags::default(), 0, 0, 0),
        ("ordinary.bin", 8 * MIB, 8 * MIB, Flags::default(), 0, 0, 0),
        ("unreadable.bin", 0, 0, Flags::UNREADABLE, 0, 0, 0),
        ("nested", 2 * MIB, 4 * MIB, Flags::DIR, 0, 8, 1),
        ("leaf.bin", 2 * MIB, 4 * MIB, Flags::default(), 7, 0, 0),
    ];
    let mut names = NameArena::default();
    let nodes = rows
        .into_iter()
        .enumerate()
        .map(
            |(index, (name, logical, allocated, flags, parent, first_child, child_count))| {
                let (name_off, name_len) = names.push(OsStr::new(name));
                Node {
                    logical,
                    allocated,
                    flags,
                    parent,
                    first_child,
                    child_count,
                    subtree_files: if index == 0 { 7 } else { 1 },
                    name_off,
                    name_len,
                    mtime: NOW,
                    depth: if index == 0 {
                        0
                    } else if parent == 0 {
                        1
                    } else {
                        2
                    },
                }
            },
        )
        .collect();
    Arc::new(SizeTree::from_parts(
        nodes,
        names,
        Owners::default(),
        "C:/basis".into(),
        ScanStats::default(),
    ))
}

#[test]
fn qualified_size_conditions_use_the_requested_metadata_on_every_branch() {
    let tree = fixture();
    let index = SearchIndex::new(tree.clone());
    for (input, expected) in [
        (
            "files with apparent size over 10 MB",
            vec!["compressed.bin"],
        ),
        (
            "files logical size over ten megabytes",
            vec!["compressed.bin"],
        ),
        ("files allocated size over 10 MB", vec!["padded.bin"]),
        ("files on-disk size over 10 MB", vec!["padded.bin"]),
        ("files on disk size over 10 MB", vec!["padded.bin"]),
        ("files size on disk over 10 MB", vec!["padded.bin"]),
        ("files over 10 MB", vec!["padded.bin"]),
        (
            "files apparent size over 10 MB and allocated size under 2 MB",
            vec!["compressed.bin"],
        ),
        ("files apparent size exactly 0 bytes", vec!["empty.txt"]),
        (
            "files allocated size exactly 0 bytes",
            vec!["empty.txt", "resident.txt"],
        ),
        (
            "files apparent size between one and eight MB",
            vec!["leaf.bin", "ordinary.bin", "padded.bin"],
        ),
        (
            "files size on disk between 1 and 8 MB",
            vec!["compressed.bin", "leaf.bin", "ordinary.bin"],
        ),
        (
            "files not apparent size over 10 MB",
            vec![
                "empty.txt",
                "leaf.bin",
                "ordinary.bin",
                "padded.bin",
                "resident.txt",
            ],
        ),
        (
            "files not allocated size exactly 0 bytes",
            vec!["compressed.bin", "leaf.bin", "ordinary.bin", "padded.bin"],
        ),
        ("folders logical size exactly 2 MB", vec!["nested"]),
        ("folders allocated size exactly 4 MB", vec!["nested"]),
        ("files named \"apparent\"", vec![]),
    ] {
        let translated = translate_with_mode(
            input,
            SearchContext::new(NOW, [2026, 11, 18]),
            SearchMode::Natural,
        );
        assert!(
            translated.is_valid(),
            "{input}: {:?}",
            translated.diagnostics
        );
        let query = translated.query.unwrap();
        let edited = kosha_query::Query::parse(translated.dsl.as_deref().unwrap()).unwrap();
        let result = index
            .run(&query, NOW, 100, &AtomicBool::new(false))
            .unwrap();
        let mut actual: Vec<_> = result
            .matches
            .iter()
            .map(|&id| tree.name_lossy(id).into_owned())
            .collect();
        actual.sort();
        assert_eq!(actual, expected, "{input}: {:?}", translated.dsl);
        for i in 1..tree.len() {
            let id = NodeId::from_index(i);
            assert_eq!(
                query.matches(&tree, id, NOW),
                edited.matches(&tree, id, NOW),
                "{input}: {}",
                tree.name_lossy(id)
            );
            assert_eq!(
                query.matches(&tree, id, NOW),
                actual
                    .iter()
                    .any(|name| name == tree.name_lossy(id).as_ref()),
                "indexed fallback differs: {input}"
            );
        }
    }
}

#[test]
fn malformed_or_conflicting_size_basis_cannot_execute_a_weaker_search() {
    let context = SearchContext::new(NOW, [2026, 11, 18]);
    for input in [
        "files apparent size",
        "files apparent size over",
        "files allocated size between 1 and",
        "folders apparent size over 10 files",
        "folders allocated size at least 10 children",
        "files apparent allocated size over 1 MB",
        "files logical size on disk over 1 MB",
        "files apparent size over 1 MB or allocated size under 1 MB",
    ] {
        let result = translate_with_mode(input, context, SearchMode::Natural);
        assert!(
            !result.is_valid() && result.dsl.is_none() && result.query.is_none(),
            "{input}: {:?}",
            result.dsl
        );
    }
}

#[test]
fn partial_qualified_conditions_keep_valid_spans_and_never_panic() {
    let context = SearchContext::new(NOW, [2026, 11, 18]);
    for text in [
        "files apparent size between one hundred and two hundred MB",
        "files named 'résumé' logical size greater than half a GB",
        "files not allocated size exactly zero bytes",
        "files apparent size over 10 MB and size on disk under one MB",
    ] {
        for end in text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
        {
            let input = &text[..end];
            let result = translate_with_mode(input, context, SearchMode::Natural);
            for entity in &result.entities {
                assert!(
                    input.get(entity.span.clone()).is_some(),
                    "invalid span for {input}"
                );
            }
            if !result.diagnostics.is_empty() {
                assert!(
                    result.query.is_none() && result.dsl.is_none(),
                    "unresolved input executes: {input}"
                );
            }
        }
    }
}
