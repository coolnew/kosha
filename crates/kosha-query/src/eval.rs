// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Running a parsed query against the arena.
//!
//! Everything here is a read of a field already present on a `Node`, except
//! `path`, which is rebuilt from parent pointers. That is the whole reason
//! `Query` records up front whether a path is needed: the walk is `O(depth)`
//! with an allocation, and doing it per node for a query that never mentions
//! paths would cost more than the rest of the evaluation put together.

use std::borrow::Cow;
use std::sync::OnceLock;

use kosha_core::{Flags, NodeId, SizeTree};

use crate::{Expr, Field, Flag, Op, Value};

pub(crate) fn matches(
    expr: &Expr,
    tree: &SizeTree,
    id: NodeId,
    now: u32,
    needs_path: bool,
) -> bool {
    // Built once per node rather than once per comparison, and only when the
    // query actually asks about it.
    let path = needs_path.then(|| tree.path_of(id).to_string_lossy().into_owned());
    evaluate(expr, tree, id, now, path.as_deref()) == Some(true)
}

// Unknown birth times propagate through NOT. AND/OR retain short-circuit
// three-valued logic: false AND unknown is false; true OR unknown is true.
fn evaluate(
    expr: &Expr,
    tree: &SizeTree,
    id: NodeId,
    now: u32,
    path: Option<&str>,
) -> Option<bool> {
    match expr {
        Expr::And(a, b) => match evaluate(a, tree, id, now, path) {
            Some(false) => Some(false),
            Some(true) => evaluate(b, tree, id, now, path),
            None => match evaluate(b, tree, id, now, path) {
                Some(false) => Some(false),
                _ => None,
            },
        },
        Expr::Or(a, b) => match evaluate(a, tree, id, now, path) {
            Some(true) => Some(true),
            Some(false) => evaluate(b, tree, id, now, path),
            None => match evaluate(b, tree, id, now, path) {
                Some(true) => Some(true),
                _ => None,
            },
        },
        Expr::Not(inner) => evaluate(inner, tree, id, now, path).map(|value| !value),
        Expr::Flag(Flag::Empty) if unknown_empty_directory(tree, id) => None,
        Expr::Flag(flag) => Some(has_flag(tree, id, *flag)),
        Expr::NameContains(text) => Some(contains_ignoring_case(&tree.name_lossy(id), text)),
        Expr::Compare { field, op, value } => {
            if unknown_value(*field, tree, id) {
                None
            } else {
                Some(compare(tree, id, now, path, *field, *op, value))
            }
        }
        Expr::In {
            field,
            values,
            negated,
        } => {
            if unknown_value(*field, tree, id) {
                return None;
            }
            // The field is read once and compared against every alternative,
            // rather than re-read per alternative. `ext in (a, b, c)` over a
            // few hundred thousand entries is three field extractions per node
            // the naive way, and extraction is the expensive half.
            let any = if *field == Field::Owner {
                // Not through `text_field`: an owner is not a field of the
                // node, and the two-spellings rule lives in `compare_owner`.
                values.iter().any(|value| match value {
                    Value::Text(wanted) => compare_owner(tree, id, Op::Eq, wanted),
                    Value::Number(_) => false,
                })
            } else if field.is_text() {
                let actual = text_field(tree, id, path, *field);
                values.iter().any(|value| match value {
                    Value::Text(wanted) => compare_text(&actual, Op::Eq, wanted),
                    Value::Number(_) => false,
                })
            } else {
                let actual = number_field(tree, id, now, *field);
                values.iter().any(|value| match value {
                    Value::Number(wanted) => compare_number(actual, Op::Eq, *wanted),
                    Value::Text(_) => false,
                })
            };
            Some(any != *negated)
        }
    }
}

fn unknown_value(field: Field, tree: &SizeTree, id: NodeId) -> bool {
    match field {
        Field::Created => tree.created(id).is_none(),
        // mtime zero is the existing scanner's unavailable-date sentinel.
        Field::Modified | Field::Age => tree.node(id).mtime == 0,
        // Failed file metadata is encoded as zeros, not proof of zero bytes.
        Field::Size | Field::Apparent | Field::Wasted => {
            let node = tree.node(id);
            !node.is_dir() && node.flags.contains(Flags::UNREADABLE)
        }
        _ => false,
    }
}

fn unknown_empty_directory(tree: &SizeTree, id: NodeId) -> bool {
    let node = tree.node(id);
    node.is_dir()
        && node.child_count == 0
        && (tree.is_incomplete()
            || node.is_partial()
            || node.flags.contains(Flags::UNREADABLE)
            || node.flags.contains(Flags::OTHER_DEVICE))
}

fn has_flag(tree: &SizeTree, id: NodeId, flag: Flag) -> bool {
    let node = tree.node(id);
    match flag {
        // A symlink is reported as what it is, never as the directory it points
        // at, so `dir` and `link` are mutually exclusive.
        Flag::Dir => node.is_dir() && !node.is_symlink(),
        Flag::File => !node.is_dir() && !node.is_symlink(),
        Flag::Link => node.is_symlink(),
        Flag::Sparse => node.flags.contains(Flags::SPARSE),
        Flag::Partial => node.is_partial(),
        Flag::Hardlink => node.flags.contains(Flags::HARDLINK_DUP),
        Flag::Unreadable => node.flags.contains(Flags::UNREADABLE),
        Flag::Empty => node.is_dir() && node.child_count == 0,
    }
}

fn compare(
    tree: &SizeTree,
    id: NodeId,
    now: u32,
    path: Option<&str>,
    field: Field,
    op: Op,
    value: &Value,
) -> bool {
    if field == Field::Created && tree.created(id).is_none() {
        return false;
    }
    if field == Field::Owner {
        let Value::Text(wanted) = value else {
            return false;
        };
        return compare_owner(tree, id, op, wanted);
    }

    if field.is_text() {
        let Value::Text(wanted) = value else {
            return false;
        };
        // A substring with no separator in it cannot span one either, so
        // "does this path contain `node_modules`" is exactly "does this entry
        // or any ancestor have a name containing it". Answering it by walking
        // parent pointers costs no allocation, where building the path costs
        // two per node — and this is the single most common path query there
        // is.
        if field == Field::Path
            && crate::needle_is_separator_free(wanted)
            && matches!(op, Op::Contains | Op::NotContains)
        {
            let found = path_component_contains(tree, id, wanted);
            return match op {
                Op::Contains => found,
                Op::NotContains => !found,
                _ => compare_text(&text_field(tree, id, path, field), op, wanted),
            };
        }
        let actual = text_field(tree, id, path, field);
        if field == Field::Path {
            return compare_path_text(&actual, op, wanted);
        }
        return compare_text(&actual, op, wanted);
    }

    let Value::Number(wanted) = value else {
        return false;
    };
    let actual = number_field(tree, id, now, field);
    compare_number(actual, op, *wanted)
}

fn text_field<'a>(
    tree: &'a SizeTree,
    id: NodeId,
    path: Option<&'a str>,
    field: Field,
) -> Cow<'a, str> {
    match field {
        Field::Name => tree.name_lossy(id),
        // `needs_path` guarantees this is present whenever a query reaches
        // here; an empty string is the safe reading if it somehow is not,
        // since it matches nothing rather than everything.
        Field::Path => Cow::Borrowed(path.unwrap_or("")),
        Field::Ext => {
            // Returned as it is written rather than lowercased, because the
            // comparison is already case-insensitive and lowercasing would mean
            // an allocation for every entry in the tree. `name_lossy` borrows
            // for any name that is valid UTF-8, which is nearly all of them, so
            // the common path allocates nothing at all.
            match tree.name_lossy(id) {
                Cow::Borrowed(name) => Cow::Borrowed(extension_of(name)),
                Cow::Owned(name) => Cow::Owned(extension_of(&name).to_string()),
            }
        }
        _ => Cow::Borrowed(""),
    }
}

/// Whether this entry, or anything above it, has a name containing `needle`.
///
/// `O(depth)` with no allocation, against `O(depth)` plus two allocations for
/// building the path and lowercasing it.
fn path_component_contains(tree: &SizeTree, id: NodeId, needle: &str) -> bool {
    let mut cursor = id;
    loop {
        if contains_ignoring_case(&tree.name_lossy(cursor), needle) {
            return true;
        }
        if cursor == tree.root() {
            break;
        }
        cursor = NodeId::from_index(tree.node(cursor).parent as usize);
    }
    // The root node's *name* is only its last component, so the directories
    // above the scan root have to be checked separately. `to_string_lossy`
    // borrows for a valid-UTF-8 path, which is the ordinary case.
    contains_ignoring_case(&tree.root_path().to_string_lossy(), needle)
}

/// The extension of a file name, without the dot.
///
/// A leading dot is the whole name, not an extension: `.gitignore` has no
/// extension, and treating it as one would put every dotfile in the wrong
/// bucket and make `ext = gitignore` find something.
fn extension_of(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext,
        _ => "",
    }
}

fn number_field(tree: &SizeTree, id: NodeId, now: u32, field: Field) -> u64 {
    let node = tree.node(id);
    match field {
        Field::Size => node.allocated,
        Field::Apparent => node.logical,
        Field::Items => u64::from(node.subtree_files),
        Field::Children => u64::from(node.child_count),
        Field::Depth => u64::from(node.depth),
        Field::Wasted => node.wasted(),
        // An unknown timestamp reads as age zero rather than as enormously old,
        // so `age > 1y` does not sweep up everything we could not date.
        Field::Age => {
            if node.mtime == 0 {
                0
            } else {
                u64::from(now.saturating_sub(node.mtime))
            }
        }
        Field::Modified => u64::from(node.mtime),
        Field::Created => tree.created(id).unwrap_or(0),
        _ => 0,
    }
}

fn compare_number(actual: u64, op: Op, wanted: u64) -> bool {
    match op {
        Op::Lt => actual < wanted,
        Op::Le => actual <= wanted,
        Op::Gt => actual > wanted,
        Op::Ge => actual >= wanted,
        Op::Eq => actual == wanted,
        Op::Ne => actual != wanted,
        // Rejected at parse time for numeric fields; false is the honest
        // answer if one ever reaches here.
        Op::Contains | Op::NotContains | Op::StartsWith | Op::EndsWith => false,
    }
}

/// Compare an entry's owner, which is the one field that is not on the node.
///
/// Three cases, and telling them apart is the whole of it:
///
/// * **The scan collected no owners.** Every comparison is false, negated ones
///   included. `owner != me` returning the whole tree would be an answer to a
///   question this scan cannot answer, and a filter that is confidently wrong is
///   worse than one that returns nothing — see [`Query::needs_owners`], which
///   is how the interface says so rather than leaving it to be inferred.
/// * **The owner could not be read.** An empty label, so `=` is false and `!=`
///   is true. "Everything not mine" should include the entries nobody could put
///   a name to: they are candidates, and leaving them out of a negative filter
///   would hide them.
/// * **There is an owner.** Compared against the full label *and* against the
///   account name without its domain, because `EXAMPLE-PC\alice` is not
///   a string anybody wants to type and the domain is the same for every local
///   account anyway.
///
/// [`Query::needs_owners`]: crate::Query::needs_owners
fn compare_owner(tree: &SizeTree, id: NodeId, op: Op, wanted: &str) -> bool {
    if !tree.owners().were_collected() {
        return false;
    }
    let wanted = resolve_me(wanted);
    let Some(owner) = tree.owners().of(id) else {
        // An unreadable owner is an empty label: `=` is false against every
        // identity, `!=` is true against all of them.
        return match op {
            Op::Ne | Op::NotContains => wanted.iter().all(|w| compare_text("", op, w)),
            _ => wanted.iter().any(|w| compare_text("", op, w)),
        };
    };

    // Two spellings of the same account. A positive operator matches if either
    // does; a negative one has to fail against both, which is what makes
    // `owner != alice` exclude `EXAMPLE-PC\alice` as well.
    let label = owner.label();
    let account = owner.account();
    // A positive matches on either spelling of any identity; a negative has to
    // miss every spelling of every one of them. With one identity — which is
    // every query but `me`, and `me` on Unix — this is exactly what it was.
    let hit = |wanted: &str| compare_text(&label, op, wanted) || compare_text(&account, op, wanted);
    let miss =
        |wanted: &str| compare_text(&label, op, wanted) && compare_text(&account, op, wanted);
    match op {
        Op::Ne | Op::NotContains => wanted.iter().all(|w| miss(w)),
        _ => wanted.iter().any(|w| hit(w)),
    }
}

/// The accounts `me` stands for.
///
/// The alternative is making somebody look up the spelling of their own account
/// before they can ask about it, which on Windows means finding out their
/// machine's name. Resolved once per process rather than per node: it involves
/// a token query and an account lookup, and it cannot change while the process
/// is alive.
///
/// **More than one of them on Windows.** The token's user is who the process is;
/// the token's owner is what gets stamped on the files it creates, and a Windows
/// policy makes those different — where it is set, an elevated administrator's
/// files are owned by `BUILTIN\\Administrators`. Measured on a clean Windows:
/// `owner = me` matched nothing in a directory this process had just filled,
/// because it was asking about the wrong one of the two. See
/// [`kosha_core::current_owners`].
///
/// An account genuinely called `me` is not a case this designs for. Somebody in
/// that position writes the qualified name.
fn resolve_me<'a>(wanted: &'a str) -> Cow<'a, [String]> {
    if !wanted.eq_ignore_ascii_case("me") {
        return Cow::Owned(vec![wanted.to_string()]);
    }
    static ME: OnceLock<Vec<String>> = OnceLock::new();
    let names = ME.get_or_init(|| {
        kosha_core::current_owners()
            .iter()
            .map(|owner| owner.label().into_owned())
            .collect()
    });
    if names.is_empty() {
        // The machine would not say who we are. Falling back to the literal
        // matches an account actually called `me` and nothing else, which is a
        // better answer than matching everything.
        return Cow::Owned(vec![wanted.to_string()]);
    }
    Cow::Borrowed(names)
}

fn compare_text(actual: &str, op: Op, wanted: &str) -> bool {
    match op {
        Op::Eq => actual.eq_ignore_ascii_case(wanted),
        Op::Ne => !actual.eq_ignore_ascii_case(wanted),
        Op::Contains => contains_ignoring_case(actual, wanted),
        Op::NotContains => !contains_ignoring_case(actual, wanted),
        Op::StartsWith | Op::EndsWith => {
            if actual.is_ascii() && wanted.is_ascii() {
                let edge = if op == Op::StartsWith {
                    actual.get(..wanted.len())
                } else {
                    actual
                        .len()
                        .checked_sub(wanted.len())
                        .and_then(|start| actual.get(start..))
                };
                edge.is_some_and(|text| text.eq_ignore_ascii_case(wanted))
            } else {
                let (actual, wanted) = (actual.to_lowercase(), wanted.to_lowercase());
                if op == Op::StartsWith {
                    actual.starts_with(&wanted)
                } else {
                    actual.ends_with(&wanted)
                }
            }
        }
        // Ordering text by `<` invites a comparison nobody means. Equality and
        // containment are the two questions worth asking about a name.
        Op::Lt | Op::Le | Op::Gt | Op::Ge => false,
    }
}

#[cfg(not(windows))]
fn compare_path_text(actual: &str, op: Op, wanted: &str) -> bool {
    // A backslash can be part of a Unix filename, never a separator there.
    compare_text(actual, op, wanted)
}

#[cfg(windows)]
fn compare_path_text(actual: &str, op: Op, wanted: &str) -> bool {
    if actual.is_ascii() && wanted.is_ascii() {
        let fold = |byte: u8| {
            if byte == b'\\' {
                b'/'
            } else {
                byte.to_ascii_lowercase()
            }
        };
        let equal = |a: &[u8], b: &[u8]| {
            a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| fold(a) == fold(b))
        };
        let (a, b) = (actual.as_bytes(), wanted.as_bytes());
        return match op {
            Op::Eq => equal(a, b),
            Op::Ne => !equal(a, b),
            Op::Contains | Op::NotContains => {
                let found = b.is_empty() || a.windows(b.len()).any(|window| equal(window, b));
                found != (op == Op::NotContains)
            }
            Op::StartsWith => a.get(..b.len()).is_some_and(|edge| equal(edge, b)),
            Op::EndsWith => a
                .len()
                .checked_sub(b.len())
                .and_then(|at| a.get(at..))
                .is_some_and(|edge| equal(edge, b)),
            _ => false,
        };
    }
    // Only Unicode paths need allocated normalization; the ordinary ASCII
    // path comparison above stays allocation-free after the path is built.
    compare_text(&actual.replace('\\', "/"), op, &wanted.replace('\\', "/"))
}

/// Case-insensitive substring test.
///
/// Allocates only when the haystack contains non-ASCII uppercase, which is the
/// rare case; filenames are overwhelmingly ASCII and this runs per node.
fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        return contains_ascii_ignoring_case(haystack.as_bytes(), needle.as_bytes());
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn contains_ascii_ignoring_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}
