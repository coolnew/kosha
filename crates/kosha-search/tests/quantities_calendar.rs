// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use kosha_query::{Expr, Field, Value};
use kosha_search::{SearchContext, SearchMode, translate_with_mode, translate_with_resolver};

fn context() -> SearchContext {
    SearchContext::new(1795003200, [2026, 11, 18])
}
fn numbers(expr: &Expr, field: Field, out: &mut Vec<u64>) {
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => {
            numbers(a, field, out);
            numbers(b, field, out)
        }
        Expr::Not(inner) => numbers(inner, field, out),
        Expr::Compare {
            field: actual,
            value: Value::Number(n),
            ..
        } if *actual == field => out.push(*n),
        _ => {}
    }
}

#[test]
fn large_integer_counts_never_round_through_floating_point() {
    for amount in [9007199254740993u64, u64::MAX - 1, u64::MAX] {
        let translated = translate_with_mode(
            &format!("folders at least {amount} files"),
            context(),
            SearchMode::Natural,
        );
        let mut found = vec![];
        numbers(
            translated.query.unwrap().expression(),
            Field::Items,
            &mut found,
        );
        assert_eq!(found, vec![amount]);
    }
    for input in [
        "folders at least 18446744073709551616 files",
        "folders at least half files",
        "folders at least one point five children",
    ] {
        let translated = translate_with_mode(input, context(), SearchMode::Natural);
        assert!(
            translated.query.is_none() && translated.dsl.is_none(),
            "{input}"
        );
    }
}

#[test]
fn integer_byte_thresholds_preserve_exact_displayed_dsl_values() {
    for amount in [9007199254740993u64, u64::MAX - 1, u64::MAX] {
        for comparator in ["exactly", "at most", "at least"] {
            let input = format!("files {comparator} {amount} bytes");
            let translated = translate_with_mode(&input, context(), SearchMode::Natural);
            let query = translated.query.unwrap();
            let mut found = vec![];
            numbers(query.expression(), Field::Size, &mut found);
            assert_eq!(found, vec![amount], "{input}");
            let reparsed = kosha_query::Query::parse(translated.dsl.as_deref().unwrap()).unwrap();
            assert_eq!(query.expression(), reparsed.expression());
        }
    }
}

#[test]
fn exact_large_byte_thresholds_select_the_correct_adjacent_files() {
    use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree};
    use std::{
        ffi::OsStr,
        sync::{Arc, atomic::AtomicBool},
    };
    let mut names = NameArena::default();
    let sizes = [u64::MAX, 9007199254740992, 9007199254740993, u64::MAX];
    let nodes = ["root", "lower.bin", "exact.bin", "maximum.bin"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let (name_off, name_len) = names.push(OsStr::new(name));
            Node {
                allocated: sizes[i],
                logical: sizes[i],
                parent: 0,
                first_child: if i == 0 { 1 } else { 0 },
                child_count: if i == 0 { 3 } else { 0 },
                subtree_files: 0,
                name_off,
                name_len,
                mtime: 0,
                depth: u8::from(i != 0),
                flags: if i == 0 { Flags::DIR } else { Flags::default() },
            }
        })
        .collect();
    let tree = Arc::new(SizeTree::from_parts(
        nodes,
        names,
        Owners::default(),
        "C:/precision".into(),
        ScanStats::default(),
    ));
    let index = kosha_search::SearchIndex::new(tree.clone());
    for (input, expected) in [
        ("files exactly 9007199254740993 bytes", vec!["exact.bin"]),
        (
            "files at most 9007199254740993 bytes",
            vec!["exact.bin", "lower.bin"],
        ),
        (
            "files at least 9007199254740993 bytes",
            vec!["exact.bin", "maximum.bin"],
        ),
        (
            "files exactly 18446744073709551615 bytes",
            vec!["maximum.bin"],
        ),
    ] {
        let translated = translate_with_mode(input, context(), SearchMode::Natural);
        let query = translated.query.unwrap();
        let result = index
            .run(&query, context().now, 10, &AtomicBool::new(false))
            .unwrap();
        let mut names: Vec<_> = result
            .matches
            .iter()
            .map(|id| tree.name_lossy(*id).into_owned())
            .collect();
        names.sort();
        assert_eq!(names, expected, "{input}");
        let reparsed = kosha_query::Query::parse(translated.dsl.as_deref().unwrap()).unwrap();
        assert_eq!(query.expression(), reparsed.expression());
    }
}

#[test]
fn calendar_relative_days_resolve_each_local_midnight_across_dst() {
    // Two days before local Nov 3 is Nov 1: a 25-hour day in New York.
    let seen = std::cell::RefCell::new(vec![]);
    let result = translate_with_resolver(
        "files modified two days ago",
        SearchContext::new(1793725200, [2026, 11, 3]),
        SearchMode::Natural,
        |date| {
            seen.borrow_mut().push(date);
            match date {
                [2026, 11, 1] => Some(1793505600),
                [2026, 11, 2] => Some(1793595600),
                _ => None,
            }
        },
    );
    assert_eq!(*seen.borrow(), vec![[2026, 11, 1], [2026, 11, 2]]);
    assert_eq!(
        result.dsl.as_deref(),
        Some("file and modified >= 2026-11-01 and modified < 2026-11-02")
    );
    let mut found = vec![];
    numbers(
        result.query.unwrap().expression(),
        Field::Modified,
        &mut found,
    );
    assert_eq!(found, vec![1793505600, 1793595600]);
}

#[test]
fn weekday_and_leap_calendar_boundaries_are_explicit_and_strict() {
    for (text, date, expected) in [
        (
            "files created last Tuesday",
            [2026, 11, 17],
            "file and created >= 2026-11-10 and created < 2026-11-11",
        ),
        (
            "files created one day ago",
            [2024, 3, 1],
            "file and created >= 2024-02-29 and created < 2024-03-01",
        ),
        (
            "files created one day ago",
            [2025, 3, 1],
            "file and created >= 2025-02-28 and created < 2025-03-01",
        ),
        (
            "files created last Friday",
            [2027, 1, 1],
            "file and created >= 2026-12-25 and created < 2026-12-26",
        ),
    ] {
        let result = translate_with_mode(
            text,
            SearchContext::new(1795003200, date),
            SearchMode::Natural,
        );
        assert_eq!(result.dsl.as_deref(), Some(expected));
    }
    for (text, date) in [
        ("files created one day ago", [1970, 1, 1]),
        ("files created in 9999", [2026, 11, 18]),
        ("files created last Tuesday", [i32::MIN, 1, 1]),
    ] {
        let result = translate_with_mode(
            text,
            SearchContext::new(1795003200, date),
            SearchMode::Natural,
        );
        assert!(result.query.is_none() && result.dsl.is_none());
    }
    assert!(
        translate_with_resolver(
            "files modified on November 5, 2026",
            context(),
            SearchMode::Natural,
            |_| None
        )
        .query
        .is_none()
    );
}

#[test]
fn partial_spoken_quantities_and_named_dates_never_panic() {
    for input in [
        "files larger than one hundred and five megabytes",
        "files created on November 5, 2026",
        "files modified two days ago",
        "files named \"résumé\" at most half a GB",
        "files named \"résumé\" apparent size at least half a GB",
        "files size on disk between one and two MB",
    ] {
        for (index, _) in input
            .char_indices()
            .chain(std::iter::once((input.len(), '\0')))
        {
            let _ = translate_with_mode(&input[..index], context(), SearchMode::Natural);
            let _ = translate_with_mode(&input[index..], context(), SearchMode::Natural);
        }
    }
}
