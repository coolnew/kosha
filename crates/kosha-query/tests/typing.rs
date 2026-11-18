// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Everything a person can type, including the half-finished and the hostile.
//!
//! **This is the only parser in the project fed by a keyboard rather than by a
//! disk.** Every reader gets bytes somebody else wrote and is swept for the bit
//! that was flipped in transit; this gets characters somebody is typing *now*,
//! in the window, which parses on every keystroke and offers completions from
//! the same text. A panic here is not a wrong answer, it is the application
//! vanishing mid-sentence.
//!
//! The two limits in `syntax.rs` came out of exactly this and were found one at
//! a time: `(((…)))` five thousand deep overflowed the stack while parsing, and
//! fifty thousand conditions overflowed it afterwards, in a destructor. What was
//! missing was the sweep — a test that goes looking rather than waiting to be
//! told where to look.
//!
//! Either answer is always correct. The assertion is reaching the next line.

use kosha_query::{Query, suggest};

/// Queries a person would really write, and the shapes the grammar has.
///
/// The corpus is the seed; the mutations below are what make it a sweep. It
/// covers every operator, both quote characters, `in (…)`, `not`, brackets,
/// bare words, sizes with units, ages, and the flags — so a mutation has
/// something of every kind to damage.
const REAL: &[&str] = &[
    "size > 1gb",
    "size >= 4.5 MiB and age > 1y",
    "ext in (mp4, mkv, avi)",
    "not (ext = tmp or ext = bak)",
    "name ~ report and size < 100kb",
    "path ~ \"program files\" and is dir",
    "is sparse or is hardlink",
    "owner = \"BUILTIN\\\\Administrators\"",
    "age < 30d and size > 0",
    "created >= @1794398400 and created <= @1795003200",
    "name startswith report and name endswith '.tar.gz'",
    "invoice",
    "name != 'draft.txt'",
    "size > 1gb and (ext = iso or ext = img) and not is dir",
];

/// The characters a query is made of, and a few it is not.
///
/// Each is spliced in at every position of every seed. The last three are the
/// ones that make this more than an ASCII test: a character of two bytes, one
/// of three, and one of four, so a lexer that walks `as_bytes()` and slices the
/// `&str` has every chance to land in the middle of one.
const INTERESTING: &[char] = &[
    '@',
    '(',
    ')',
    ',',
    '"',
    '\'',
    '>',
    '<',
    '=',
    '!',
    '~',
    '&',
    '|',
    '\\',
    ' ',
    '\t',
    '.',
    '-',
    '+',
    '0',
    '9',
    'e',
    'x',
    '\u{00E9}',
    '\u{65E5}',
    '\u{1F600}',
];

/// Parse it, and sometimes ask what it would suggest.
///
/// **`suggest` is sampled and `parse` is not, because of what the sampling
/// measured.** Parsing every one of these fifteen thousand mutations takes
/// 0.03 seconds; suggesting for every one of them takes fifteen. That is not a
/// slow test, it is the cost of the feature: a mutation is usually a query with
/// a mistake in it, which is the repair path, which is about a millisecond even
/// on forty characters. One in seven keeps thousands of calls in the sweep and
/// the suite in seconds, and the shapes that matter most to `suggest` have a
/// test of their own below.
fn parses(input: &str, suggesting: bool) {
    if suggesting {
        let _ = suggest(input, 8);
    }
    match Query::parse(input) {
        Ok(query) => {
            // Touching what a caller would touch: a query nobody asks anything
            // of is a query half of whose code never ran.
            let _ = query.source();
            let _ = query.is_plain_text();
            let _ = query.needs_path();
        }
        Err(error) => {
            // `text()` slices the source by a byte span the lexer computed. A
            // span that lands inside a character is the panic this is for, and
            // it is why the mutations put multi-byte characters everywhere.
            let _ = error.text();
            let _ = error.to_string();
            let _ = error.kind;
            let _ = error.span.clone();
        }
    }
}

#[test]
fn every_character_spliced_into_every_position_of_every_query() {
    let mut tried = 0usize;
    for seed in REAL {
        parses(seed, true);
        // Character boundaries, not byte offsets: inserting inside a character
        // would not be a string at all, and `char_indices` is what a person
        // moving a cursor through the text produces.
        let positions: Vec<usize> = seed
            .char_indices()
            .map(|(at, _)| at)
            .chain(std::iter::once(seed.len()))
            .collect();
        for at in positions {
            for ch in INTERESTING {
                let mut mutated = String::with_capacity(seed.len() + 4);
                mutated.push_str(&seed[..at]);
                mutated.push(*ch);
                mutated.push_str(&seed[at..]);
                parses(&mutated, tried.is_multiple_of(7));
                tried += 1;
            }
        }
    }
    assert!(tried > 5_000, "only {tried} mutations");
}

#[test]
fn every_query_cut_off_at_every_character() {
    let mut tried = 0usize;
    for seed in REAL {
        for (at, _) in seed.char_indices() {
            parses(&seed[..at], true);
            tried += 1;
        }
    }
    assert!(tried > 200, "only {tried} truncations");
}

#[test]
fn every_character_of_every_query_deleted_or_doubled() {
    for seed in REAL {
        for (at, ch) in seed.char_indices() {
            let end = at + ch.len_utf8();
            let mut without = String::from(&seed[..at]);
            without.push_str(&seed[end..]);
            parses(&without, true);

            let mut twice = String::from(&seed[..at]);
            twice.push(ch);
            twice.push_str(&seed[at..]);
            parses(&twice, true);
        }
    }
}

/// The shapes that are not typing mistakes but weapons.
///
/// Each one is a thing that has killed a recursive-descent parser somewhere:
/// nesting to the limit and one past it, a chain longer than the limit, a token
/// longer than any word, and a string that is nothing but the characters the
/// lexer switches on.
#[test]
fn nothing_pathological_brings_the_parser_down() {
    let deep_enough = format!("{}size > 1{}", "(".repeat(64), ")".repeat(64));
    let too_deep = format!("{}size > 1{}", "(".repeat(5_000), ")".repeat(5_000));
    let long_chain = (0..50_000)
        .map(|n| format!("size > {n}"))
        .collect::<Vec<_>>()
        .join(" and ");
    let unclosed = "(".repeat(5_000);
    let mismatched = ")".repeat(5_000);

    for input in [
        deep_enough.as_str(),
        too_deep.as_str(),
        long_chain.as_str(),
        unclosed.as_str(),
        mismatched.as_str(),
        &"a".repeat(100_000),
        &"\u{1F600}".repeat(10_000),
        &"\"".repeat(1_000),
        &"><=!~&|,()".repeat(1_000),
        &"size > ".repeat(1_000),
        &"not ".repeat(5_000),
        &"in (".repeat(1_000),
        "",
        " ",
        "\0",
        "\u{FFFD}",
    ] {
        parses(input, true);
    }

    // And the one that has to *succeed*, so the limits are not simply refusing
    // everything: sixty-four is the documented ceiling and must be reachable.
    assert!(
        Query::parse(&deep_enough).is_ok(),
        "the documented nesting limit is not reachable"
    );
    assert!(
        Query::parse(&too_deep).is_err(),
        "nesting past the limit has to be refused rather than recursed"
    );
}

/// Suggesting stops where a person's typing does.
///
/// **Found by the sweep above taking seventy-six seconds.** Repairing a
/// candidate copies the whole input and parses it again, several candidates
/// deep, so the cost grows faster than the text: 531 ms for 10 KB, 1.6 s for
/// 20 KB, **81 seconds** for 839 KB. The window calls this on every keystroke
/// on the thread that draws, so it is not a slow suggestion but a frozen
/// application, and it needs a paste rather than an attack.
///
/// A size rather than a duration, because a timing assertion fails on a busy
/// machine and passes on a fast one with the bug still in it.
#[test]
fn a_query_longer_than_anybody_types_is_not_thought_about() {
    // The catalogue's longest recipe, and well inside the limit.
    let real = "ext in (jpg, jpeg, png, heic, raw, tiff) and size > 10mb";
    assert!(real.len() < 256);
    assert!(
        Query::parse(real).is_ok(),
        "the corpus has to be a query for the rest to mean anything"
    );

    // At the limit, help is still offered for something that needs it.
    let nearly = format!("{} and siz > 1gb", "a".repeat(256 - 20));
    assert!(nearly.len() <= 256);
    assert!(
        !suggest(&nearly, 3).is_empty(),
        "a repairable query inside the limit gets no help"
    );

    // Past it, nothing - which is the right answer as well as the fast one: a
    // suggestion replaces what is in the box, and no correction to half a
    // megabyte of text is one anybody wanted.
    let pasted = (0..50_000)
        .map(|n| format!("siz > {n}"))
        .collect::<Vec<_>>()
        .join(" and ");
    assert!(pasted.len() > 500_000);
    assert!(suggest(&pasted, 3).is_empty());
    // And the filter itself still reads it, because that is a different job.
    assert!(Query::parse("size > 1gb").is_ok());
}
