// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Turning a query that went wrong into one that would work.
//!
//! # Offer, never rewrite
//!
//! Nothing here changes what the user typed. Every suggestion is a complete
//! replacement they can take or ignore, and taking one is always an explicit
//! act. Silently correcting a filter would show results for a question nobody
//! asked, and — worse — the user would not notice, because the results look
//! plausible either way.
//!
//! # When to speak
//!
//! Three situations, in descending order of confidence:
//!
//! 1. **The query is broken and we know why.** A misspelled field, a missing
//!    operator, a size that is not a size. These come with the exact fix.
//! 2. **The query parses as plain text but looks like an attempt at a query.**
//!    `size 1gb` is a name search that will match nothing, and almost certainly
//!    meant `size > 1gb`.
//! 3. **The text reads like a description of a task.** "big old files" is not a
//!    query and never will be, but the catalogue has a recipe for it.
//!
//! Anything else gets silence. A suggester that always has an opinion is one
//! people learn to ignore.

use crate::help::{self, Topic};
use crate::{ErrorKind, Query};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SuggestionKind {
    /// A fix for something demonstrably wrong.
    Correction,
    /// A guess at what the text was reaching for.
    Interpretation,
    /// A worked example from the catalogue.
    Example,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    /// The complete query to use. Applying a suggestion sets the box to this.
    pub query: String,
    /// Why it is being offered, in one short line.
    pub reason: String,
}

/// The longest input this will think about.
///
/// **A limit on work, not on queries.** Repairing a candidate copies the whole
/// input and parses it again, several candidates deep, so the cost of
/// suggesting grows faster than the text does: measured at 531 ms for 10 KB,
/// 1.6 s for 20 KB and **81 seconds** for 839 KB. The window calls this on
/// every keystroke, on the thread that draws, so that is not a slow suggestion
/// but a frozen application — and it needs no hostile input, only a paste.
///
/// **The number is set by the frame budget, not by how long a query can be.**
/// The cost is the repair path, which runs only when there is a mistake to
/// fix, and it was measured against the longest input at each size with a typo
/// in it: 64 bytes 3.7 ms, 128 bytes 5.1 ms, 256 bytes 8.1 ms, 512 bytes
/// 14.4 ms. A frame is 8.3 ms at 120 Hz and the refilter runs inside the same
/// one, so 512 would be a visible stutter on every keystroke of a long query
/// and 256 is about the most that fits.
///
/// That is still four and a half times the longest recipe in the catalogue.
/// Above it there is nothing useful to offer anyway: a suggestion *replaces*
/// what is in the box, and no correction to half a megabyte of text is one
/// somebody wanted. What would let this rise is waiting for a pause in the
/// typing rather than answering every keystroke, which belongs in the window
/// and not here.
///
/// The filter itself is unaffected and still parses the whole thing, which is
/// fast — 23 ms for the same 839 KB. What stops is the help.
const MAX_SUGGESTED: usize = 256;

/// What to offer for this input, best first. Empty when there is nothing
/// confident to say.
pub fn suggest(input: &str, limit: usize) -> Vec<Suggestion> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_SUGGESTED {
        return Vec::new();
    }

    let mut out = Vec::new();
    match Query::parse(trimmed) {
        Err(error) => corrections(trimmed, &error, &mut out),
        Ok(query) if query.is_plain_text() => plain_text_ideas(trimmed, &mut out),
        // A query that parses and is not a bare name is doing what it says.
        Ok(_) => {}
    }

    // A query can be wrong in more than one way, and the parser only reports
    // the first. `saiz > 10furlongs` corrects to `size > 10furlongs`, which is
    // still not a query — so each candidate is repaired until it runs or the
    // attempt is abandoned. Without this the user is offered nothing for a
    // query with two typos in it, which is exactly when they need the most
    // help.
    for suggestion in &mut out {
        if let Some(repaired) = repair(&suggestion.query, 3) {
            suggestion.query = repaired;
        }
    }

    // Nothing leaves here that does not run. A correction is a guess — the
    // nearest field to `big` is `dir`, which turns "big old files" into "dir
    // old files" — and offering a guess that is itself broken replaces one
    // dead end with another. Checking is cheaper than being careful.
    out.retain(|suggestion| {
        suggestion.query.trim() != trimmed && Query::parse(&suggestion.query).is_ok()
    });

    // Two routes can reach the same answer; the user should see it once.
    let mut seen = Vec::new();
    out.retain(|suggestion| {
        let fresh = !seen.contains(&suggestion.query);
        if fresh {
            seen.push(suggestion.query.clone());
        }
        fresh
    });

    out.truncate(limit);
    out
}

/// Fix a candidate until it parses, or give up.
///
/// Only corrections are followed, never examples: an example would replace the
/// user's query with something unrelated while claiming to be a fix for it.
fn repair(query: &str, depth: u8) -> Option<String> {
    let error = match Query::parse(query) {
        Ok(_) => return Some(query.to_string()),
        Err(error) => error,
    };
    if depth == 0 {
        return None;
    }

    let mut candidates = Vec::new();
    fixes(query, &error, &mut candidates);
    candidates
        .into_iter()
        .filter(|candidate| candidate.query != query)
        .find_map(|candidate| repair(&candidate.query, depth - 1))
}

/// Fixes for a query that does not parse, plus an example or two to fall back
/// on when the fix is not obvious.
fn corrections(input: &str, error: &crate::ParseError, out: &mut Vec<Suggestion>) {
    fixes(input, error, out);
    // Whatever the failure, a working example beats a dead end.
    add_examples(input, out, 2);
}

/// Fixes only — no examples, so this can be applied repeatedly.
fn fixes(input: &str, error: &crate::ParseError, out: &mut Vec<Suggestion>) {
    match &error.kind {
        ErrorKind::UnknownField { word } => {
            // The word might be a field spelled wrong, or a flag used where a
            // field was expected — `folder > 1gb` means `dir and size > 1gb`.
            for candidate in nearest(word, &help::field_names(), 3) {
                out.push(Suggestion {
                    kind: SuggestionKind::Correction,
                    query: replace_span(input, &error.span, candidate),
                    reason: format!("`{word}` is not a field — did you mean `{candidate}`?"),
                });
            }
            for candidate in nearest(word, &help::flag_names(), 1) {
                out.push(Suggestion {
                    kind: SuggestionKind::Correction,
                    query: replace_span(input, &error.span, candidate),
                    reason: format!("`{candidate}` is a yes-or-no property, not a field"),
                });
            }
        }

        ErrorKind::MissingOperator { field } => {
            // The operator belongs *between* the field and whatever follows it.
            // Appending blindly turns `size 1gb` into `size 1gb > 100mb`, which
            // is not what anyone meant and does not parse either.
            let after = text_after(input, field);
            match after {
                Some(value) if !value.is_empty() => {
                    for op in candidate_operators(field) {
                        out.push(Suggestion {
                            kind: SuggestionKind::Correction,
                            query: format!("{field} {op} {value}"),
                            reason: format!("`{field}` needs an operator before `{value}`"),
                        });
                    }
                }
                _ => out.push(Suggestion {
                    kind: SuggestionKind::Correction,
                    query: format!("{} {}", input.trim_end(), default_comparison(field)),
                    reason: format!("`{field}` needs something to compare against"),
                }),
            }
        }

        ErrorKind::MissingValue => {
            if let Some(field) = trailing_field(input) {
                out.push(Suggestion {
                    kind: SuggestionKind::Correction,
                    query: format!("{} {}", input.trim_end(), example_value(&field)),
                    reason: format!("add a value, like {}", example_value(&field)),
                });
            }
        }

        ErrorKind::BadValue { field, raw } => {
            // Keep whatever number they typed and fix only the unit, which is
            // almost always what went wrong.
            if let Some(fixed) = repair_value(field, raw) {
                out.push(Suggestion {
                    kind: SuggestionKind::Correction,
                    query: replace_span(input, &error.span, &fixed),
                    reason: format!("`{raw}` is not {}", value_noun(field)),
                });
            }
        }

        // Only where one value can express the fix. `name = *.txt` needs a
        // different operator as well, and a suggestion that changed the value
        // alone would offer a query that is still wrong.
        ErrorKind::UnmatchableText {
            field,
            raw,
            instead,
        } => {
            if let Some(better) = instead {
                out.push(Suggestion {
                    kind: SuggestionKind::Correction,
                    query: replace_span(input, &error.span, better),
                    reason: format!("`{raw}` cannot match any {field}"),
                });
            }
        }

        ErrorKind::TextOperatorOnNumber { field, raw } => out.push(Suggestion {
            kind: SuggestionKind::Correction,
            query: format!("{field} > {raw}"),
            reason: format!("`~` compares text, and `{field}` is a number"),
        }),

        ErrorKind::Unclosed => {
            let closed = close_openings(input);
            if closed != input {
                out.push(Suggestion {
                    kind: SuggestionKind::Correction,
                    query: closed,
                    reason: "something was left open".into(),
                });
            }
        }

        // `file > 100mb` and `folder > 1gb` are among the most natural things
        // to type and among the most certainly wrong: `file` and `dir` are
        // yes-or-no properties, so there is nothing for `>` to compare. The
        // parser can only report trailing input, but the shape is unmistakable.
        ErrorKind::Empty | ErrorKind::Other => {
            if let Some(fixed) = flag_used_as_a_field(input) {
                out.push(fixed);
            }
        }
    }
}

/// `file > 100mb` — a flag where a field belongs.
///
/// The user means "files, bigger than 100mb", which is two conditions. `size`
/// is the field they wanted, because it is the only one anybody compares a flag
/// against by mistake.
fn flag_used_as_a_field(input: &str) -> Option<Suggestion> {
    let mut words = input.split_whitespace();
    // Resolved through the parser's own alias table, so `folder`, `dirs` and
    // `directory` are understood exactly where `dir` is.
    let flag = crate::flag_for_word(words.next()?)?.name();

    let operator = words.next()?;
    if !matches!(operator, ">" | ">=" | "<" | "<=" | "=" | "==") {
        return None;
    }
    let value: Vec<&str> = words.collect();
    if value.is_empty() {
        return None;
    }

    let query = format!("{flag} and size {operator} {}", value.join(" "));
    Query::parse(&query).ok()?;
    Some(Suggestion {
        kind: SuggestionKind::Correction,
        query,
        reason: format!("`{flag}` is a yes-or-no property — did you mean its size?"),
    })
}

/// Ideas for text that parses as a plain name search.
fn plain_text_ideas(input: &str, out: &mut Vec<Suggestion>) {
    let words: Vec<&str> = input.split_whitespace().collect();

    // `size 1gb` — a field and a value with the operator missing. The parser
    // cannot object, because as a name search it is perfectly valid; it simply
    // will not find anything.
    if words.len() == 2 {
        let candidates = help::field_names();
        if let Some(field) = nearest(words[0], &candidates, 1).first().copied()
            && Query::parse(&format!("{field} > {}", words[1])).is_ok()
        {
            out.push(Suggestion {
                kind: SuggestionKind::Interpretation,
                query: format!("{field} > {}", words[1]),
                reason: format!("searching names for \"{input}\" — did you mean this?"),
            });
        }
    }

    // Several words are rarely a filename and often a description of a task.
    if words.len() >= 2 {
        add_examples(input, out, 3);
    }
}

/// Offer catalogue recipes that match the words used.
fn add_examples(input: &str, out: &mut Vec<Suggestion>, limit: usize) {
    let mut added = 0;
    for topic in matching_topics(input) {
        if added >= limit {
            break;
        }
        if out.iter().any(|s| s.query == topic.example) {
            continue;
        }
        out.push(Suggestion {
            kind: SuggestionKind::Example,
            query: topic.example.to_string(),
            reason: topic.summary.to_string(),
        });
        added += 1;
    }
}

/// Topics matching any word in the input, best first.
///
/// Searched word by word rather than as one string, because the input is a
/// phrase — "big old files" should find the recipes for "big" and for "old",
/// which no single-string search would.
fn matching_topics(input: &str) -> Vec<&'static Topic> {
    let mut found: Vec<&'static Topic> = Vec::new();
    for word in input.split_whitespace().filter(|w| w.len() >= 3) {
        for topic in help::search(word, 3) {
            if !found.iter().any(|t| std::ptr::eq(*t, topic)) {
                found.push(topic);
            }
        }
    }
    // Recipes are complete answers; a field is only part of one.
    found.sort_by_key(|topic| topic.kind != crate::TopicKind::Recipe);
    found
}

/// The closest spellings from a list, nearest first.
fn nearest(word: &str, candidates: &[&'static str], limit: usize) -> Vec<&'static str> {
    let lower = word.to_lowercase();
    let mut scored: Vec<(usize, &'static str)> = candidates
        .iter()
        .map(|candidate| (help::edit_distance(&lower, candidate), *candidate))
        // Two edits is a typo; three is a different word.
        .filter(|(distance, candidate)| {
            *distance <= 2 || candidate.starts_with(&lower) || lower.starts_with(*candidate)
        })
        .collect();
    scored.sort_by_key(|(distance, _)| *distance);
    scored.into_iter().take(limit).map(|(_, c)| c).collect()
}

fn replace_span(input: &str, span: &crate::Span, replacement: &str) -> String {
    let mut out = String::with_capacity(input.len() + replacement.len());
    out.push_str(&input[..span.start.min(input.len())]);
    out.push_str(replacement);
    out.push_str(&input[span.end.min(input.len())..]);
    out
}

/// Whatever follows the last mention of `field` in the input.
fn text_after(input: &str, field: &str) -> Option<String> {
    let lower = input.to_lowercase();
    let at = lower.rfind(field)?;
    Some(input[at + field.len()..].trim().to_string())
}

/// The operators worth offering for a field, most likely first.
fn candidate_operators(field: &str) -> &'static [&'static str] {
    match field {
        "name" | "path" => &["~", "=", "startswith", "endswith"],
        "ext" => &["=", "!="],
        "modified" | "created" => &["<", ">"],
        _ => &[">", "<", "="],
    }
}

/// A sensible first comparison for a field, used when one is missing.
fn default_comparison(field: &str) -> &'static str {
    match field {
        "name" | "path" => "~ something",
        "ext" => "= pdf",
        "modified" | "created" => "< 2024-01-01",
        "age" => "> 6mo",
        "depth" => "> 10",
        "items" | "children" => "> 1000",
        _ => "> 100mb",
    }
}

fn example_value(field: &str) -> &'static str {
    match field {
        "name" | "path" => "something",
        "ext" => "pdf",
        "modified" | "created" => "2024-01-01",
        "age" => "6mo",
        "depth" => "10",
        "items" | "children" => "1000",
        _ => "100mb",
    }
}

fn value_noun(field: &str) -> &'static str {
    match field {
        "age" => "a length of time",
        "modified" | "created" => "a date",
        "size" | "apparent" | "wasted" => "a size",
        "owner" | "user" | "account" => "an account name",
        "name" | "path" | "ext" => "some text",
        _ => "a number",
    }
}

/// Salvage the number out of a value whose unit made no sense.
///
/// `10 furlongs` is `10 megabytes` with the wrong word attached, and keeping
/// the number the user typed makes the suggestion feel like a correction rather
/// than a replacement.
fn repair_value(field: &str, raw: &str) -> Option<String> {
    let digits: String = raw
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() || digits == "." {
        return None;
    }
    Some(match field {
        "age" => format!("{digits}d"),
        "size" | "apparent" | "wasted" => format!("{digits}mb"),
        _ => digits,
    })
}

/// Close any brackets or quotes left open, in the order they were opened.
fn close_openings(input: &str) -> String {
    let mut out = input.to_string();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;

    for c in input.chars() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' if quote.is_none() => depth += 1,
            ')' if quote.is_none() => depth -= 1,
            _ => {}
        }
    }
    if let Some(q) = quote {
        out.push(q);
    }
    for _ in 0..depth.max(0) {
        out.push(')');
    }
    out
}

/// The field name a query ends with, if it ends with one.
fn trailing_field(input: &str) -> Option<String> {
    let names = help::field_names();
    input
        .split_whitespace()
        .rev()
        .find(|word| names.contains(&word.to_lowercase().as_str()))
        .map(|word| word.to_lowercase())
}
