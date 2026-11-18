// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree};
use kosha_query::{Expr, Field, Query, Value};
use kosha_search::{
    SearchContext, SearchIndex, SearchMode, translate_with_mode, translate_with_resolver,
};

#[test]
fn automatic_routing_preserves_leading_flag_dsl() {
    let context = SearchContext::new(1795003200, [2026, 11, 18]);
    for text in [
        "dir and items > 1000",
        "dir and not path ~ cache",
        "EXT IN (md, txt)",
        "not file",
        "name startswith report",
        "name endswith .pdf",
        "not name startswith report",
    ] {
        let translated = kosha_search::translate(text, context);
        assert_eq!(translated.mode, SearchMode::Dsl, "{text}");
        assert!(
            translated.is_valid(),
            "{text}: {:?}",
            translated.diagnostics
        );
        assert_eq!(translated.dsl.as_deref(), Some(text));
    }
    assert_eq!(
        kosha_search::translate("files greater than 10 MB", context).mode,
        SearchMode::Natural
    );
    for text in [
        "not pdf",
        "files in cache",
        "files created in last 24 hours",
    ] {
        let translated = kosha_search::translate(text, context);
        assert_eq!(translated.mode, SearchMode::Natural, "{text}");
        assert!(
            translated.is_valid(),
            "{text}: {:?}",
            translated.diagnostics
        );
    }
}
use std::{
    ffi::OsStr,
    sync::{Arc, atomic::AtomicBool},
};

fn tree() -> Arc<SizeTree> {
    let mut names = NameArena::default();
    let specs = [
        ("root", 0, 54, Flags::DIR, 1, 3),
        ("docs", 0, 18, Flags::DIR, 4, 3),
        ("source", 0, 6, Flags::DIR, 7, 2),
        ("data.zip", 0, 30, Flags::default(), 0, 0),
        ("report.docx", 1, 12, Flags::default(), 0, 0),
        ("notes.MD", 1, 5, Flags::default(), 0, 0),
        ("unknown.txt", 1, 1, Flags::default(), 0, 0),
        ("main.c", 2, 4, Flags::default(), 0, 0),
        ("main.h", 2, 2, Flags::default(), 0, 0),
    ];
    let nodes = specs
        .iter()
        .enumerate()
        .map(|(i, (name, parent, mb, flags, first, count))| {
            let (name_off, name_len) = names.push(OsStr::new(name));
            Node {
                logical: mb * 1048576,
                allocated: mb * 1048576,
                parent: *parent,
                first_child: *first,
                child_count: *count,
                subtree_files: 0,
                name_off,
                mtime: if i == 6 { 0 } else { 1794956400 },
                name_len,
                flags: *flags,
                depth: if i == 0 {
                    0
                } else if *parent == 0 {
                    1
                } else {
                    2
                },
            }
        })
        .collect();
    Arc::new(
        SizeTree::from_parts(
            nodes,
            names,
            Owners::default(),
            "C:/fixture".into(),
            ScanStats::default(),
        )
        .with_creation_times(vec![
            u64::MAX,
            1794268800,
            u64::MAX,
            1794956400,
            1794956400,
            1794956400,
            u64::MAX,
            1794956400,
            1794956400,
        ]),
    )
}

#[test]
fn bounded_results_and_complete_totals_equal_existing_query_engine() {
    let tree = tree();
    let index = SearchIndex::new(tree.clone());
    for text in [
        "file",
        "dir",
        "file or dir",
        "size > 3mb",
        "ext in (md, c)",
        "path ~ docs",
        "name ~ main",
        "not ext = md",
        "created > 2026-01-01",
    ] {
        let query = Query::parse(text).unwrap();
        for limit in [0, 1, 3, 100] {
            let expected = query.run(&tree, 1795003200, limit);
            let actual = index
                .run(&query, 1795003200, limit, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(actual.matches, expected.matches, "{text} limit={limit}");
            assert_eq!(actual.total_matched, expected.total_matched, "{text}");
            assert_eq!(actual.total_allocated, expected.total_allocated, "{text}");
        }
    }
}

#[test]
fn cancelled_work_never_returns_partial_counts() {
    let tree = tree();
    let cancel = AtomicBool::new(true);
    assert!(SearchIndex::new_cancellable(tree.clone(), &cancel).is_none());
    assert!(
        SearchIndex::new(tree)
            .run(&Query::parse("file").unwrap(), 0, 10, &cancel)
            .is_none()
    );
}

#[test]
fn categories_and_natural_clauses_execute_the_expected_predicates() {
    let tree = tree();
    let index = SearchIndex::new(tree.clone());
    for (input, expected) in [
        ("files greater than 10 MB", vec!["data.zip", "report.docx"]),
        ("files ending with md", vec!["notes.MD"]),
        ("files which are c code", vec!["main.c"]),
        ("word documents", vec!["report.docx"]),
        ("folders created last week", vec!["docs"]),
    ] {
        let parsed = translate_with_mode(
            input,
            SearchContext::new(1795003200, [2026, 11, 18]),
            SearchMode::Natural,
        );
        let results = index
            .run(
                parsed.query.as_ref().unwrap(),
                1795003200,
                20,
                &AtomicBool::new(false),
            )
            .unwrap();
        let names: Vec<_> = results
            .matches
            .iter()
            .map(|id| tree.name_lossy(*id).into_owned())
            .collect();
        assert_eq!(names, expected, "{input}");
    }
}

#[test]
fn explicit_local_boundary_resolver_preserves_dst_changes() {
    // New York 2026-11-01 starts at 04:00 UTC; Nov 2 starts at 05:00 UTC.
    let parsed = translate_with_resolver(
        "files modified on 2026-11-01",
        SearchContext::new(1795003200, [2026, 11, 18]),
        SearchMode::Natural,
        |date| match date {
            [2026, 11, 1] => Some(1793505600),
            [2026, 11, 2] => Some(1793595600),
            _ => None,
        },
    );
    assert_eq!(
        parsed.dsl.as_deref(),
        Some("file and modified >= 2026-11-01 and modified < 2026-11-02")
    );
    fn collect(expr: &Expr, out: &mut Vec<u64>) {
        match expr {
            Expr::And(a, b) => {
                collect(a, out);
                collect(b, out)
            }
            Expr::Compare {
                field: Field::Modified,
                value: Value::Number(n),
                ..
            } => out.push(*n),
            _ => {}
        }
    }
    let mut boundaries = vec![];
    collect(parsed.query.unwrap().expression(), &mut boundaries);
    assert_eq!(boundaries, vec![1793505600, 1793595600]);
    assert_eq!(boundaries[1] - boundaries[0], 25 * 3600);
}

#[test]
fn literal_mode_and_natural_mode_cannot_accidentally_execute_dsl() {
    let context = SearchContext::new(1795003200, [2026, 11, 18]);
    let literal = translate_with_mode("size > 10mb", context, SearchMode::Literal);
    assert!(matches!(
        literal.query.unwrap().expression(),
        Expr::NameContains(_)
    ));
    assert!(translate_with_mode("size > 10mb", context, SearchMode::Natural).is_valid());
    assert!(
        translate_with_mode("files over 10mb nonsense", context, SearchMode::Natural)
            .query
            .is_none()
    );
}

#[test]
fn top_k_honors_requested_order_across_the_whole_tree() {
    let tree = tree();
    let index = SearchIndex::new(tree.clone());
    let query = Query::parse("file").unwrap();
    for descending in [false, true] {
        let compare = |a, b| {
            let order = tree.name(a).cmp(tree.name(b));
            if descending { order.reverse() } else { order }
        };
        let mut expected = query.search(&tree, 0, 100);
        expected.sort_by(|a, b| compare(*a, *b));
        expected.truncate(2);
        let actual = index
            .run_sorted(&query, 0, 2, &AtomicBool::new(false), compare)
            .unwrap();
        assert_eq!(actual.matches, expected);
        assert_eq!(actual.total_matched, 6);
        assert_eq!(actual.total_allocated, 54 * 1048576);
    }
}

#[test]
fn input_limits_reject_deep_expression_chains() {
    let context = SearchContext::new(0, [2026, 11, 18]);
    assert!(!translate_with_mode(&"files ".repeat(1000), context, SearchMode::Natural).is_valid());
    assert!(!translate_with_mode(&"x".repeat(20000), context, SearchMode::Natural).is_valid());
    for input in [
        format!("files {}", "not ".repeat(300)),
        format!("files {}", "sparse ".repeat(129)),
    ] {
        let result = translate_with_mode(&input, context, SearchMode::Natural);
        assert!(result.query.is_none() && result.dsl.is_none());
    }
}

#[test]
fn empty_categories_use_apparent_size_even_when_allocation_is_zero() {
    let source = tree();
    let mut nodes = source.nodes_slice().to_vec();
    let mut names = NameArena::default();
    names.append_raw(source.names_bytes());
    for (index, name, logical, allocated) in [
        (5, "empty.JPG", 0, 0),
        (7, "empty.docx", 0, 4096),
        (4, "resident.docx", 23, 0),
    ] {
        let (off, len) = names.push(OsStr::new(name));
        nodes[index].name_off = off;
        nodes[index].name_len = len;
        nodes[index].logical = logical;
        nodes[index].allocated = allocated;
    }
    let tree = Arc::new(SizeTree::from_parts(
        nodes,
        names,
        Owners::default(),
        "C:/fixture".into(),
        ScanStats::default(),
    ));
    let index = SearchIndex::new(tree.clone());
    for (input, expected) in [
        ("empty images", vec!["empty.JPG"]),
        ("documents empty", vec!["empty.docx"]),
    ] {
        let translated = translate_with_mode(
            input,
            SearchContext::new(1795003200, [2026, 11, 18]),
            SearchMode::Natural,
        );
        let query = translated.query.unwrap();
        let result = index
            .run(&query, 1795003200, 10, &AtomicBool::new(false))
            .unwrap();
        let names: Vec<_> = result
            .matches
            .iter()
            .map(|id| tree.name_lossy(*id).into_owned())
            .collect();
        assert_eq!(names, expected, "{input}");
        let reparsed = Query::parse(query.source()).unwrap();
        for id in (1..tree.len()).map(kosha_core::NodeId::from_index) {
            assert_eq!(
                query.matches(&tree, id, 1795003200),
                reparsed.matches(&tree, id, 1795003200)
            );
        }
    }
}

#[test]
fn indexed_boolean_expressions_match_scan_for_special_names_and_kinds() {
    let mut arena = NameArena::default();
    let specs = [
        ("root", 0, Flags::DIR, 1, 5),
        ("parent.MD", 0, Flags::DIR, 6, 1),
        (".gitignore", 0, Flags::default(), 0, 0),
        ("trailing.", 0, Flags::default(), 0, 0),
        ("unicode.É", 0, Flags::default(), 0, 0),
        ("link.MD", 0, Flags::SYMLINK, 0, 0),
        ("child.md", 1, Flags::default(), 0, 0),
    ];
    let nodes = specs
        .iter()
        .enumerate()
        .map(|(i, (name, parent, flags, first, count))| {
            let (name_off, name_len) = arena.push(OsStr::new(name));
            Node {
                logical: 100,
                allocated: 100,
                parent: *parent,
                first_child: *first,
                child_count: *count,
                subtree_files: 0,
                name_off,
                mtime: 0,
                name_len,
                flags: *flags,
                depth: if i == 0 {
                    0
                } else if *parent == 0 {
                    1
                } else {
                    2
                },
            }
        })
        .collect();
    let tree = Arc::new(SizeTree::from_parts(
        nodes,
        arena,
        Owners::default(),
        "C:/fixture".into(),
        ScanStats::default(),
    ));
    let index = SearchIndex::new(tree.clone());
    let leaves = [
        "ext = md",
        "ext = ''",
        "ext = É",
        "ext = é",
        "ext in (md, É)",
        "ext = absent",
        "dir",
        "link",
        "file",
        "not ext = md",
    ];
    for a in leaves {
        for b in leaves {
            for operator in ["and", "or"] {
                let query = Query::parse(&format!("({a}) {operator} ({b})")).unwrap();
                let expected = query.run(&tree, 0, 2);
                let actual = index.run(&query, 0, 2, &AtomicBool::new(false)).unwrap();
                assert_eq!(actual.matches, expected.matches, "{}", query.source());
                assert_eq!(
                    actual.total_matched,
                    expected.total_matched,
                    "{}",
                    query.source()
                );
                assert_eq!(
                    actual.total_allocated,
                    expected.total_allocated,
                    "{}",
                    query.source()
                );
            }
        }
    }
}

#[test]
fn extension_label_cap_falls_back_without_dropping_unindexed_matches() {
    let count = 5000;
    let mut arena = NameArena::default();
    let (name_off, name_len) = arena.push(OsStr::new("root"));
    let mut nodes = vec![Node {
        logical: count,
        allocated: count,
        parent: 0,
        first_child: 1,
        child_count: count as u32,
        subtree_files: count as u32,
        name_off,
        mtime: 0,
        name_len,
        flags: Flags::DIR,
        depth: 0,
    }];
    for i in 0..count {
        let (name_off, name_len) = arena.push(OsStr::new(&format!("entry.x{i:04}")));
        nodes.push(Node {
            logical: 1,
            allocated: 1,
            parent: 0,
            first_child: 0,
            child_count: 0,
            subtree_files: 0,
            name_off,
            mtime: 0,
            name_len,
            flags: Flags::default(),
            depth: 1,
        });
    }
    let tree = Arc::new(SizeTree::from_parts(
        nodes,
        arena,
        Owners::default(),
        "C:/fixture".into(),
        ScanStats::default(),
    ));
    let index = SearchIndex::new(tree.clone());
    for text in [
        "ext = x0001",
        "ext = x4999",
        "ext = x9999",
        "ext in (x0001, x4999)",
        "not ext = x4999",
        "ext = x0001 or name ~ 4999",
    ] {
        let query = Query::parse(text).unwrap();
        let expected = query.run(&tree, 0, 10);
        let actual = index.run(&query, 0, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(actual.matches, expected.matches, "{text}");
        assert_eq!(actual.total_matched, expected.total_matched, "{text}");
        assert_eq!(actual.total_allocated, expected.total_allocated, "{text}");
    }
}
