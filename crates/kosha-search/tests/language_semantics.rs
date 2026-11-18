// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Independent examples: expected identities are authored from metadata, not
//! regenerated from translator output. Also exercise editable DSL equivalence.

use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree};
use kosha_query::Query;
use kosha_search::{SearchContext, SearchIndex, SearchMode, translate_with_mode};
use std::{
    ffi::OsStr,
    sync::{Arc, atomic::AtomicBool},
};

const NOW: u32 = 1_795_003_200;
const MIB: u64 = 1_048_576;

fn fixture() -> Arc<SizeTree> {
    let old = Some(u64::from(NOW) - 86_401);
    let rows = [
        (
            "report.pdf",
            12 * MIB,
            12 * MIB,
            Flags::default(),
            Some(u64::from(NOW) - 3600),
        ),
        (
            "notes.md",
            5 * MIB,
            5 * MIB,
            Flags::default(),
            Some(u64::from(NOW) - 86400),
        ),
        (
            "photo.JPG",
            4 * MIB,
            4 * MIB,
            Flags::default(),
            Some(u64::from(NOW)),
        ),
        (
            "backup.tar.gz",
            3 * MIB,
            3 * MIB,
            Flags::default(),
            Some(u64::from(NOW) - 2 * 86400 + 3600),
        ),
        ("empty.txt", 0, 0, Flags::default(), old),
        // A nonempty resident NTFS file can consume zero allocated bytes.
        ("resident.txt", 23, 0, Flags::default(), old),
        ("unknown.md", MIB, MIB, Flags::default(), None),
        (
            "future.md",
            MIB,
            MIB,
            Flags::default(),
            Some(u64::from(NOW) + 1),
        ),
        ("Éclair.TXT", 16, 4096, Flags::default(), old),
        ("budget.xlsx", 2 * MIB, 2 * MIB, Flags::default(), old),
        ("draft.docx", 2 * MIB, 2 * MIB, Flags::default(), old),
        ("empty-folder", 0, 0, Flags::DIR, old),
    ];
    let mut names = NameArena::default();
    let (name_off, name_len) = names.push(OsStr::new("fixture"));
    let mut nodes = vec![Node {
        logical: 30 * MIB,
        allocated: 30 * MIB,
        parent: 0,
        first_child: 1,
        child_count: rows.len() as u32,
        subtree_files: 11,
        name_off,
        name_len,
        mtime: NOW,
        flags: Flags::DIR,
        depth: 0,
    }];
    let mut created = vec![u64::MAX];
    for (name, logical, allocated, flags, birth) in rows {
        let (name_off, name_len) = names.push(OsStr::new(name));
        nodes.push(Node {
            logical,
            allocated,
            parent: 0,
            first_child: 0,
            child_count: 0,
            subtree_files: u32::from(!flags.contains(Flags::DIR)),
            name_off,
            name_len,
            mtime: birth.map_or(0, |time| time as u32),
            flags,
            depth: 1,
        });
        created.push(birth.unwrap_or(u64::MAX));
    }
    Arc::new(
        SizeTree::from_parts(
            nodes,
            names,
            Owners::default(),
            "C:/fixture".into(),
            ScanStats::default(),
        )
        .with_creation_times(created),
    )
}

fn check(input: &str, expected: &[&str]) {
    let tree = fixture();
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
    let query = translated.query.as_ref().unwrap();
    let reparsed = Query::parse(translated.dsl.as_deref().unwrap()).expect("editable DSL parses");
    let index = SearchIndex::new(tree.clone());
    let found = index.run(query, NOW, 100, &AtomicBool::new(false)).unwrap();
    let mut actual: Vec<_> = found
        .matches
        .iter()
        .map(|&id| tree.name_lossy(id).into_owned())
        .collect();
    actual.sort();
    let mut expected: Vec<_> = expected.iter().map(|name| name.to_string()).collect();
    expected.sort();
    assert_eq!(actual, expected, "{input} => {:?}", translated.dsl);
    for i in 1..tree.len() {
        let id = kosha_core::NodeId::from_index(i);
        assert_eq!(
            query.matches(&tree, id, NOW),
            reparsed.matches(&tree, id, NOW),
            "{input}: {}",
            tree.name_lossy(id)
        );
    }
}

#[test]
fn custom_recipes_execute_exact_dsl_and_relative_ages_use_the_search_clock() {
    let book = kosha_search::RecipeBook::from_json(
        r#"[
        {"phrase":"review docs","dsl":"file and ext in (pdf,xlsx)"},
        {"phrase":"yesterday's notes","dsl":"file and ext = md and age > 1d"}
    ]"#,
    )
    .unwrap();
    let tree = fixture();
    let index = SearchIndex::new(tree.clone());
    for (phrase, now, expected) in [
        (
            "REVIEW \u{20}\u{20}docs",
            NOW,
            vec!["budget.xlsx", "report.pdf"],
        ),
        ("yesterday's notes", NOW, vec![]),
        ("yesterday's notes", NOW + 86400, vec!["notes.md"]),
    ] {
        let translated = book.translate_with_resolver(
            phrase,
            SearchContext::new(now, [2026, 11, 18]),
            SearchMode::Natural,
            |_| None,
        );
        let result = index
            .run(
                translated.query.as_ref().unwrap(),
                now,
                100,
                &AtomicBool::new(false),
            )
            .unwrap();
        let mut names: Vec<_> = result
            .matches
            .iter()
            .map(|&id| tree.name_lossy(id).into_owned())
            .collect();
        names.sort();
        assert_eq!(names, expected, "{phrase}, clock={now}");
        let edited = Query::parse(translated.dsl.as_deref().unwrap()).unwrap();
        assert_eq!(edited.run(&tree, now, 100).matches, result.matches);
    }
}

#[test]
fn manual_recipe_example_loads_common_and_current_platform_recipes() {
    let manual = include_str!("../../../docs/manual/search-recipes.md");
    let json = manual
        .split("```json\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let book = kosha_search::RecipeBook::from_json(json).unwrap();
    let mut expected = vec!["my build leftovers", "large invoices", "old downloads"];
    if cfg!(windows) {
        expected.push("nuget global packages");
    }
    let actual: Vec<_> = book
        .recipes()
        .iter()
        .map(|recipe| recipe.phrase.as_str())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn independently_expected_exclusions_unions_and_names() {
    for (input, expected) in [
        ("empty files", vec!["empty.txt"]),
        ("empty folders", vec!["empty-folder"]),
        ("files starting with re", vec!["report.pdf", "resident.txt"]),
        (
            "files with names ending with '.tar.gz'",
            vec!["backup.tar.gz"],
        ),
        ("pdf or xlsx files", vec!["report.pdf", "budget.xlsx"]),
        (
            "documents except pdf",
            vec![
                "notes.md",
                "empty.txt",
                "resident.txt",
                "unknown.md",
                "future.md",
                "Éclair.TXT",
                "draft.docx",
            ],
        ),
    ] {
        check(input, &expected);
    }
}

#[test]
fn spoken_quantities_find_entries_by_their_actual_byte_sizes() {
    for (input, expected) in [
        ("files greater than ten megabytes", vec!["report.pdf"]),
        (
            "files at least one point five megabytes",
            vec![
                "report.pdf",
                "notes.md",
                "photo.JPG",
                "backup.tar.gz",
                "budget.xlsx",
                "draft.docx",
            ],
        ),
        (
            "files smaller than half a megabyte",
            vec!["empty.txt", "resident.txt", "Éclair.TXT"],
        ),
        (
            "files exactly one megabyte",
            vec!["unknown.md", "future.md"],
        ),
    ] {
        check(input, &expected);
    }
}

#[test]
fn named_calendar_phrases_find_the_expected_recorded_days() {
    // This headless corpus explicitly uses UTC, independently of the host's
    // timezone. Calendar DSL remains a local-date expression when edited in a
    // GUI; therefore only parsing, not host-local re-evaluation, is checked here.
    let tree = fixture();
    let tuesday = vec![
        "notes.md",
        "empty.txt",
        "resident.txt",
        "Éclair.TXT",
        "budget.xlsx",
        "draft.docx",
    ];
    for (phrase, expected) in [
        ("two days ago", vec!["backup.tar.gz"]),
        ("last Tuesday", tuesday.clone()),
        ("on November 17, 2026", tuesday.clone()),
        ("on 17th November 2026", tuesday),
        ("in October 2026", vec![]),
        ("in 2025", vec![]),
        (
            "in November 2026",
            vec![
                "report.pdf",
                "notes.md",
                "photo.JPG",
                "backup.tar.gz",
                "empty.txt",
                "resident.txt",
                "future.md",
                "Éclair.TXT",
                "budget.xlsx",
                "draft.docx",
            ],
        ),
    ] {
        for field in ["created", "modified"] {
            let input = format!("files {field} {phrase}");
            let result = translate_with_mode(
                &input,
                SearchContext::new(NOW, [2026, 11, 18]),
                SearchMode::Natural,
            );
            assert!(result.is_valid(), "{input}: {:?}", result.diagnostics);
            Query::parse(result.dsl.as_deref().unwrap()).expect("calendar DSL is editable");
            let query = result.query.unwrap();
            let mut actual: Vec<_> = (1..tree.len())
                .map(kosha_core::NodeId::from_index)
                .filter(|&id| query.matches(&tree, id, NOW))
                .map(|id| tree.name_lossy(id).into_owned())
                .collect();
            actual.sort();
            let mut expected: Vec<_> = expected.iter().map(|name| name.to_string()).collect();
            expected.sort();
            assert_eq!(actual, expected, "{input}");
        }
    }
}

#[test]
fn rolling_ranges_have_exact_inclusive_bounds_and_exclude_future_and_unknown() {
    for field in ["created", "modified"] {
        check(
            &format!("files {field} in the last 24 hours"),
            &["report.pdf", "notes.md", "photo.JPG"],
        );
        check(
            &format!("files not {field} in the last 24 hours"),
            &[
                "backup.tar.gz",
                "empty.txt",
                "resident.txt",
                "future.md",
                "Éclair.TXT",
                "budget.xlsx",
                "draft.docx",
            ],
        );
    }
}

#[test]
fn partially_typed_unicode_and_structural_phrases_never_panic() {
    let context = SearchContext::new(NOW, [2026, 11, 18]);
    for text in [
        "documents except pdf",
        "files with names ending with '.tar.gz'",
        "files named 'Éclair and résumé.txt'",
        "files created in the last 24 hours",
        "files not modified yesterday",
        "pdf or xlsx files over 10 MB",
    ] {
        for end in text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
        {
            for suffix in ["", "'", " and", " except", " 😀"] {
                let input = format!("{}{suffix}", &text[..end]);
                let result = translate_with_mode(&input, context, SearchMode::Natural);
                if !result.diagnostics.is_empty() {
                    assert!(
                        result.query.is_none() && result.dsl.is_none(),
                        "unresolved input executes: {input}"
                    );
                }
            }
        }
    }
}
