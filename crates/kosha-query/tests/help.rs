// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The searchable help, and the suggestions built on it.
//!
//! Two properties matter more than any individual match. Every example in the
//! catalogue must actually run, or the help teaches people syntax that fails.
//! And every suggestion must be a query that parses, or clicking one replaces a
//! broken query with a differently broken one.

use kosha_query::{Query, SuggestionKind, TopicKind, help, suggest};

// ------------------------------------------------------------ the catalogue

#[test]
fn every_example_in_the_help_actually_runs() {
    // The whole premise of click-to-insert is that what goes in the box works.
    for topic in help::catalogue() {
        assert!(
            Query::parse(topic.example).is_ok(),
            "`{}` offers an example that does not parse: {:?}\n  {:?}",
            topic.label,
            topic.example,
            Query::parse(topic.example).err().map(|e| e.to_string()),
        );
    }
}

#[test]
fn every_field_and_flag_has_a_topic() {
    // A field nobody can discover is a field that does not exist.
    let labels: Vec<&str> = help::catalogue().iter().map(|t| t.label).collect();

    for field in help::field_names() {
        assert!(labels.contains(&field), "no help topic for field `{field}`");
    }
    for flag in help::flag_names() {
        assert!(labels.contains(&flag), "no help topic for flag `{flag}`");
    }
}

#[test]
fn topics_are_not_duplicated() {
    let mut labels: Vec<&str> = help::catalogue().iter().map(|t| t.label).collect();
    labels.sort_unstable();
    let before = labels.len();
    labels.dedup();
    assert_eq!(before, labels.len(), "two topics share a label");
}

#[test]
fn summaries_fit_on_one_line() {
    for topic in help::catalogue() {
        assert!(
            topic.summary.chars().count() <= 72,
            "`{}` has a {}-character summary",
            topic.label,
            topic.summary.chars().count()
        );
        assert!(
            !topic.summary.ends_with('.'),
            "`{}` ends in a period",
            topic.label
        );
    }
}

// -------------------------------------------------------------- the search

fn labels(needle: &str) -> Vec<&'static str> {
    help::search(needle, 8)
        .into_iter()
        .map(|t| t.label)
        .collect()
}

#[test]
fn an_empty_search_shows_the_catalogue_in_order() {
    let found = help::search("", 5);
    assert_eq!(found.len(), 5);
    assert_eq!(found[0].label, help::catalogue()[0].label);
}

#[test]
fn searching_by_field_name_finds_it_first() {
    assert_eq!(labels("size")[0], "size");
    assert_eq!(labels("depth")[0], "depth");
}

#[test]
fn searching_by_a_word_someone_would_actually_use() {
    // Nobody searches documentation with the word the documentation uses.
    assert!(labels("big").contains(&"Big files"));
    assert!(
        labels("old")
            .iter()
            .any(|l| l.contains("nobody has opened") || *l == "age")
    );
    assert!(labels("video").contains(&"Videos and other media"));
    assert!(labels("folder").iter().any(|l| *l == "dir" || *l == "path"));
    assert!(labels("photo").contains(&"Photos and images"));
}

#[test]
fn a_typo_still_finds_the_topic() {
    assert!(labels("siz").contains(&"size"), "{:?}", labels("siz"));
    assert!(
        labels("moddified").contains(&"modified"),
        "{:?}",
        labels("moddified")
    );
    assert!(labels("exten").contains(&"ext"), "{:?}", labels("exten"));
}

#[test]
fn searching_for_a_concept_finds_the_operator() {
    assert!(labels("contains").iter().any(|l| l.contains('~')));
    assert!(
        labels("exclude")
            .iter()
            .any(|l| l.contains("not") || l.contains("except"))
    );
    assert!(
        labels("either")
            .iter()
            .any(|l| l.contains("in") || l.contains("or"))
    );
}

#[test]
fn nonsense_finds_nothing_rather_than_everything() {
    // A search that always matches is a search nobody trusts.
    assert!(
        help::search("qzxwvk", 8).is_empty(),
        "{:?}",
        labels("qzxwvk")
    );
}

#[test]
fn results_respect_the_limit() {
    assert!(help::search("size", 2).len() <= 2);
    assert!(help::search("", 3).len() <= 3);
}

// ---------------------------------------------------------- the suggestions

fn suggestions(input: &str) -> Vec<kosha_query::Suggestion> {
    suggest(input, 5)
}

fn queries(input: &str) -> Vec<String> {
    suggestions(input).into_iter().map(|s| s.query).collect()
}

#[test]
fn every_suggestion_parses() {
    // Clicking a suggestion must not replace a broken query with another one.
    for input in [
        "saiz > 1mb",
        "size >",
        "size",
        "size > 10furlongs",
        "size ~ 100",
        "(size > 1mb",
        "name ~ \"unclosed",
        "big old files",
        "size 1gb",
        "video",
        "folder > 1gb",
        "file > 100mb",
        "siz 1gb",
        "ext in (mp4",
        "modified > 1969-12-31",
        "modified > not-a-date",
        "modified > 2026-02-30",
    ] {
        for suggestion in suggestions(input) {
            assert!(
                Query::parse(&suggestion.query).is_ok(),
                "`{input}` suggested `{}`, which does not parse",
                suggestion.query
            );
        }
    }
}

#[test]
fn a_misspelled_field_is_corrected_in_place() {
    let found = queries("saiz > 1mb");
    assert!(
        found.contains(&"size > 1mb".to_string()),
        "expected the rest of the query kept intact: {found:?}"
    );
}

#[test]
fn a_correction_keeps_the_rest_of_a_longer_query() {
    let found = queries("file and saiz > 1mb and not path ~ cache");
    assert!(
        found.contains(&"file and size > 1mb and not path ~ cache".to_string()),
        "{found:?}"
    );
}

#[test]
fn a_field_with_no_comparison_gets_one() {
    let found = queries("size");
    assert!(found.iter().any(|q| q.starts_with("size >")), "{found:?}");
}

#[test]
fn a_missing_value_is_offered_one() {
    let found = queries("size >");
    assert!(found.iter().any(|q| Query::parse(q).is_ok()), "{found:?}");
    assert!(found.iter().any(|q| q.starts_with("size >")), "{found:?}");
}

#[test]
fn a_bad_unit_keeps_the_number() {
    // "10 furlongs" is "10 megabytes" with the wrong word attached, and keeping
    // the 10 makes it read as a correction rather than a replacement.
    let found = queries("size > 10furlongs");
    assert!(found.contains(&"size > 10mb".to_string()), "{found:?}");
}

#[test]
fn a_substring_operator_on_a_number_is_turned_into_a_comparison() {
    let found = queries("size ~ 100");
    assert!(found.contains(&"size > 100".to_string()), "{found:?}");
}

#[test]
fn an_unclosed_bracket_is_closed() {
    assert!(queries("(size > 1mb").contains(&"(size > 1mb)".to_string()));
}

#[test]
fn an_unclosed_quote_is_closed() {
    let found = queries("name ~ \"annual report");
    assert!(found.iter().any(|q| q.ends_with('"')), "{found:?}");
}

#[test]
fn a_field_and_a_value_with_no_operator_is_understood() {
    // `size` is a field, so the parser gets far enough to say what is missing
    // and the answer is a definite correction.
    let found = queries("size 1gb");
    assert!(found.contains(&"size > 1gb".to_string()), "{found:?}");
    assert_eq!(
        suggestions("size 1gb")
            .into_iter()
            .find(|s| s.query == "size > 1gb")
            .unwrap()
            .kind,
        SuggestionKind::Correction
    );
}

#[test]
fn a_misspelled_field_with_a_value_is_interpreted() {
    // `siz 1gb` is not a query at all — both words are ordinary text, so it
    // parses as a perfectly valid name search that will find nothing. Only a
    // suggester can catch this one.
    let found = queries("siz 1gb");
    assert!(found.contains(&"size > 1gb".to_string()), "{found:?}");
    assert_eq!(
        suggestions("siz 1gb")
            .into_iter()
            .find(|s| s.query == "size > 1gb")
            .unwrap()
            .kind,
        SuggestionKind::Interpretation
    );
}

#[test]
fn a_flag_compared_like_a_field_is_corrected() {
    // Among the most natural things to type, and among the most certainly
    // wrong: `file` is a yes-or-no property, so `>` has nothing to compare.
    assert!(
        queries("file > 100mb").contains(&"file and size > 100mb".to_string()),
        "{:?}",
        queries("file > 100mb")
    );
    // Aliases resolve to the canonical spelling, which is the one documented.
    assert!(
        queries("folder > 1gb").contains(&"dir and size > 1gb".to_string()),
        "{:?}",
        queries("folder > 1gb")
    );
}

#[test]
fn a_description_of_a_task_offers_a_recipe() {
    let found = queries("big old files");
    assert!(!found.is_empty(), "no suggestions for a plain description");
    assert!(found.iter().all(|q| Query::parse(q).is_ok()), "{found:?}");
    assert!(
        suggestions("big old files")
            .iter()
            .any(|s| s.kind == SuggestionKind::Example)
    );
}

#[test]
fn a_working_query_is_left_alone() {
    // Nothing to suggest about a query that says what it means.
    for query in [
        "size > 1gb",
        "file and size > 200mb",
        "ext in (mp4, mkv) and age > 1y",
    ] {
        assert!(
            suggestions(query).is_empty(),
            "`{query}` works, but was second-guessed: {:?}",
            queries(query)
        );
    }
}

#[test]
fn a_plain_name_search_of_one_word_is_left_alone() {
    // Typing a filename is the commonest thing anyone does here, and it is not
    // a mistake. A suggester with an opinion about it is one people mute.
    assert!(suggestions("node_modules").is_empty());
    assert!(suggestions("invoice").is_empty());
}

#[test]
fn nothing_is_suggested_for_nothing() {
    assert!(suggestions("").is_empty());
    assert!(suggestions("    ").is_empty());
}

#[test]
fn suggestions_are_deduplicated() {
    for input in ["big old files", "saiz > 1mb", "video files"] {
        let found = queries(input);
        let mut unique = found.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(found.len(), unique.len(), "`{input}` repeated a suggestion");
    }
}

#[test]
fn corrections_come_before_examples() {
    // A fix for what you typed is more use than a recipe for something else.
    let found = suggestions("saiz > 1mb");
    let first_example = found.iter().position(|s| s.kind == SuggestionKind::Example);
    let last_correction = found
        .iter()
        .rposition(|s| s.kind == SuggestionKind::Correction);

    if let (Some(example), Some(correction)) = (first_example, last_correction) {
        assert!(correction < example, "an example outranked a correction");
    }
}

#[test]
fn every_suggestion_explains_itself() {
    for input in [
        "saiz > 1mb",
        "size > 10furlongs",
        "big old files",
        "size 1gb",
    ] {
        for suggestion in suggestions(input) {
            assert!(
                !suggestion.reason.trim().is_empty(),
                "`{input}` suggested `{}` without saying why",
                suggestion.query
            );
        }
    }
}

#[test]
fn the_number_of_suggestions_is_bounded() {
    // A wall of guesses is worse than none.
    for input in ["big old large huge files video photo", "saiz > 1mb"] {
        assert!(suggest(input, 3).len() <= 3);
    }
}

// ------------------------------------------------------ kinds are meaningful

#[test]
fn recipes_are_complete_queries_and_fields_are_fragments_of_one() {
    for topic in help::catalogue() {
        let parsed = Query::parse(topic.example).unwrap();
        if topic.kind == TopicKind::Recipe {
            assert!(
                !parsed.is_plain_text(),
                "recipe `{}` is only a name search",
                topic.label
            );
        }
    }
}

#[test]
fn a_query_wrong_in_two_places_is_still_fixed() {
    // The parser reports the first problem only. Correcting `saiz` gives
    // `size > 10furlongs`, which is still not a query — so without following
    // the fix through, a user with two typos is offered nothing at the moment
    // they need the most help.
    let found = queries("saiz > 10furlongs");
    assert!(
        found.contains(&"size > 10mb".to_string()),
        "expected both problems fixed: {found:?}"
    );
}

#[test]
fn a_repaired_suggestion_is_offered_before_unrelated_examples() {
    let found = suggestions("saiz > 10furlongs");
    let fix = found.iter().position(|s| s.query == "size > 10mb");
    let example = found.iter().position(|s| s.kind == SuggestionKind::Example);

    assert!(fix.is_some(), "{:?}", queries("saiz > 10furlongs"));
    if let (Some(fix), Some(example)) = (fix, example) {
        assert!(fix < example, "an unrelated example outranked the fix");
    }
}

#[test]
fn repair_gives_up_rather_than_inventing_something() {
    // Three rounds, then silence. A "fix" arrived at by rewriting everything is
    // no longer a fix for what was typed.
    for input in ["qqq > www", "zzz zzz zzz"] {
        for suggestion in suggestions(input) {
            assert!(
                suggestion.kind == SuggestionKind::Example
                    || Query::parse(&suggestion.query).is_ok(),
                "`{input}` offered `{}`",
                suggestion.query
            );
        }
    }
}
