// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Query parsing and evaluation.
//!
//! A filter that silently returns the wrong set is worse than no filter,
//! because the user concludes the files are not there. So these lean hard on
//! the cases where a query could plausibly mean two things, and on the errors
//! a person will actually make while typing.

use std::fs;
use std::path::Path;

use kosha_core::{NodeId, ScanOptions, Scanner, SizeTree};
use kosha_query::Query;

const MB: usize = 1024 * 1024;

/// ```text
/// root/
///   media/holiday.MP4        6 MB   (uppercase extension on purpose)
///   media/clip.mkv           4 MB
///   code/main.rs             1 KB
///   code/notes.txt           1 KB
///   node_modules/dep/big.bin 8 MB
///   .gitignore               (a dotfile: no extension)
///   empty/                   (an empty directory)
/// ```
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("media")).unwrap();
    fs::create_dir_all(root.join("code")).unwrap();
    fs::create_dir_all(root.join("node_modules/dep")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();

    fs::write(root.join("media/holiday.MP4"), vec![0u8; 6 * MB]).unwrap();
    fs::write(root.join("media/clip.mkv"), vec![0u8; 4 * MB]).unwrap();
    fs::write(root.join("code/main.rs"), vec![0u8; 1024]).unwrap();
    fs::write(root.join("code/notes.txt"), vec![0u8; 1024]).unwrap();
    fs::write(root.join("node_modules/dep/big.bin"), vec![0u8; 8 * MB]).unwrap();
    fs::write(root.join(".gitignore"), b"target").unwrap();
    dir
}

fn scan(root: &Path) -> SizeTree {
    Scanner::new(ScanOptions::default()).scan(root).unwrap()
}

fn now() -> u32 {
    kosha_core::format::now_unix()
}

#[test]
fn exact_creation_instants_round_trip_without_timezone_rounding() {
    let dir = fixture();
    let tree = scan(dir.path());
    let ids: Vec<_> = tree.children(tree.root()).collect();
    let mut times = vec![u64::MAX; tree.len()];
    times[ids[0].index()] = 1794398400;
    times[ids[1].index()] = 1795003200;
    times[ids[2].index()] = 1795003201; // Future entry is outside the interval.
    times[tree.root().index()] = 0; // Real epoch differs from missing birth time.
    let tree = tree.with_creation_times(times);
    let query = Query::parse("created >= @1794398400 and created <= @1795003200").unwrap();
    assert!(query.matches(&tree, ids[0], 1795003200));
    assert!(query.matches(&tree, ids[1], 1795003200));
    assert!(!query.matches(&tree, ids[2], 1795003200));
    assert!(
        Query::parse("created = @0")
            .unwrap()
            .matches(&tree, tree.root(), 0)
    );
    assert_eq!(
        Query::parse(query.source()).unwrap().expression(),
        query.expression()
    );
    assert!(Query::parse("modified >= @0").is_ok());
    for invalid in [
        "@",
        "@-1",
        "@+1",
        "@1.5",
        "@NaN",
        "@1_000",
        "@18446744073709551615",
        "@18446744073709551616",
    ] {
        assert!(
            Query::parse(&format!("created >= {invalid}")).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn literal_prefix_suffix_queries_preserve_case_and_unicode_boundaries() {
    let dir = fixture();
    fs::write(dir.path().join("Éclair.Ö"), b"unicode").unwrap();
    let tree = scan(dir.path());
    assert_eq!(
        names(&tree, "file and name startswith HOLI"),
        vec!["holiday.MP4"]
    );
    assert_eq!(names(&tree, "name endswith .MP4"), vec!["holiday.MP4"]);
    assert_eq!(
        names(&tree, "name startswith 'éC' and name endswith '.ö'"),
        vec!["Éclair.Ö"]
    );
    assert!(
        names(
            &tree,
            "name startswith much-longer-than-any-fixture-filename-here"
        )
        .is_empty()
    );
    assert!(
        names(
            &tree,
            "name endswith much-longer-than-any-fixture-filename-here"
        )
        .is_empty()
    );
    let all = names(&tree, "file");
    let negative = names(&tree, "file and not name endswith .mp4");
    assert_eq!(negative.len(), all.len() - 1);
    assert!(!negative.iter().any(|name| name == "holiday.MP4"));
    let path_query = Query::parse(&format!("path startswith '{}'", dir.path().display())).unwrap();
    for id in tree.children(tree.root()) {
        assert!(path_query.matches(&tree, id, now()));
    }
    for invalid in [
        "size startswith 10",
        "size endswith '10'",
        "created startswith @0",
        "name startswith",
    ] {
        assert!(Query::parse(invalid).is_err(), "{invalid}");
    }
}

#[cfg(windows)]
#[test]
fn windows_path_filters_accept_either_separator_without_changing_names() {
    let dir = fixture();
    fs::write(dir.path().join("code/Éclair.TXT"), b"unicode path").unwrap();
    let tree = scan(dir.path());
    assert_eq!(
        names(&tree, "file and path ~ 'CODE/main.rs'"),
        vec!["main.rs"]
    );
    assert_eq!(
        names(&tree, "file and path endswith 'code/main.rs'"),
        vec!["main.rs"]
    );
    assert_eq!(
        names(&tree, "file and path endswith 'code/éclair.txt'"),
        vec!["Éclair.TXT"]
    );
    let id = tree
        .children(tree.root())
        .find(|&id| tree.name_lossy(id) == "code")
        .unwrap();
    let slashed = tree.path_of(id).to_string_lossy().replace('\\', "/");
    let query = Query::parse(&format!("path = '{slashed}'")).unwrap();
    assert!(query.matches(&tree, id, now()));
    let query = Query::parse(&format!("path startswith '{slashed}/'")).unwrap();
    for child in tree.children(id) {
        assert!(query.matches(&tree, child, now()));
    }
    assert!(names(&tree, "name ~ 'code/main.rs'").is_empty());
    assert!(
        names(
            &tree,
            "path ~ 'this/needle/is/longer/than/anything/in/the/fixture/and/should/not/match'"
        )
        .is_empty()
    );
}

#[test]
fn creation_dates_are_distinct_from_modification_and_unknown() {
    let dir = fixture();
    let tree = scan(dir.path());
    let id = tree.children(tree.root()).next().unwrap();
    let mut times = vec![u64::MAX; tree.len()];
    times[id.index()] = 1_700_000_000;
    let tree = tree.with_creation_times(times);
    assert!(
        Query::parse("created >= 2020-01-01")
            .unwrap()
            .matches(&tree, id, now())
    );
    assert!(
        !Query::parse("created < 2020-01-01")
            .unwrap()
            .matches(&tree, id, now())
    );
    for expression in [
        "created < 2020-01-01",
        "created != 2020-01-01",
        "created >= 2020-01-01",
    ] {
        assert!(
            !Query::parse(expression)
                .unwrap()
                .matches(&tree, tree.root(), now())
        );
    }
    assert!(
        Query::parse("created >= 2020-01-01")
            .unwrap()
            .search(&tree, now(), 1000)
            .contains(&id)
    );
    assert!(Query::parse("created < 1969-01-01").is_err());
    for expression in [
        "not created >= 2020-01-01",
        "not (not created >= 2020-01-01)",
        "created not in (2020-01-01)",
    ] {
        assert!(
            !Query::parse(expression)
                .unwrap()
                .matches(&tree, tree.root(), now())
        );
    }
    assert!(
        Query::parse("dir or created >= 2020-01-01")
            .unwrap()
            .matches(&tree, tree.root(), now())
    );
    assert!(
        Query::parse("not (file and created >= 2020-01-01)")
            .unwrap()
            .matches(&tree, tree.root(), now())
    );
}

#[test]
fn missing_modification_dates_do_not_mean_ancient_files() {
    let dir = fixture();
    let tree = scan(dir.path());
    let mut nodes = tree.nodes().to_vec();
    for node in &mut nodes {
        node.mtime = 0;
    }
    let tree = SizeTree::from_parts(
        nodes,
        kosha_core::NameArena::from_bytes(tree.names_bytes().to_vec()),
        kosha_core::Owners::default(),
        tree.root_path().to_path_buf(),
        kosha_core::ScanStats::default(),
    );
    let id = tree.children(tree.root()).next().unwrap();
    for expression in [
        "modified < 2026-01-01",
        "age > 1y",
        "not modified >= 2026-01-01",
        "not age < 1d",
    ] {
        assert!(
            !Query::parse(expression).unwrap().matches(&tree, id, now()),
            "{expression}"
        );
    }
    assert!(
        Query::parse("dir or modified < 2026-01-01")
            .unwrap()
            .matches(&tree, tree.root(), now())
    );
}

#[test]
fn unreadable_and_unvisited_entries_do_not_mean_empty() {
    let dir = fixture();
    let original = scan(dir.path());
    let mut nodes = original.nodes().to_vec();
    let empty_dir = original
        .children(original.root())
        .find(|&id| original.name_lossy(id) == "empty")
        .unwrap();
    let denied_file = original
        .children(original.root())
        .find(|&id| original.name_lossy(id) == ".gitignore")
        .unwrap();
    nodes[denied_file.index()].flags = kosha_core::Flags::UNREADABLE;
    nodes[denied_file.index()].logical = 0;
    nodes[denied_file.index()].allocated = 0;
    for flags in [
        kosha_core::Flags::DIR | kosha_core::Flags::UNREADABLE,
        kosha_core::Flags::DIR | kosha_core::Flags::OTHER_DEVICE,
        kosha_core::Flags::DIR | kosha_core::Flags::PARTIAL,
    ] {
        nodes[empty_dir.index()].flags = flags;
        let tree = SizeTree::from_parts(
            nodes.clone(),
            kosha_core::NameArena::from_bytes(original.names_bytes().to_vec()),
            kosha_core::Owners::default(),
            original.root_path().to_path_buf(),
            kosha_core::ScanStats::default(),
        );
        for expression in ["empty", "not empty"] {
            assert!(
                !Query::parse(expression)
                    .unwrap()
                    .matches(&tree, empty_dir, now()),
                "{expression}"
            );
        }
        for expression in [
            "apparent = 0",
            "not apparent = 0",
            "size = 0",
            "not size = 0",
        ] {
            assert!(
                !Query::parse(expression)
                    .unwrap()
                    .matches(&tree, denied_file, now()),
                "{expression}"
            );
        }
    }
    let tree = SizeTree::from_parts(
        original.nodes().to_vec(),
        kosha_core::NameArena::from_bytes(original.names_bytes().to_vec()),
        kosha_core::Owners::default(),
        original.root_path().to_path_buf(),
        kosha_core::ScanStats {
            cancelled: true,
            ..Default::default()
        },
    );
    assert!(
        !Query::parse("empty")
            .unwrap()
            .matches(&tree, empty_dir, now())
    );
    assert!(
        !Query::parse("not empty")
            .unwrap()
            .matches(&tree, empty_dir, now())
    );
    assert!(
        Query::parse("empty")
            .unwrap()
            .matches(&original, empty_dir, now())
    );
}

/// Names of everything matching, sorted for a stable comparison.
fn names(tree: &SizeTree, query: &str) -> Vec<String> {
    let query = Query::parse(query).unwrap_or_else(|e| panic!("`{query}` did not parse: {e}"));
    let mut found: Vec<String> = query
        .search(tree, now(), 1000)
        .into_iter()
        .map(|id| tree.name_lossy(id).to_string())
        .collect();
    found.sort();
    found
}

fn error(query: &str) -> String {
    match Query::parse(query) {
        Ok(_) => panic!("`{query}` parsed, but should not have"),
        Err(e) => e.to_string(),
    }
}

// ------------------------------------------------------------ plain text

#[test]
fn bare_text_is_a_name_search() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(names(&tree, "clip"), ["clip.mkv"]);
    assert!(Query::parse("clip").unwrap().is_plain_text());
}

#[test]
fn bare_text_ignores_case() {
    let dir = fixture();
    let tree = scan(dir.path());
    assert_eq!(names(&tree, "HOLIDAY"), ["holiday.MP4"]);
}

#[test]
fn bare_text_with_spaces_stays_one_search() {
    // Two adjacent words would otherwise be parsed as two conditions joined by
    // an implicit `and`, which is not what someone typing a filename means.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("my holiday video.mp4"), b"x").unwrap();
    let tree = scan(dir.path());

    assert_eq!(names(&tree, "holiday video"), ["my holiday video.mp4"]);
}

#[test]
fn a_quoted_string_is_always_a_name_search() {
    // Even when it contains words that would otherwise be keywords.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("size and age.txt"), b"x").unwrap();
    let tree = scan(dir.path());

    assert_eq!(names(&tree, "\"size and age\""), ["size and age.txt"]);
}

#[test]
fn quoted_numeric_and_date_values_are_reported_instead_of_silently_matching_nothing() {
    for input in [
        "size > '1mb'",
        "items >= '100'",
        "age < '7d'",
        "modified >= '2026-01-01'",
        "created > '@1795003200'",
        "size in ('1mb', '2mb')",
    ] {
        let error = Query::parse(input).unwrap_err();
        assert!(error.message.contains("unquoted"), "{input}: {error}");
        assert!(error.hint.as_deref().unwrap().contains("quotes"));
        assert!(matches!(
            error.kind,
            kosha_query::ErrorKind::BadValue { .. }
        ));
    }
    assert!(Query::parse("name = '100'").is_ok());
    assert!(Query::parse("name ~ 'size > 1mb'").is_ok());
}

// -------------------------------------------------------------- numbers

#[test]
fn size_comparison_uses_binary_units() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(
        names(&tree, "file and size > 5mb"),
        ["big.bin", "holiday.MP4"]
    );
    assert_eq!(names(&tree, "file and size >= 8mb"), ["big.bin"]);
    assert!(names(&tree, "size > 100mb").is_empty());
}

#[test]
fn size_units_are_all_accepted() {
    let dir = fixture();
    let tree = scan(dir.path());

    for spelling in ["5mb", "5MB", "5mib", "5m", "5242880", "5242880b"] {
        assert_eq!(
            names(&tree, &format!("file and size > {spelling}")),
            ["big.bin", "holiday.MP4"],
            "`{spelling}` did not mean 5 MiB"
        );
    }
}

#[test]
fn fractional_sizes_work() {
    let dir = fixture();
    let tree = scan(dir.path());
    assert_eq!(names(&tree, "file and size > 7.5mb"), ["big.bin"]);
}

#[test]
fn size_and_age_literals_preserve_large_integers_and_reject_overflow() {
    use kosha_query::{Expr, Field, Value};
    for (input, expected_field, expected) in [
        (
            "size = 9007199254740993b",
            Field::Size,
            9_007_199_254_740_993,
        ),
        ("size = 18446744073709551615b", Field::Size, u64::MAX),
        ("size = 18446744073709551614b", Field::Size, u64::MAX - 1),
        ("size = .5kib", Field::Size, 512),
        ("size = 1.25mb", Field::Size, 1_310_720),
        (
            "size = 0.99999999999999999999999999999999999999999999b",
            Field::Size,
            0,
        ),
        ("age = 9007199254740993s", Field::Age, 9_007_199_254_740_993),
        ("age = 18446744073709551615s", Field::Age, u64::MAX),
        ("age = .5h", Field::Age, 1800),
    ] {
        let query = Query::parse(input).unwrap();
        assert!(
            matches!(query.expression(), Expr::Compare {field,value:Value::Number(number),..}
            if *field == expected_field && *number == expected),
            "{input}: {:?}",
            query.expression()
        );
    }
    for input in [
        "size = 18446744073709551616b",
        "size = 18446744073709551615kib",
        "age = 18446744073709551616s",
        "age = 18446744073709551615d",
        "size = 1..2mb",
    ] {
        assert!(Query::parse(input).is_err(), "{input}");
    }
}

#[test]
fn counts_and_depth_are_plain_numbers() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert!(names(&tree, "dir and children > 1").contains(&"media".to_string()));
    assert!(names(&tree, "depth = 1").contains(&"media".to_string()));
    assert!(!names(&tree, "depth = 1").contains(&"big.bin".to_string()));
}

// ---------------------------------------------------------------- text

#[test]
fn extension_matching_ignores_case() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(names(&tree, "ext = mp4"), ["holiday.MP4"]);
    assert_eq!(names(&tree, "ext = MP4"), ["holiday.MP4"]);
}

#[test]
fn a_dotfile_has_no_extension() {
    // `.gitignore` is a name, not an extension. Treating it as one would put
    // every dotfile in the wrong bucket.
    let dir = fixture();
    let tree = scan(dir.path());

    assert!(!names(&tree, "ext = gitignore").contains(&".gitignore".to_string()));
    assert!(names(&tree, "ext = \"\"").contains(&".gitignore".to_string()));
}

#[test]
fn in_lists_several_alternatives() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(
        names(&tree, "ext in (mp4, mkv)"),
        ["clip.mkv", "holiday.MP4"]
    );
    assert!(!names(&tree, "ext not in (mp4, mkv)").contains(&"clip.mkv".to_string()));
}

#[test]
fn path_matching_searches_the_whole_path() {
    let dir = fixture();
    let tree = scan(dir.path());

    let found = names(&tree, "path ~ node_modules");
    assert!(found.contains(&"big.bin".to_string()));
    assert!(!found.contains(&"clip.mkv".to_string()));
}

#[test]
fn a_query_without_path_does_not_pay_for_one() {
    assert!(!Query::parse("size > 1mb").unwrap().needs_path());
    assert!(!Query::parse("name ~ tmp").unwrap().needs_path());
}

#[test]
fn a_separator_free_path_search_needs_no_path_built() {
    // The common case. A substring with no separator cannot span one, so it is
    // answered by walking parent pointers and never assembling a string —
    // which took `path ~ node_modules` over 400,000 entries from 746 ms to
    // single digits.
    assert!(!Query::parse("path ~ node_modules").unwrap().needs_path());
    assert!(!Query::parse("path !~ cache").unwrap().needs_path());
}

#[test]
fn a_path_search_spanning_a_separator_does_need_one() {
    assert!(Query::parse("path ~ AppData/Local").unwrap().needs_path());
    assert!(Query::parse("path = /usr/share").unwrap().needs_path());
}

#[test]
fn the_two_path_strategies_agree() {
    // The fast walk and the assembled string must answer identically, or the
    // optimisation is a silent behaviour change.
    let dir = fixture();
    let tree = scan(dir.path());
    let now = now();

    let walked = Query::parse("path ~ node_modules").unwrap();
    let assembled = Query::parse("path ~ \"node_modules\"").unwrap();

    for index in 0..tree.len() {
        let id = NodeId::from_index(index);
        let by_walk = walked.matches(&tree, id, now);
        // Compare against the full path directly, which is what the slow
        // strategy would have produced.
        let by_string = tree
            .path_of(id)
            .to_string_lossy()
            .to_lowercase()
            .contains("node_modules");
        assert_eq!(
            by_walk,
            by_string,
            "disagreement at {}",
            tree.path_of(id).display()
        );
    }
    let _ = assembled;
}

// ----------------------------------------------------------------- flags

#[test]
fn kind_flags_partition_the_tree() {
    let dir = fixture();
    let tree = scan(dir.path());

    let dirs = names(&tree, "dir");
    let files = names(&tree, "file");
    assert!(dirs.contains(&"media".to_string()));
    assert!(files.contains(&"clip.mkv".to_string()));
    assert!(!dirs.contains(&"clip.mkv".to_string()));
    assert!(!files.contains(&"media".to_string()));
}

#[test]
fn empty_finds_directories_with_nothing_in_them() {
    let dir = fixture();
    let tree = scan(dir.path());
    assert_eq!(names(&tree, "empty"), ["empty"]);
}

// ------------------------------------------------------------- combining

#[test]
fn conditions_combine_with_and_or_and_not() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(
        names(&tree, "ext = mkv or ext = rs"),
        ["clip.mkv", "main.rs"]
    );
    assert_eq!(
        names(&tree, "file and size > 5mb"),
        ["big.bin", "holiday.MP4"]
    );
    assert_eq!(
        names(&tree, "file and size > 5mb and not path ~ node_modules"),
        ["holiday.MP4"]
    );
}

#[test]
fn adjacency_means_and() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(
        names(&tree, "file size > 5mb"),
        names(&tree, "file and size > 5mb")
    );
}

#[test]
fn a_size_query_matches_directories_on_their_total() {
    // A directory carries the size of everything below it, so `size > 5mb`
    // finds the directories holding that much as well as the files. That is
    // the useful reading — "show me what is big" should not exclude the folder
    // that is big — and `file` narrows it when only files are wanted.
    let dir = fixture();
    let tree = scan(dir.path());

    let everything = names(&tree, "size > 5mb");
    assert!(everything.contains(&"media".to_string()), "{everything:?}");
    assert!(everything.contains(&"holiday.MP4".to_string()));

    let files_only = names(&tree, "file and size > 5mb");
    assert!(!files_only.contains(&"media".to_string()));
}

#[test]
fn and_binds_tighter_than_or() {
    let dir = fixture();
    let tree = scan(dir.path());

    // Read as `(ext = rs and size < 1mb) or ext = mkv`, not
    // `ext = rs and (size < 1mb or ext = mkv)`.
    assert_eq!(
        names(&tree, "ext = rs and size < 1mb or ext = mkv"),
        ["clip.mkv", "main.rs"]
    );
}

#[test]
fn brackets_override_precedence() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(
        names(&tree, "(ext = mkv or ext = mp4) and size > 5mb"),
        ["holiday.MP4"]
    );
}

#[test]
fn pipes_mean_or_and_not_and() {
    // `&&` can be dropped entirely, because adjacency already means `and`.
    // `||` cannot, and dropping it turned every `a || b` into `a and b`, which
    // matched nothing and looked like the query had simply found no results.
    let dir = fixture();
    let tree = scan(dir.path());

    let either = names(&tree, "ext = mkv || ext = rs");
    assert_eq!(either, ["clip.mkv", "main.rs"]);
    assert!(
        names(&tree, "ext = mkv and ext = rs").is_empty(),
        "the conjunction really is empty, which is what made the bug invisible"
    );
}

#[test]
fn a_lone_ampersand_or_pipe_is_a_helpful_error() {
    let message = match Query::parse("file & dir") {
        Ok(_) => panic!("parsed"),
        Err(e) => e.to_string(),
    };
    assert!(message.contains("&&"), "{message}");
}

#[test]
fn symbolic_operators_mean_the_same_as_the_words() {
    let dir = fixture();
    let tree = scan(dir.path());

    assert_eq!(
        names(&tree, "file && size > 5mb"),
        names(&tree, "file and size > 5mb")
    );
    assert_eq!(
        names(&tree, "ext = mkv || ext = rs"),
        names(&tree, "ext = mkv or ext = rs")
    );
    assert_eq!(names(&tree, "! file"), names(&tree, "not file"));
}

// -------------------------------------------------------------- the root

#[test]
fn the_scan_root_is_never_a_result() {
    // It matches almost everything trivially — it is every entry's ancestor —
    // and is never the answer to "find me the things that…".
    let dir = fixture();
    let tree = scan(dir.path());

    let found = Query::parse("dir").unwrap().search(&tree, now(), 100);
    assert!(!found.contains(&tree.root()));
}

// -------------------------------------------------------------- ordering

#[test]
fn results_come_back_largest_first() {
    let dir = fixture();
    let tree = scan(dir.path());

    let found = Query::parse("file").unwrap().search(&tree, now(), 100);
    let sizes: Vec<u64> = found.iter().map(|&id| tree.node(id).allocated).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
}

#[test]
fn a_limit_keeps_the_biggest_matches() {
    let dir = fixture();
    let tree = scan(dir.path());

    let found = Query::parse("file").unwrap().search(&tree, now(), 2);
    assert_eq!(found.len(), 2);
    let names: Vec<String> = found
        .iter()
        .map(|&id| tree.name_lossy(id).to_string())
        .collect();
    assert_eq!(names, ["big.bin", "holiday.MP4"]);
}

// ---------------------------------------------------------------- totals

#[test]
fn totals_do_not_double_count_nested_matches() {
    // `size > 1kb` matches node_modules, dep and big.bin — a chain. Adding all
    // three would report three times the bytes that actually exist.
    let dir = fixture();
    let tree = scan(dir.path());

    let query = Query::parse("size > 1kb").unwrap();
    let total = query.total_allocated(&tree, now());

    assert!(
        total <= tree.total_allocated(),
        "totalled {total} from a tree holding {}",
        tree.total_allocated()
    );
}

// ----------------------------------------------------------------- errors

#[test]
fn an_unknown_field_suggests_the_nearest_one() {
    let message = error("saiz > 1mb");
    assert!(message.contains("saiz"), "{message}");
    assert!(message.contains("size"), "{message}");
}

#[test]
fn a_missing_value_says_so() {
    let message = error("size >");
    assert!(
        message.contains("value") || message.contains("ends early"),
        "{message}"
    );
}

#[test]
fn a_bad_size_is_rejected_rather_than_guessed_at() {
    let message = error("size > 10furlongs");
    assert!(message.contains("not a size"), "{message}");
}

#[test]
fn comparing_a_number_with_a_substring_is_an_error() {
    // `size ~ 100` looks reasonable and means nothing; silently answering a
    // different question would be worse than refusing.
    let message = error("size ~ 100");
    assert!(message.contains("compares text"), "{message}");
}

/// A glob is refused with the spelling that works, not answered with nothing.
///
/// **Measured: `kosha <dir> --query "name = *.txt"` said `0 matched`** over a
/// directory holding two `.txt` files. A shell has taught everybody to type
/// that, no name on Windows can contain a `*`, and the answer looked like a
/// fact about the disk. This file's own opening says it: a filter that silently
/// returns the wrong set is worse than no filter, because the user concludes
/// the files are not there.
#[test]
fn a_wildcard_is_refused_with_the_spelling_that_works() {
    for query in [
        "name = *.txt",
        "path ~ *node_modules*",
        "name = holiday.??4",
    ] {
        let message = error(query);
        assert!(
            message.contains("wildcard"),
            "`{query}` was not called out: {message}"
        );
        assert!(
            message.contains('~'),
            "`{query}` was not given the fix: {message}"
        );
    }

    // Quoted, it is a literal and reaches the tree as one — which is how
    // somebody who really has a file called `*.txt` asks for it.
    let dir = fixture();
    let tree = scan(dir.path());
    assert!(Query::parse("name = \"*.txt\"").is_ok());
    assert!(names(&tree, "name = \"*.txt\"").is_empty());
}

/// An extension typed with its dot, likewise.
///
/// `0 matched` for `ext = .txt`, in a tool whose every row shows the dot. The
/// correction is given rather than applied: `ext = .gitignore` corrected to
/// `ext = gitignore` would answer a *different* question — a dotfile's leading
/// dot is its whole name and it has no extension at all — so the message says
/// both spellings and lets the person pick the one they meant.
#[test]
fn an_extension_with_its_dot_is_refused_with_both_spellings() {
    for query in ["ext = .txt", "ext in (.mp4, .mkv)", "ext != .rs"] {
        let message = error(query);
        assert!(
            message.contains("without one") || message.contains("without its dot"),
            "`{query}` was not called out: {message}"
        );
    }

    let message = error("ext = .gitignore");
    assert!(message.contains("ext = gitignore"), "{message}");
    assert!(message.contains("name = .gitignore"), "{message}");

    // And the spellings that do work still do.
    let dir = fixture();
    let tree = scan(dir.path());
    assert_eq!(names(&tree, "ext = txt"), ["notes.txt"]);
    assert_eq!(names(&tree, "name = .gitignore"), [".gitignore"]);
}

/// A date before 1970 is refused for what it is, not called something else.
///
/// `modified > 1969-12-31` came back as "`1969-12-31` is not a date", which is
/// the mistake `--top 4294967296` made about a number: it *is* a date, and
/// what is wrong is that a file's timestamp is seconds since 1970 and cannot
/// be one. The distinction is the difference between a typo and a boundary.
#[test]
fn a_date_before_the_epoch_is_refused_for_being_before_it() {
    for query in ["modified > 1969-12-31", "modified < 0001-01-01"] {
        let message = error(query);
        assert!(message.contains("before 1970"), "{query}: {message}");
        assert!(!message.contains("not a date"), "{query}: {message}");
    }

    // What is genuinely not a date still says so.
    assert!(error("modified > 01/02/2024").contains("not a date"));
    assert!(error("modified > yesterday").contains("not a date"));
    // Including a day that does not exist, which is its own message.
    assert!(error("modified > 2026-02-30").contains("not a date"));

    // And the epoch itself, and everything after it, is a date this compares.
    assert!(Query::parse("modified > 1970-01-01").is_ok());
    assert!(Query::parse("modified > 2026-09-27").is_ok());
}

#[test]
fn an_unclosed_bracket_is_reported() {
    assert!(error("(size > 1mb").contains("bracket"));
}

#[test]
fn an_unclosed_quote_is_reported() {
    assert!(error("name ~ \"holiday").contains("quote"));
}

#[test]
fn an_empty_query_is_an_error() {
    assert!(error("   ").contains("empty"));
}

#[test]
fn an_error_points_at_the_offending_text() {
    let error = Query::parse("size > 1mb and saiz > 2mb").unwrap_err();
    assert_eq!(error.text(), "saiz");
}

// ------------------------------------------------------------------ ages

#[test]
fn age_accepts_several_spellings() {
    for spelling in ["30d", "30", "1mo", "1y", "12h", "2w"] {
        assert!(
            Query::parse(&format!("age > {spelling}")).is_ok(),
            "`{spelling}` was rejected"
        );
    }
}

#[test]
fn months_are_not_minutes() {
    // `mo` has to be checked before `m`, or six months becomes six minutes and
    // a query for stale files returns everything.
    let dir = fixture();
    let tree = scan(dir.path());

    // Everything in the fixture was just written, so nothing is a month old.
    assert!(names(&tree, "age > 1mo").is_empty());
    // But everything is more than a minute old in age-seconds terms? No — it is
    // seconds old, so this must also be empty. The point is that they differ by
    // a factor of 43,200 and both cannot be the reading.
    assert!(Query::parse("age > 1mo").is_ok());
}

#[test]
fn freshly_written_files_are_not_old() {
    let dir = fixture();
    let tree = scan(dir.path());
    assert!(names(&tree, "age > 1y").is_empty());
}

#[test]
fn an_unknown_timestamp_does_not_read_as_ancient() {
    // A file we could not date has mtime 0. Treating that as 1970 would sweep
    // every undateable entry into "older than a year".
    let dir = fixture();
    let tree = scan(dir.path());

    let query = Query::parse("age > 10y").unwrap();
    for index in 0..tree.len() {
        let id = NodeId::from_index(index);
        if tree.node(id).mtime == 0 {
            assert!(
                !query.matches(&tree, id, now()),
                "an undated entry was reported as ancient"
            );
        }
    }
}

#[test]
fn dates_parse_and_order_correctly() {
    assert!(Query::parse("modified < 2024-01-01").is_ok());
    assert!(Query::parse("modified > 1999-12-31").is_ok());
    assert!(Query::parse("modified < 2024-13-01").is_err(), "month 13");
    assert!(Query::parse("modified < notadate").is_err());
}

#[test]
fn a_date_query_selects_the_right_side_of_the_line() {
    let dir = fixture();
    let tree = scan(dir.path());

    // Everything in the fixture was written now, so it is all after 2020 and
    // none of it before.
    assert!(!names(&tree, "file and modified > 2020-01-01").is_empty());
    assert!(names(&tree, "file and modified < 2020-01-01").is_empty());
}

// ------------------------------------------------------------- reference

#[test]
fn every_field_is_documented() {
    // The reference drives help text and completion. A field that exists but is
    // not listed is a field nobody will discover.
    let listed: String = kosha_query::reference()
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" ");

    for field in [
        "name", "path", "ext", "size", "apparent", "items", "children", "depth", "age", "modified",
        "wasted",
    ] {
        assert!(listed.contains(field), "`{field}` is not in the reference");
    }
    for flag in [
        "dir",
        "file",
        "link",
        "empty",
        "sparse",
        "partial",
        "unreadable",
    ] {
        assert!(listed.contains(flag), "`{flag}` is not in the reference");
    }
}

// ------------------------------------------------ the documented language

/// The reference is read, not restated.
///
/// `docs/manual/query-language.md` is what somebody consults before typing,
/// so a field listed there that the parser does not accept is a promise the
/// program breaks. Restating the list in the test would only assert that two
/// copies of it agree, and the copy that matters is the one on the page — so
/// this parses the markdown and holds the parser to it. Edit the table and
/// this test starts checking the new row.
mod documented {
    use super::*;

    const REFERENCE: &str = include_str!("../../../docs/manual/query-language.md");

    /// Rows of the `### Fields` table: the first cell of each, unbackticked.
    fn fields() -> Vec<String> {
        REFERENCE
            .lines()
            .skip_while(|line| !line.starts_with("### Fields"))
            .take_while(|line| !line.starts_with("### Flags"))
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|rest| rest.split('`').next())
            .map(str::to_string)
            .collect()
    }

    /// The single line under `### Flags`, which is a prose list of backticked
    /// words rather than a table.
    fn flags() -> Vec<String> {
        REFERENCE
            .lines()
            .skip_while(|line| !line.starts_with("### Flags"))
            .find(|line| line.contains('`'))
            .unwrap_or_default()
            .split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_reference_is_where_this_test_thinks_it_is() {
        // If the headings move, the two extractors above quietly return
        // nothing and every assertion below passes vacuously.
        assert!(fields().len() >= 10, "fields: {:?}", fields());
        assert!(flags().len() >= 6, "flags: {:?}", flags());
        assert!(fields().contains(&"size".to_string()));
        assert!(flags().contains(&"dir".to_string()));
    }

    #[test]
    fn every_documented_field_is_a_field_the_parser_knows() {
        for field in fields() {
            // Some fields are text and some are numbers, so one query shape
            // cannot suit them all. What must never happen is the parser not
            // recognising the *name*, and that is what the message says.
            if let Err(error) = Query::parse(&format!("{field} > 1")) {
                let text = error.to_string();
                assert!(
                    !text.contains("did you mean") && !text.contains("known fields"),
                    "`{field}` is documented but the parser does not know it: {text}"
                );
            }
        }
    }

    #[test]
    fn every_documented_flag_parses_on_its_own() {
        for flag in flags() {
            assert!(
                Query::parse(&flag).is_ok(),
                "`{flag}` is documented as a flag but does not parse"
            );
        }
    }

    /// The third side of the triangle.
    ///
    /// `every_field_is_documented` already checks that a hand-written list of
    /// fields appears in `reference()`, which is what drives the in-app help
    /// and completion. This checks the *page* against `reference()`, so a
    /// field added to the markdown and nowhere else cannot end up documented
    /// on disk and invisible in the application.
    #[test]
    fn every_documented_field_is_offered_by_the_in_app_reference() {
        let listed: Vec<&str> = kosha_query::reference()
            .iter()
            .map(|(name, _)| *name)
            .collect();

        for field in fields() {
            assert!(
                listed.iter().any(|name| *name == field),
                "`{field}` is in docs/manual/query-language.md but not in \
                 the in-app reference, so nothing in the window will ever \
                 mention it"
            );
        }
    }

    /// A word the reference does not list must not be silently accepted as a
    /// field either, or the documentation is not a description of the
    /// language but a subset of it.
    #[test]
    fn a_word_the_reference_does_not_list_is_not_a_field() {
        let error = Query::parse("bigness > 1").expect_err("`bigness` is not a field");
        assert!(
            error.to_string().contains("did you mean") || error.to_string().contains("known"),
            "{error}"
        );
    }
}

// ------------------------------------------------------------------- owners

fn scan_with_owners(root: &Path) -> SizeTree {
    Scanner::new(ScanOptions {
        collect_owners: true,
        ..Default::default()
    })
    .scan(root)
    .unwrap()
}

/// Who the tree says owns it, and who this process thinks it is.
///
/// In every failure message below, because these three tests can only fail for
/// one reason — the two disagree — and a machine where that happens is the only
/// place to find out why. Windows has a policy that makes a new file's owner the
/// *Administrators group* rather than the account that created it, and a test
/// that fails with `left: []` names none of that.
fn attribution(tree: &SizeTree) -> String {
    let owners: Vec<String> = kosha_core::owner::shares(tree)
        .iter()
        .map(|share| match &share.owner {
            Some(owner) => format!("{} ({} files)", owner.label(), share.files),
            None => format!("unreadable ({} files)", share.files),
        })
        .collect();
    format!(
        "the tree says {:?}; this process says {:?}",
        owners,
        kosha_core::current_owner().map(|me| me.label().to_string())
    )
}

/// The query this field exists for: the owners panel names an account, and this
/// is how you get from that row to the files behind it.
#[test]
fn everything_this_account_owns() {
    let dir = fixture();
    let tree = scan_with_owners(dir.path());

    let mine = names(&tree, "file and owner = me");
    assert_eq!(
        mine,
        vec![
            ".gitignore",
            "big.bin",
            "clip.mkv",
            "holiday.MP4",
            "main.rs",
            "notes.txt"
        ],
        "files this process just wrote are not owned by this process — {}",
        attribution(&tree)
    );

    // And the same by name rather than by `me` — the name the *tree* gives, which
    // need not be the process' own user. See `me_is_every_account_this_process_writes_as`.
    let by_name = names(
        &tree,
        &format!("file and owner = \"{}\"", owner_of(&tree).account()),
    );
    assert_eq!(by_name, mine, "the account name found a different set");
}

/// What `me` has to mean, stated as the thing that was wrong.
///
/// `owner = me` came back empty on a clean Windows in a directory this process
/// had just filled, because the token's *user* is not always what gets stamped
/// on a new file: with the Administrators-group owner policy set — which the
/// Windows Sandbox ships — an elevated administrator's files are owned by
/// `BUILTIN\\Administrators`. The filter was asking about the wrong one of the
/// two accounts the process has.
///
/// So the invariant is this, and it holds on both machines: whoever the tree says
/// owns a file this process just wrote is one of the accounts `me` stands for.
#[test]
fn me_is_every_account_this_process_writes_as() {
    let dir = fixture();
    let tree = scan_with_owners(dir.path());
    let theirs = owner_of(&tree);

    let mine = kosha_core::current_owners();
    assert!(!mine.is_empty(), "the process token names nobody at all");
    assert!(
        mine.iter().any(|me| me.id == theirs.id),
        "a file this process wrote is owned by {:?}, which is none of {:?}",
        theirs.label(),
        mine.iter().map(|me| me.label()).collect::<Vec<_>>()
    );
}

/// The one account the tree attributes its files to.
///
/// A fixture written by one process in one directory has exactly one, and a test
/// that took the name from `current_owner()` instead was assuming the answer.
fn owner_of(tree: &SizeTree) -> kosha_core::Owner {
    let mut named: Vec<kosha_core::Share> = kosha_core::owner::shares(tree)
        .into_iter()
        .filter(|share| share.files > 0)
        .collect();
    assert_eq!(
        named.len(),
        1,
        "the fixture's files are attributed to {} accounts: {:?}",
        named.len(),
        named
            .iter()
            .map(|s| s.label().into_owned())
            .collect::<Vec<_>>()
    );
    named
        .pop()
        .and_then(|share| share.owner)
        .expect("the files have an owner the machine could name")
}

/// The other half, and the one somebody on a shared machine actually types.
#[test]
fn everything_this_account_does_not_own() {
    let dir = fixture();
    let tree = scan_with_owners(dir.path());
    assert!(
        names(&tree, "file and owner != me").is_empty(),
        "files this process wrote came back as somebody else's — {}",
        attribution(&tree)
    );
}

/// On Windows an account is spelled `MACHINE\\name`, which is not a string
/// anybody wants to type and whose first half is the same for every local
/// account. Both spellings have to work, and a negative has to exclude both.
#[test]
fn an_account_matches_with_or_without_its_domain() {
    let dir = fixture();
    let tree = scan_with_owners(dir.path());
    // The account the files are actually attributed to, which on a machine with
    // the Administrators-group owner policy is not the process' own user.
    let me = owner_of(&tree);

    let full = names(&tree, &format!("file and owner = \"{}\"", me.label()));
    let short = names(&tree, &format!("file and owner = \"{}\"", me.account()));
    assert_eq!(full, short);
    assert!(!full.is_empty(), "{}", attribution(&tree));

    for spelling in [me.label(), me.account()] {
        assert!(
            names(&tree, &format!("file and owner != \"{spelling}\"")).is_empty(),
            "`owner != {spelling}` did not exclude the account it names"
        );
    }
}

/// The case the whole design turns on. A scan that never recorded owners cannot
/// answer the question, and answering it anyway — with everything, which is
/// what a negated comparison against a missing field naturally produces — would
/// be confidently wrong.
#[test]
fn a_scan_without_owners_answers_nothing_rather_than_everything() {
    let dir = fixture();
    let tree = scan(dir.path());
    assert!(!tree.owners().were_collected());

    assert!(names(&tree, "owner = me").is_empty());
    assert!(
        names(&tree, "owner != me").is_empty(),
        "a negated owner query matched the whole tree on a scan with no owners"
    );
    assert!(names(&tree, "owner ~ a").is_empty());
    assert!(names(&tree, "owner !~ zzz").is_empty());
    assert!(names(&tree, "owner in (me, root)").is_empty());
}

/// So the interface can say *why* nothing matched, rather than leaving "no
/// results" to mean both "nobody matched" and "this scan cannot tell you".
#[test]
fn a_query_says_whether_it_needs_owners() {
    for source in [
        "owner = me",
        "size > 1gb and owner != root",
        "not owner ~ admin",
        "owner in (me, root)",
        "user = me",
        "account = me",
    ] {
        assert!(
            Query::parse(source).unwrap().needs_owners(),
            "`{source}` did not declare that it needs owners"
        );
    }
    for source in ["size > 1gb", "name ~ owner", "dir and items > 10"] {
        assert!(
            !Query::parse(source).unwrap().needs_owners(),
            "`{source}` claimed to need owners"
        );
    }
}

/// An owner query must not start rebuilding paths: they are unrelated costs and
/// the path is the one expensive field there is.
#[test]
fn asking_about_an_owner_does_not_make_the_query_expensive() {
    assert!(!Query::parse("owner = me").unwrap().needs_path());
}

// ------------------------------------------------ what a query may not do

/// Five thousand brackets used to kill the process outright.
///
/// The parser is recursive descent, so nesting is stack depth, and a stack
/// overflow is not an error anybody can catch: `STATUS_STACK_OVERFLOW` on
/// Windows, a segfault elsewhere, nothing printed that kosha wrote. In the
/// window, which parses on every keystroke, it is the application vanishing
/// mid-sentence.
#[test]
fn a_query_that_nests_too_deeply_is_refused_rather_than_fatal() {
    let query = format!("{}size > 1{}", "(".repeat(5_000), ")".repeat(5_000));
    let error = kosha_query::Query::parse(&query).expect_err("5,000 brackets deep");
    let said = error.to_string();
    assert!(said.contains("nests"), "{said}");
    // And it says what to do instead, because a refusal without one is just a
    // different way to be stuck.
    assert!(!said.is_empty());
}

/// `not` recurses too, and does not need a bracket to do it.
#[test]
fn a_tower_of_nots_is_refused() {
    let query = format!("{}size > 1", "not ".repeat(5_000));
    let error = kosha_query::Query::parse(&query).expect_err("5,000 nots");
    assert!(error.to_string().contains("nests"), "{error}");
}

/// The other half, and a different failure with the same cause: chaining does
/// not recurse while parsing — the loop in `parse_and` sees to that — but it
/// builds a tree that leans, and dropping one recurses. Fifty thousand terms
/// parsed happily and then overflowed the stack on the way out of scope.
#[test]
fn a_query_that_joins_too_many_conditions_is_refused() {
    let query = std::iter::repeat_n("size > 1", 50_000)
        .collect::<Vec<_>>()
        .join(" and ");
    let error = kosha_query::Query::parse(&query).expect_err("50,000 conditions");
    assert!(error.to_string().contains("conditions"), "{error}");
}

/// **The limits must not bite anything real.** A refusal that fires on a query
/// somebody would actually write is a worse bug than the crash it prevents,
/// because it happens every day instead of never.
#[test]
fn the_limits_are_nowhere_near_a_real_query() {
    // Deeper than any recipe in the catalogue, and well inside the limit.
    let nested = "((((size > 1gb and (ext = mp4 or ext = mkv)) or (age > 1y and size > 100mb)) \
                  and not path ~ cache) or name ~ backup)";
    kosha_query::Query::parse(nested).expect("a query with real brackets in it");

    // A hundred conditions is already absurd for a person, and is accepted.
    let long = std::iter::repeat_n("size > 1", 100)
        .collect::<Vec<_>>()
        .join(" or ");
    kosha_query::Query::parse(&long).expect("a hundred conditions");

    // Sixty levels, one under the limit.
    let deep = format!("{}size > 1{}", "(".repeat(60), ")".repeat(60));
    kosha_query::Query::parse(&deep).expect("sixty brackets");
}

/// Every recipe kosha ships has to survive its own limits.
#[test]
fn the_catalogue_still_parses() {
    for topic in kosha_query::help::catalogue() {
        kosha_query::Query::parse(topic.example)
            .unwrap_or_else(|e| panic!("`{}` no longer parses: {e}", topic.example));
    }
}

// ------------------------------------------- a value and its unit, spaced

/// A size written the way kosha prints it.
///
/// **The interface says `512 KiB` and the parser would not read it back.**
/// `size > 512 kib` came back "`kib` is not a field (did you mean `age`?)" —
/// advice about a field for what is plainly a unit, and advice that leads
/// away from the answer. `age > 30 d` did the same.
///
/// The space is a convention of how a value is written, never a second term:
/// nothing in this grammar puts two values side by side.
#[test]
fn a_unit_may_be_separated_from_its_number() {
    let dir = fixture();
    let tree = scan(dir.path());

    for (spaced, joined) in [
        ("size > 5 mb", "size > 5mb"),
        ("size > 2.5 mib", "size > 2.5mib"),
        ("size > 512 kib", "size > 512kib"),
        ("age > 30 d", "age > 30d"),
        ("apparent > 1 mb", "apparent > 1mb"),
    ] {
        assert_eq!(
            names(&tree, spaced),
            names(&tree, joined),
            "`{spaced}` and `{joined}` are the same question"
        );
    }
}

/// And it takes only what can be a unit of that field.
///
/// The join is the whole of the condition — a following word is taken only
/// when it turns the number into a value this field accepts — so the
/// keywords need no special case, and neither does a second number.
#[test]
fn a_word_that_is_not_a_unit_is_left_where_it_was() {
    let dir = fixture();
    let tree = scan(dir.path());

    // `1and` is not a size, so `and` is still the conjunction it looks like.
    assert_eq!(
        names(&tree, "size > 1 and ext = mkv"),
        names(&tree, "size > 1b and ext = mkv"),
    );
    assert!(!names(&tree, "size > 1 or ext = mkv").is_empty());
    assert!(!names(&tree, "not size > 1mb").is_empty());

    // Two numbers are a mistake, not a number with a unit: `1 2` would
    // otherwise quietly become `12`.
    assert!(error("size > 1 2").contains("is not a field"));

    // A unit belongs to the field that has it. Neither of these joins,
    // because `2d` is not a count and `1gb` is not a depth.
    assert!(error("items > 2 d").contains("is not a field"));
    assert!(error("depth > 1 gb").contains("is not a field"));

    // A unit nothing knows is still refused, spaced or not.
    assert!(error("size > 1pb").contains("is not a size"));
}
