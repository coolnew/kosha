// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree};
use kosha_search::{CreationCoverage, SearchContext, SearchIndex, SearchMode, translate_with_mode};
use std::{
    ffi::OsStr,
    sync::{Arc, atomic::AtomicBool},
};

fn fixture(with_birth_times: bool) -> Arc<SizeTree> {
    // UTC, Monday 2026-11-09 inclusive through Monday 2026-11-16 exclusive.
    let rows = [
        ("week-start.txt", Flags::default(), 1_794_182_400),
        ("before.txt", Flags::default(), 1_794_182_399),
        ("next-week.txt", Flags::default(), 1_794_787_200),
        ("week-end-folder", Flags::DIR, 1_794_787_199),
        ("unknown.txt", Flags::default(), u64::MAX),
        ("unknown-folder", Flags::DIR, u64::MAX),
    ];
    let mut names = NameArena::default();
    let (name_off, name_len) = names.push(OsStr::new("root"));
    let mut nodes = vec![Node {
        logical: 4,
        allocated: 4,
        parent: 0,
        first_child: 1,
        child_count: 6,
        subtree_files: 4,
        name_off,
        name_len,
        mtime: 1_795_003_200,
        flags: Flags::DIR,
        depth: 0,
    }];
    let mut created = vec![1_794_182_400];
    for (name, flags, born) in rows {
        let (name_off, name_len) = names.push(OsStr::new(name));
        nodes.push(Node {
            logical: 1,
            allocated: 1,
            parent: 0,
            first_child: 0,
            child_count: 0,
            subtree_files: u32::from(!flags.contains(Flags::DIR)),
            name_off,
            name_len,
            // Deliberately unrelated: modified dates cannot stand in for birth.
            mtime: 1_795_003_200,
            flags,
            depth: 1,
        });
        created.push(born);
    }
    Arc::new(
        SizeTree::from_parts(
            nodes,
            names,
            Owners::default(),
            "C:/week".into(),
            ScanStats::default(),
        )
        .with_creation_times(if with_birth_times { created } else { vec![] }),
    )
}

#[test]
fn files_and_folders_created_last_week_use_recorded_birth_times() {
    let index = SearchIndex::new(fixture(true));
    assert_eq!(
        index.creation_coverage(),
        CreationCoverage { known: 4, total: 6 }
    );
    for (phrase, expected) in [
        ("files created last week", "week-start.txt"),
        ("folders created last week", "week-end-folder"),
    ] {
        let result = translate_with_mode(
            phrase,
            SearchContext::new(1_795_003_200, [2026, 11, 18]),
            SearchMode::Natural,
        );
        assert!(result.is_valid(), "{:?}", result.diagnostics);
        let query = result.query.unwrap();
        let found = index
            .run(&query, 1_795_003_200, 10, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(found.total_matched, 1, "{phrase}");
        assert_eq!(index.tree().name_lossy(found.matches[0]), expected);
    }
}

#[test]
fn old_or_all_unknown_metadata_is_not_a_proven_empty_date_result() {
    for all_unknown in [false, true] {
        let mut tree = Arc::try_unwrap(fixture(false)).unwrap_or_else(|_| unreachable!());
        if all_unknown {
            let count = tree.len();
            tree = tree.with_creation_times(vec![u64::MAX; count]);
        }
        let index = SearchIndex::new(Arc::new(tree));
        assert_eq!(
            index.creation_coverage(),
            CreationCoverage { known: 0, total: 6 }
        );
        let result = translate_with_mode(
            "files created last week",
            SearchContext::new(1_795_003_200, [2026, 11, 18]),
            SearchMode::Natural,
        );
        let found = index
            .run(
                &result.query.unwrap(),
                1_795_003_200,
                10,
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(found.total_matched, 0);
    }
}
