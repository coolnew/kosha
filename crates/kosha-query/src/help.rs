// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The searchable catalogue behind the query language's help and suggestions.
//!
//! One list, two consumers. The help panel searches it so a user can find the
//! syntax without leaving the window, and the suggester searches it so a query
//! that went wrong can be answered with something that would work. Keeping them
//! on the same data means the thing suggested is always something documented,
//! and the thing documented is always something that runs.
//!
//! Every entry carries a complete, runnable `example`. That is the point: the
//! fastest way to learn a small language is to paste a working line of it and
//! change one word.
//!
//! Matching is deliberately generous. People do not search documentation with
//! the word the documentation uses — they type "big", "old", "video", "junk" —
//! so each topic carries the words somebody might actually reach for.

use crate::{Field, Flag};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TopicKind {
    /// Something an entry has: `size`, `age`, `name`.
    Field,
    /// Something an entry is: `dir`, `empty`, `sparse`.
    Flag,
    /// How values are compared or combined.
    Operator,
    /// How to write a size, a duration or a date.
    Literal,
    /// A whole query for a task somebody actually has.
    Recipe,
}

impl TopicKind {
    pub fn label(self) -> &'static str {
        match self {
            TopicKind::Field => "field",
            TopicKind::Flag => "flag",
            TopicKind::Operator => "operator",
            TopicKind::Literal => "value",
            TopicKind::Recipe => "recipe",
        }
    }
}

/// One entry in the help.
#[derive(Clone, Copy, Debug)]
pub struct Topic {
    pub kind: TopicKind,
    /// What it is called: `size`, or "Big files nobody opens".
    pub label: &'static str,
    /// One line. No trailing period on fragments.
    pub summary: &'static str,
    /// A complete query. Clicking a topic puts exactly this in the box, so it
    /// has to run as written.
    pub example: &'static str,
    /// Words somebody might search for instead of the label.
    pub keywords: &'static [&'static str],
}

/// Everything the help knows, in the order it reads best.
///
/// Recipes come first on purpose: someone opening the help usually has a task,
/// not a grammar question, and the fastest answer is a line they can run.
pub fn catalogue() -> &'static [Topic] {
    use TopicKind::{Field, Flag, Literal, Operator, Recipe};

    &[
        // ---------------------------------------------------------- recipes
        Topic {
            kind: Recipe,
            label: "Big files",
            summary: "The largest files anywhere in the tree",
            example: "file and size > 1gb",
            keywords: &["big", "large", "huge", "biggest", "space", "hog"],
        },
        Topic {
            kind: Recipe,
            label: "Big files nobody has opened",
            summary: "Large and untouched for a year — the usual candidates",
            example: "file and size > 500mb and age > 1y",
            keywords: &["old", "stale", "forgotten", "unused", "cold", "abandoned"],
        },
        Topic {
            kind: Recipe,
            label: "Videos and other media",
            summary: "Media files, which are usually the largest thing on a disk",
            example: "ext in (mp4, mkv, mov, avi) and size > 200mb",
            keywords: &["video", "movie", "film", "media", "footage"],
        },
        Topic {
            kind: Recipe,
            label: "Photos and images",
            summary: "Image files above a size worth looking at",
            example: "ext in (jpg, jpeg, png, heic, raw, tiff) and size > 10mb",
            keywords: &["photo", "image", "picture", "camera", "jpeg"],
        },
        Topic {
            kind: Recipe,
            label: "Archives and installers",
            summary: "Downloads that can be fetched again if needed",
            example: "ext in (zip, iso, dmg, exe, msi, pkg) and size > 100mb",
            keywords: &[
                "zip",
                "download",
                "installer",
                "archive",
                "setup",
                "package",
            ],
        },
        Topic {
            kind: Recipe,
            label: "Virtual machine images",
            summary: "Disk images, often the single biggest thing a developer has",
            example: "ext in (vdi, vmdk, qcow2, vhdx) and size > 1gb",
            keywords: &["vm", "virtual", "machine", "docker", "disk", "image"],
        },
        Topic {
            kind: Recipe,
            label: "Build output and caches",
            summary: "Directories a tool will happily recreate",
            example: "dir and name in (node_modules, target, build, dist, __pycache__)",
            keywords: &[
                "build",
                "cache",
                "junk",
                "artifact",
                "node_modules",
                "target",
            ],
        },
        Topic {
            kind: Recipe,
            label: "Logs",
            summary: "Log files, which grow quietly and are rarely read",
            example: "ext = log and size > 50mb",
            keywords: &["log", "logging", "trace", "debug"],
        },
        Topic {
            kind: Recipe,
            label: "Directories with far too many files",
            summary: "Where a huge file count is costing you in slack alone",
            example: "dir and items > 50000",
            keywords: &["many", "count", "crowded", "clutter", "slack"],
        },
        Topic {
            kind: Recipe,
            label: "Space lost to small files",
            summary: "Entries whose on-disk size exceeds what they contain",
            example: "wasted > 10mb",
            keywords: &["waste", "wasted", "slack", "overhead", "cluster"],
        },
        Topic {
            kind: Recipe,
            label: "Somebody else's large files",
            summary: "On a shared machine, the big things that are not yours",
            example: "file and owner != me and size > 100mb",
            keywords: &[
                "owner", "user", "account", "shared", "somebody", "else", "theirs", "who",
            ],
        },
        Topic {
            kind: Recipe,
            label: "Everything one account owns",
            summary: "Follow a row of the owners panel back to the files behind it",
            example: "owner = me",
            keywords: &["owner", "user", "account", "mine", "whose", "belongs"],
        },
        Topic {
            kind: Recipe,
            label: "Recently changed",
            summary: "What has moved lately — useful after something grew",
            example: "file and age < 7d and size > 100mb",
            keywords: &["recent", "new", "today", "changed", "modified", "grew"],
        },
        Topic {
            kind: Recipe,
            label: "Everything except a directory",
            summary: "Exclude somewhere noisy from a search",
            example: "file and size > 100mb and not path ~ node_modules",
            keywords: &["exclude", "ignore", "except", "without", "skip"],
        },
        Topic {
            kind: Recipe,
            label: "Empty directories",
            summary: "Directories holding nothing at all",
            example: "empty",
            keywords: &["empty", "blank", "nothing", "unused"],
        },
        // ----------------------------------------------------------- fields
        Topic {
            kind: Field,
            label: "size",
            summary: "Bytes on disk — what you get back by deleting it",
            example: "size > 1gb",
            keywords: &["big", "large", "bytes", "space", "disk", "how much"],
        },
        Topic {
            kind: Field,
            label: "apparent",
            summary: "The size a file claims, which can exceed what it occupies",
            example: "apparent > 1gb",
            keywords: &["logical", "claimed", "reported", "sparse"],
        },
        Topic {
            kind: Field,
            label: "age",
            summary: "Time since it was last modified",
            example: "age > 1y",
            keywords: &["old", "stale", "recent", "when", "time", "untouched"],
        },
        Topic {
            kind: Field,
            label: "modified",
            summary: "The date it was last modified",
            example: "modified < 2024-01-01",
            keywords: &["date", "when", "before", "after", "changed"],
        },
        Topic {
            kind: Field,
            label: "created",
            summary: "Filesystem creation date, when recorded; unknown dates do not match",
            example: "created >= 2026-01-01",
            keywords: &["birth", "creation", "date", "when", "before", "after"],
        },
        Topic {
            kind: Field,
            label: "name",
            summary: "The entry's own name, without any directory part",
            example: "name ~ backup",
            keywords: &["called", "named", "filename", "title"],
        },
        Topic {
            kind: Field,
            label: "path",
            summary: "The full path, for narrowing to or excluding a location",
            example: "path ~ Downloads",
            keywords: &[
                "folder",
                "directory",
                "location",
                "where",
                "under",
                "inside",
            ],
        },
        Topic {
            kind: Field,
            label: "ext",
            summary: "Extension, without the dot",
            example: "ext = pdf",
            keywords: &["extension", "type", "kind", "format", "suffix"],
        },
        Topic {
            kind: Field,
            label: "items",
            summary: "Files anywhere below a directory",
            example: "dir and items > 10000",
            keywords: &["count", "how many", "files", "contents"],
        },
        Topic {
            kind: Field,
            label: "children",
            summary: "Direct children, not counting anything deeper",
            example: "dir and children > 500",
            keywords: &["count", "direct", "immediate", "contents"],
        },
        Topic {
            kind: Field,
            label: "depth",
            summary: "How many levels below the scan root it sits",
            example: "depth > 12",
            keywords: &["nested", "deep", "level", "buried"],
        },
        Topic {
            kind: Field,
            label: "wasted",
            summary: "On-disk bytes beyond the apparent size — cluster slack",
            example: "wasted > 10mb",
            keywords: &["slack", "overhead", "waste", "lost", "cluster"],
        },
        Topic {
            kind: Field,
            label: "owner",
            summary: "The account it belongs to. `me` is the one you are running as",
            example: "owner = me",
            keywords: &[
                "owner", "user", "account", "who", "whose", "mine", "shared", "person",
            ],
        },
        // ------------------------------------------------------------ flags
        Topic {
            kind: Flag,
            label: "file",
            summary: "Only files, excluding directories and links",
            example: "file and size > 500mb",
            keywords: &["files", "not folder", "regular"],
        },
        Topic {
            kind: Flag,
            label: "dir",
            summary: "Only directories, which carry the size of everything below",
            example: "dir and size > 5gb",
            keywords: &["folder", "directory", "dirs", "folders"],
        },
        Topic {
            kind: Flag,
            label: "link",
            summary: "Symbolic links, measured as the link and never followed",
            example: "link",
            keywords: &["symlink", "shortcut", "junction", "alias"],
        },
        Topic {
            kind: Flag,
            label: "empty",
            summary: "Directories with nothing in them",
            example: "empty",
            keywords: &["blank", "nothing", "vacant"],
        },
        Topic {
            kind: Flag,
            label: "sparse",
            summary: "Occupies less than it claims: sparse or compressed",
            example: "sparse and apparent > 1gb",
            keywords: &["compressed", "thin", "virtual"],
        },
        Topic {
            kind: Flag,
            label: "partial",
            summary: "Something below it could not be read, so its total is a floor",
            example: "partial",
            keywords: &["incomplete", "unreadable", "permission", "denied"],
        },
        Topic {
            kind: Flag,
            label: "unreadable",
            summary: "Could not be read at all",
            example: "unreadable",
            keywords: &["permission", "denied", "locked", "access"],
        },
        Topic {
            kind: Flag,
            label: "hardlink",
            summary: "An inode already counted elsewhere in this scan",
            example: "hardlink",
            keywords: &["link", "inode", "duplicate", "shared"],
        },
        // -------------------------------------------------------- operators
        Topic {
            kind: Operator,
            label: "> >= < <=",
            summary: "Compare a number: bigger, smaller, at least, at most",
            example: "size >= 100mb",
            keywords: &["more", "less", "greater", "over", "under", "above", "below"],
        },
        Topic {
            kind: Operator,
            label: "= and !=",
            summary: "Exactly this, or anything but",
            example: "ext = pdf",
            keywords: &["equals", "is", "not", "exactly", "match"],
        },
        Topic {
            kind: Operator,
            label: "~ and !~",
            summary: "Contains, or does not contain — for names and paths",
            example: "name ~ invoice",
            keywords: &["contains", "like", "substring", "includes", "search"],
        },
        Topic {
            kind: Operator,
            label: "startswith and endswith",
            summary: "Literal name/path prefix or suffix, ignoring case; no wildcards",
            example: "name startswith report and name endswith .pdf",
            keywords: &["prefix", "suffix", "starts", "ends", "begin", "finish"],
        },
        Topic {
            kind: Operator,
            label: "in (…)",
            summary: "Any one of several alternatives",
            example: "ext in (mp4, mkv, mov)",
            keywords: &["any", "one of", "list", "either", "several"],
        },
        Topic {
            kind: Operator,
            label: "and or not",
            summary: "Combine conditions. Also spelled && || !",
            example: "file and size > 1gb and not path ~ cache",
            keywords: &["combine", "both", "either", "exclude", "negate"],
        },
        Topic {
            kind: Operator,
            label: "( … )",
            summary: "Group conditions so `or` binds where you mean it to",
            example: "(ext = mp4 or ext = mkv) and size > 500mb",
            keywords: &["group", "bracket", "parenthesis", "precedence"],
        },
        // --------------------------------------------------------- literals
        Topic {
            kind: Literal,
            label: "Sizes",
            summary: "100, 10k, 1.5mb, 2gb, 500mib — powers of 1024",
            example: "size > 1.5gb",
            keywords: &["kb", "mb", "gb", "tb", "bytes", "megabyte", "gigabyte"],
        },
        Topic {
            kind: Literal,
            label: "Durations",
            summary: "30d, 6mo, 2y, 12h, 1w. A bare number means days",
            example: "age > 6mo",
            keywords: &["day", "week", "month", "year", "hour", "time", "ago"],
        },
        Topic {
            kind: Literal,
            label: "Dates",
            summary: "Written as year-month-day",
            example: "modified < 2024-06-01",
            keywords: &["date", "calendar", "before", "since", "after"],
        },
        Topic {
            kind: Literal,
            label: "Exact timestamps",
            summary: "@ followed by Unix seconds, independent of timezone",
            example: "created >= @1794398400 and created <= @1795003200",
            keywords: &["date", "instant", "epoch", "unix", "timestamp", "rolling"],
        },
        Topic {
            kind: Literal,
            label: "Quoted text",
            summary: "Quote anything containing spaces or a keyword",
            example: "name ~ \"annual report\"",
            keywords: &["quote", "space", "phrase", "literal", "exact"],
        },
    ]
}

/// Search the catalogue.
///
/// Returns the best matches, most relevant first. An empty needle returns the
/// catalogue in its own order, which is how the panel looks before anyone types.
pub fn search(needle: &str, limit: usize) -> Vec<&'static Topic> {
    let needle = needle.trim();
    if needle.is_empty() {
        return catalogue().iter().take(limit).collect();
    }

    let mut scored: Vec<(u32, usize, &'static Topic)> = catalogue()
        .iter()
        .enumerate()
        .filter_map(|(index, topic)| score(topic, needle).map(|s| (s, index, topic)))
        .collect();

    // Score first, then catalogue order, so equally good matches stay in the
    // order somebody reading the list would expect.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, _, t)| t).collect()
}

/// How well a topic answers this search, if at all.
///
/// The label is worth most, then the words somebody might reach for instead,
/// then the example, then the prose. A topic that only matches its summary is
/// still worth showing, but below one whose name was typed.
fn score(topic: &Topic, needle: &str) -> Option<u32> {
    let mut best = None;
    let mut consider = |text: &str, weight: u32| {
        if let Some(score) = match_score(text, needle) {
            let scaled = score.saturating_add(weight);
            best = Some(best.map_or(scaled, |current: u32| current.max(scaled)));
        }
    };

    consider(topic.label, 400);
    for keyword in topic.keywords {
        consider(keyword, 300);
    }
    consider(topic.example, 100);
    consider(topic.summary, 0);
    best
}

/// Score one string against a needle, or `None` if it does not match at all.
///
/// Public because the command palette ranks with it too. Looking something up
/// by a word other than its name is one question, and answering it two
/// different ways in two parts of the same window would mean `⌘K` and the help
/// panel disagreeing about what "dupes" finds.
pub fn match_score(haystack: &str, needle: &str) -> Option<u32> {
    let hay = haystack.to_lowercase();
    let need = needle.to_lowercase();

    if hay == need {
        return Some(1000);
    }
    if hay.starts_with(&need) {
        return Some(900 - (hay.len().saturating_sub(need.len()) as u32).min(200));
    }
    if let Some(at) = hay.find(&need) {
        return Some(700 - (at as u32).min(200));
    }
    // Word-initial match: "how much" is found by "hm", and "node modules" by
    // "nm", which is how people abbreviate when they are in a hurry.
    if initials(&hay).starts_with(&need) {
        return Some(600);
    }
    if is_subsequence(&hay, &need) {
        return Some(450);
    }
    // A typo in a short word is still that word.
    if need.len() >= 3 {
        let distance = edit_distance(&hay, &need);
        if distance <= 2 {
            return Some(400 - (distance as u32) * 80);
        }
        // Or a typo in one of its words.
        for word in hay.split(|c: char| !c.is_alphanumeric()) {
            if word.len() >= 3 && edit_distance(word, &need) <= 1 {
                return Some(350);
            }
        }
    }
    None
}

fn initials(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(|word| word.chars().next())
        .collect()
}

fn is_subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|c| chars.any(|h| h == c))
}

/// Levenshtein distance, two rows at a time.
///
/// Shared with the parser, which uses it to suggest a field name when one is
/// misspelled — the same question in a different place.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0usize; b_chars.len() + 1];

    for (i, ac) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, bc) in b_chars.iter().enumerate() {
            let cost = usize::from(ac != *bc);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b_chars.len()]
}

/// Every field name the language accepts, for suggestions and completion.
pub fn field_names() -> Vec<&'static str> {
    [
        Field::Name,
        Field::Path,
        Field::Ext,
        Field::Size,
        Field::Apparent,
        Field::Items,
        Field::Children,
        Field::Depth,
        Field::Age,
        Field::Modified,
        Field::Created,
        Field::Wasted,
        Field::Owner,
    ]
    .iter()
    .map(|field| field.name())
    .collect()
}

/// Every flag name the language accepts.
pub fn flag_names() -> Vec<&'static str> {
    [
        Flag::Dir,
        Flag::File,
        Flag::Link,
        Flag::Sparse,
        Flag::Partial,
        Flag::Hardlink,
        Flag::Unreadable,
        Flag::Empty,
    ]
    .iter()
    .map(|flag| flag.name())
    .collect()
}
