// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Independently authored expected node sets for elapsed timestamp conditions.
//! These are instant comparisons, unlike plain "N days ago" calendar periods.

use kosha_core::{Flags, NameArena, Node, NodeId, Owners, ScanStats, SizeTree};
use kosha_query::Query;
use kosha_search::{
    SearchContext, SearchIndex, SearchMode, translate_with_mode, translate_with_resolver,
};
use std::{
    ffi::OsStr,
    sync::{Arc, atomic::AtomicBool},
};

const NOW: u32 = 1795003200;
const BOUNDARY: u32 = 1792411200; // exactly thirty days before the captured clock
fn context() -> SearchContext {
    SearchContext::new(NOW, [2026, 11, 18])
}

fn fixture() -> Arc<SizeTree> {
    let cases = [
        ("before", BOUNDARY - 1),
        ("at", BOUNDARY),
        ("after", BOUNDARY + 1),
        ("now", NOW),
        ("future", NOW + 1),
        ("unknown", 0),
        ("epoch", 0),
    ];
    let mut names = NameArena::default();
    let mut nodes = vec![];
    let mut births = vec![];
    let (name_off, name_len) = names.push(OsStr::new("root"));
    nodes.push(Node {
        logical: 14,
        allocated: 14,
        parent: 0,
        first_child: 1,
        child_count: 14,
        subtree_files: 7,
        name_off,
        name_len,
        mtime: 0,
        flags: Flags::DIR,
        depth: 0,
    });
    births.push(u64::MAX);
    for (prefix, flags) in [("file", Flags::default()), ("folder", Flags::DIR)] {
        for (suffix, modified) in cases {
            let (name_off, name_len) = names.push(OsStr::new(&format!("{prefix}_{suffix}")));
            nodes.push(Node {
                logical: 1,
                allocated: 1,
                parent: 0,
                first_child: 0,
                child_count: 0,
                subtree_files: 0,
                name_off,
                name_len,
                mtime: modified,
                flags,
                depth: 1,
            });
            births.push(if suffix == "unknown" {
                u64::MAX
            } else {
                u64::from(modified)
            });
        }
    }
    Arc::new(
        SizeTree::from_parts(
            nodes,
            names,
            Owners::default(),
            "C:/timestamp-age".into(),
            ScanStats::default(),
        )
        .with_creation_times(births),
    )
}

#[test]
fn strict_inclusive_and_negated_conditions_select_independent_expected_sets() {
    let tree = fixture();
    let index = SearchIndex::new(tree.clone());
    // Expected suffixes are authored from the fixture's timeline, rather than
    // obtained by executing another version of the predicate under test.
    let comparisons = [
        (
            "more than",
            "<",
            &["before"][..],
            &["at", "after", "now", "future"][..],
            true,
        ),
        (
            "at least",
            "<=",
            &["before", "at"][..],
            &["after", "now", "future"][..],
            true,
        ),
        (
            "less than",
            ">",
            &["after", "now"][..],
            &["before", "at", "future"][..],
            false,
        ),
        (
            "at most",
            ">=",
            &["at", "after", "now"][..],
            &["before", "future"][..],
            false,
        ),
    ];
    for field in ["created", "modified"] {
        for (subject, prefix, flag) in [("files", "file", "file"), ("folders", "folder", "dir")] {
            for (phrase, operator, positive, negative, epoch_positive) in comparisons {
                for negated in [false, true] {
                    let input = format!(
                        "{subject} {}{field} {phrase} thirty days ago",
                        if negated { "not " } else { "" }
                    );
                    let translated = translate_with_mode(&input, context(), SearchMode::Natural);
                    assert!(
                        translated.is_valid(),
                        "{input}: {:?}",
                        translated.diagnostics
                    );
                    let mut expected: Vec<_> = (if negated { negative } else { positive })
                        .iter()
                        .map(|suffix| format!("{prefix}_{suffix}"))
                        .collect();
                    // Creation epoch zero is known; mtime zero is the legacy
                    // unknown sentinel. Neither unknown can match under NOT.
                    if field == "created" && epoch_positive != negated {
                        expected.push(format!("{prefix}_epoch"));
                    }
                    expected.sort();
                    let inner = format!("{field} {operator} @{BOUNDARY} and {field} <= @{NOW}");
                    let expected_dsl = if negated {
                        format!("{flag} and not ({inner})")
                    } else {
                        format!("{flag} and {inner}")
                    };
                    assert_eq!(translated.dsl.as_deref(), Some(expected_dsl.as_str()));
                    let query = translated.query.unwrap();
                    let actual = index
                        .run(&query, NOW, 100, &AtomicBool::new(false))
                        .unwrap();
                    let mut names: Vec<_> = actual
                        .matches
                        .iter()
                        .map(|id| tree.name_lossy(*id).into_owned())
                        .collect();
                    names.sort();
                    assert_eq!(names, expected, "{input}");
                    assert_eq!(actual.total_matched, expected.len());
                    assert_eq!(actual.total_allocated, expected.len() as u64);
                    let reparsed = Query::parse(&expected_dsl).unwrap();
                    for id in (0..tree.len()).map(NodeId::from_index) {
                        assert_eq!(
                            query.matches(&tree, id, NOW),
                            reparsed.matches(&tree, id, NOW),
                            "{input}: {}",
                            tree.name_lossy(id)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn zero_duration_and_underflow_are_checked_without_ambient_calendar_resolution() {
    let tree = fixture();
    let index = SearchIndex::new(tree.clone());
    for (input, expected) in [
        ("files created at most zero seconds ago", vec!["file_now"]),
        ("files modified less than zero seconds ago", vec![]),
    ] {
        let translated = translate_with_resolver(input, context(), SearchMode::Natural, |_| {
            panic!("elapsed comparisons must not resolve local midnight")
        });
        let query = translated.query.unwrap();
        let result = index
            .run(&query, NOW, 100, &AtomicBool::new(false))
            .unwrap();
        let names: Vec<_> = result
            .matches
            .iter()
            .map(|id| tree.name_lossy(*id).into_owned())
            .collect();
        assert_eq!(names, expected);
    }
    let underflow = translate_with_mode(
        "files created more than two seconds ago",
        SearchContext::new(1, [1970, 1, 1]),
        SearchMode::Natural,
    );
    assert!(underflow.query.is_none() && underflow.dsl.is_none());
    assert!(
        underflow
            .diagnostics
            .iter()
            .any(|d| d.message.contains("epoch"))
    );
    let calendar = translate_with_mode(
        "files modified two days ago",
        context(),
        SearchMode::Natural,
    );
    assert_eq!(
        calendar.dsl.as_deref(),
        Some("file and modified >= 2026-11-16 and modified < 2026-11-17")
    );
}

#[test]
fn partial_comparative_phrases_preserve_utf8_spans_and_never_execute_a_weaker_query() {
    let phrase = "files named \"résumé\" modified at most one point five minutes ago";
    let translated = translate_with_mode(phrase, context(), SearchMode::Natural);
    assert!(translated.is_valid());
    let entity = translated.entities.last().unwrap();
    assert_eq!(
        &phrase[entity.span.clone()],
        "modified at most one point five minutes ago"
    );
    assert_eq!(entity.value, "age <= 90s");
    for input in [
        phrase,
        "folders created at least two weeks ago",
        "files not modified more than thirty days ago",
    ] {
        for (at, _) in input
            .char_indices()
            .chain(std::iter::once((input.len(), '\0')))
        {
            let result = translate_with_mode(&input[..at], context(), SearchMode::Natural);
            for span in result
                .entities
                .iter()
                .map(|e| &e.span)
                .chain(result.diagnostics.iter().map(|d| &d.span))
            {
                assert!(span.start <= span.end && span.end <= at);
                assert!(
                    input[..at].is_char_boundary(span.start)
                        && input[..at].is_char_boundary(span.end)
                );
            }
        }
    }
    for input in [
        "files modified more than thirty days",
        "files modified more than thirty days agone",
        "files created less than hours ago",
        "files created at most half a second ago",
        "files modified at least seven days ago junk",
    ] {
        let result = translate_with_mode(input, context(), SearchMode::Natural);
        assert!(result.query.is_none() && result.dsl.is_none(), "{input}");
    }
}
