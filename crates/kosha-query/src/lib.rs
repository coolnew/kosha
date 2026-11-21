// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! A query language for a scanned tree.
//!
//! ```text
//! size > 1gb
//! ext in (mp4, mkv, mov) and size > 500mb
//! name ~ backup and modified < 2024-01-01
//! dir and items > 10000
//! age > 1y and size > 100mb and not path ~ node_modules
//! wasted > 100mb
//! ```
//!
//! # Why this is cheap
//!
//! A [`SizeTree`] is a flat array of 48-byte records, so ordinary predicates
//! read contiguous metadata without building a parallel object tree. Costs
//! depend on the predicate and result collection; full-tree execution belongs
//! on a worker. `kosha-search` adds bounded results and extension postings while
//! retaining this evaluator as the source of query semantics.
//!
//! Full paths are the one expensive field, because they are rebuilt from parent
//! pointers rather than stored. A query is compiled knowing whether it mentions
//! `path` at all, and only pays for it when it does.
//!
//! # Being wrong is not an option
//!
//! A query is a filter, and a filter that silently drops matches is worse than
//! no filter: the user concludes the files are not there. So parsing is strict
//! and reports the exact position of a problem, rather than guessing at intent
//! and quietly returning the wrong set.
//!
//! The one deliberate exception is bare text. A query with no operators in it —
//! `node_modules`, `.mp4` — is a substring match on the name, so the simple
//! case stays simple and the syntax only appears when it is asked for.

// `ParseError` carries the message, the offending span, a hint, and the source
// text, because all four are what make an error worth reading. That puts it
// over clippy's size threshold for an `Err` variant.
//
// Boxing it would shrink the `Result` and make every construction site and
// every caller worse, to save copying a struct once per parse — and parsing
// happens on a keystroke, not in a loop over four hundred thousand nodes. The
// lint is right in general and wrong here.
#![allow(clippy::result_large_err)]

use kosha_core::{NodeId, SizeTree};

mod eval;
pub mod help;
mod suggest;
mod syntax;

pub use help::{Topic, TopicKind};
pub use suggest::{Suggestion, SuggestionKind, suggest};
pub use syntax::{ErrorKind, ParseError, Span, field_for_word, flag_for_word};

/// A field of an entry that a query can test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    /// The entry's own name, without any directory part.
    Name,
    /// The full path. The only field that costs anything to read.
    Path,
    /// Lowercased extension, without the dot. Empty when there is none.
    Ext,
    /// Bytes on disk. What you get back by deleting it.
    Size,
    /// Apparent size — what the file claims.
    Apparent,
    /// Files anywhere below this entry.
    Items,
    /// Direct children.
    Children,
    /// Levels below the scan root.
    Depth,
    /// How long since it was modified.
    Age,
    /// When it was modified, as a date.
    Modified,
    /// Filesystem birth time, when recorded; never Unix inode change time.
    Created,
    /// On-disk bytes beyond the apparent size — cluster slack.
    Wasted,
    /// The account that owns the entry, where the scan recorded one.
    ///
    /// Unlike every other field this is not read from the node: owners live in
    /// a side table beside the arena, and a scan that was not asked for them
    /// has none. A query mentioning it on such a scan matches nothing, which is
    /// why [`Query::needs_owners`] exists for the interface to say so.
    Owner,
}

impl Field {
    /// Whether reading this field requires rebuilding a path.
    fn is_expensive(self) -> bool {
        matches!(self, Field::Path)
    }

    /// Fields compared as text rather than as numbers.
    fn is_text(self) -> bool {
        matches!(self, Field::Name | Field::Path | Field::Ext | Field::Owner)
    }

    /// The canonical spelling, which is what a suggestion offers.
    pub fn name(self) -> &'static str {
        match self {
            Field::Name => "name",
            Field::Path => "path",
            Field::Ext => "ext",
            Field::Size => "size",
            Field::Apparent => "apparent",
            Field::Items => "items",
            Field::Children => "children",
            Field::Depth => "depth",
            Field::Age => "age",
            Field::Modified => "modified",
            Field::Created => "created",
            Field::Wasted => "wasted",
            Field::Owner => "owner",
        }
    }
}

/// A property an entry either has or does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flag {
    Dir,
    File,
    Link,
    /// Occupies less than it claims: sparse or transparently compressed.
    Sparse,
    /// Something below it could not be read, so its total is a floor.
    Partial,
    /// An inode already counted elsewhere in this scan.
    Hardlink,
    /// Could not be read at all.
    Unreadable,
    /// A directory with nothing in it.
    Empty,
}

impl Flag {
    /// The canonical spelling, which is what a suggestion offers.
    pub fn name(self) -> &'static str {
        match self {
            Flag::Dir => "dir",
            Flag::File => "file",
            Flag::Link => "link",
            Flag::Sparse => "sparse",
            Flag::Partial => "partial",
            Flag::Hardlink => "hardlink",
            Flag::Unreadable => "unreadable",
            Flag::Empty => "empty",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    /// Case-insensitive substring.
    Contains,
    NotContains,
    /// Case-insensitive literal prefix/suffix; no wildcard interpretation.
    StartsWith,
    EndsWith,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    /// Bytes, seconds, a count, or a Unix timestamp — the field decides.
    Number(u64),
    Text(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Compare {
        field: Field,
        op: Op,
        value: Value,
    },
    /// `ext in (mp4, mkv)`, and its negation.
    In {
        field: Field,
        values: Vec<Value>,
        negated: bool,
    },
    Flag(Flag),
    /// Bare text: a substring match on the name.
    NameContains(String),
}

/// A parsed, ready-to-run query.
#[derive(Clone, Debug)]
pub struct Query {
    expr: Expr,
    /// Precomputed so evaluation never asks the question per node.
    needs_path: bool,
    /// Whether this query asks about an owner, so a caller can notice that the
    /// scan it is running against did not record any.
    needs_owners: bool,
    source: String,
}

impl Query {
    /// Compile a resolved expression without reading the clock or timezone.
    pub fn from_expression(expr: Expr, source: impl Into<String>) -> Self {
        Self {
            needs_path: mentions_path(&expr),
            needs_owners: mentions_owner(&expr),
            expr,
            source: source.into(),
        }
    }

    /// Parse a query, or explain exactly what is wrong with it.
    pub fn parse(input: &str) -> Result<Query, ParseError> {
        let expr = syntax::parse(input)?;
        Ok(Query {
            needs_path: mentions_path(&expr),
            needs_owners: mentions_owner(&expr),
            expr,
            source: input.to_string(),
        })
    }

    /// The text this was parsed from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// True when this is nothing more than a substring match, which is what an
    /// ordinary filter box types. Callers use it to keep the simple case simple.
    pub fn is_plain_text(&self) -> bool {
        matches!(self.expr, Expr::NameContains(_))
    }

    /// Whether evaluating this needs a path rebuilt for every candidate.
    pub fn needs_path(&self) -> bool {
        self.needs_path
    }

    /// Whether this query asks about who owns something.
    ///
    /// A scan that did not collect owners answers every such query with
    /// nothing, and "no results" is indistinguishable from "nobody matched".
    /// The interface uses this to tell the two apart.
    pub fn needs_owners(&self) -> bool {
        self.needs_owners
    }

    pub fn expression(&self) -> &Expr {
        &self.expr
    }

    /// Test one entry.
    pub fn matches(&self, tree: &SizeTree, id: NodeId, now: u32) -> bool {
        eval::matches(&self.expr, tree, id, now, self.needs_path)
    }

    /// Evaluate recorded metadata with an explicit logical path, e.g. a portable
    /// view alias. The path is data only; no filesystem resolution takes place.
    pub fn matches_with_path(&self, tree: &SizeTree, id: NodeId, now: u32, path: &str) -> bool {
        eval::matches_with_path(&self.expr, tree, id, now, path)
    }

    /// Run the query: what matched, how much of it there is, and the biggest.
    ///
    /// One pass over the arena. Asking for the matches and the total separately
    /// would evaluate the predicate twice, which for a path query over four
    /// hundred thousand entries is the difference between 95 ms and 195 ms.
    pub fn run(&self, tree: &SizeTree, now: u32, limit: usize) -> Results {
        // The root is every query's trivial ancestor and never a useful answer
        // to "find me the things that…".
        let matched: Vec<NodeId> = (0..tree.len())
            .map(NodeId::from_index)
            .filter(|&id| id != tree.root() && self.matches(tree, id, now))
            .collect();

        let lookup: rustc_hash::FxHashSet<NodeId> = matched.iter().copied().collect();
        let total_allocated = matched
            .iter()
            .filter(|&&id| !has_matched_ancestor(tree, id, &lookup))
            .map(|&id| tree.node(id).allocated)
            .fold(0u64, u64::saturating_add);

        let mut shown = matched;
        let total_matched = shown.len();
        shown.sort_unstable_by_key(|&id| {
            (
                std::cmp::Reverse(tree.node(id).allocated),
                std::cmp::Reverse(tree.node(id).logical),
                id,
            )
        });
        shown.truncate(limit);

        Results {
            matches: shown,
            total_matched,
            total_allocated,
        }
    }

    /// Every entry that matches, largest on disk first.
    ///
    /// `limit` caps the result, applied after ordering, so the biggest matches
    /// survive rather than whichever happened to be scanned first.
    pub fn search(&self, tree: &SizeTree, now: u32, limit: usize) -> Vec<NodeId> {
        self.run(tree, now, limit).matches
    }

    /// Total on-disk bytes across everything matching.
    ///
    /// Counts only entries with no matching ancestor, so a query that hits both
    /// a directory and its contents does not add the same bytes twice.
    pub fn total_allocated(&self, tree: &SizeTree, now: u32) -> u64 {
        self.run(tree, now, 0).total_allocated
    }
}

/// What a query found.
#[derive(Clone, Debug, Default)]
pub struct Results {
    /// The biggest matches, largest on disk first, capped by the caller.
    pub matches: Vec<NodeId>,
    /// How many entries matched in total, before the cap.
    pub total_matched: usize,
    /// On-disk bytes across everything matching, counting nested matches once.
    pub total_allocated: u64,
}

fn has_matched_ancestor(
    tree: &SizeTree,
    id: NodeId,
    matched: &rustc_hash::FxHashSet<NodeId>,
) -> bool {
    let mut cursor = id;
    while cursor != tree.root() {
        cursor = NodeId::from_index(tree.node(cursor).parent as usize);
        if matched.contains(&cursor) {
            return true;
        }
    }
    false
}

/// A substring that cannot span a path separator, and so can be answered by
/// looking at one name at a time.
pub(crate) fn needle_is_separator_free(needle: &str) -> bool {
    !needle.contains('/') && !needle.contains('\\')
}

/// Whether this query asks about who owns something.
///
/// Plainer than [`mentions_path`], which has an optimisation folded into it:
/// there is no cheap half of an owner comparison, so mentioning the field at
/// all is the whole question.
fn mentions_owner(expr: &Expr) -> bool {
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => mentions_owner(a) || mentions_owner(b),
        Expr::Not(inner) => mentions_owner(inner),
        Expr::Compare { field, .. } | Expr::In { field, .. } => *field == Field::Owner,
        Expr::Flag(_) | Expr::NameContains(_) => false,
    }
}

/// Whether evaluating this needs a full path built for every node.
///
/// Mentioning `path` is not enough. The common case — `path ~ something` with
/// no separator in it — is answered by walking parent pointers, so it costs
/// nothing extra. Only a comparison that genuinely needs the assembled string
/// makes this true.
fn mentions_path(expr: &Expr) -> bool {
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => mentions_path(a) || mentions_path(b),
        Expr::Not(inner) => mentions_path(inner),
        Expr::Compare { field, op, value } => {
            if !field.is_expensive() {
                return false;
            }
            match (op, value) {
                (Op::Contains | Op::NotContains, Value::Text(needle)) => {
                    !needle_is_separator_free(needle)
                }
                _ => true,
            }
        }
        Expr::In { field, values, .. } => {
            field.is_expensive()
                && values.iter().any(|value| match value {
                    Value::Text(needle) => !needle_is_separator_free(needle),
                    Value::Number(_) => true,
                })
        }
        Expr::Flag(_) | Expr::NameContains(_) => false,
    }
}

/// The fields, flags and forms a query can use, for help text and completion.
///
/// Kept beside the parser so a new field cannot be added without the help
/// noticing; `every_field_is_documented` asserts it.
pub fn reference() -> Vec<(&'static str, &'static str)> {
    vec![
        ("name", "the entry's own name"),
        ("path", "the full path"),
        ("ext", "extension, without the dot"),
        ("size", "bytes on disk"),
        ("apparent", "size the file claims"),
        ("items", "files below a directory"),
        ("children", "direct children"),
        ("depth", "levels below the scan root"),
        ("age", "time since last modified"),
        ("modified", "date last modified"),
        ("created", "filesystem creation date, when recorded"),
        ("wasted", "on-disk bytes beyond the apparent size"),
        ("owner", "the account that owns it, or `me`"),
        ("dir, file, link", "what kind of entry it is"),
        ("empty", "a directory with nothing in it"),
        ("sparse", "occupies less than it claims"),
        ("partial", "something below it was unreadable"),
        ("hardlink", "already counted elsewhere"),
        ("unreadable", "could not be read"),
    ]
}
