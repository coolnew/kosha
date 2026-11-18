// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Everything worth saying about one entry, gathered in one place.
//!
//! This is what `Space` shows. It is deliberately *not* a file preview: kosha
//! is an answer tool for "where did my disk go", and a thumbnail of a holiday
//! photograph answers a different question — one a file manager already answers
//! better, and one the roadmap says this program is not going to grow into.
//! What it cannot get anywhere else is why a particular entry costs what it
//! costs, so that is what this says.
//!
//! Pure arithmetic over the arena, with no I/O at all: everything here was
//! already read during the scan. That is what makes `Space` instant, and it is
//! also what makes the whole of it testable without a window.

use kosha_core::{Flags, NodeId, SizeTree, format};

use crate::theme::Category;

/// How many extensions the breakdown names before pooling the rest.
///
/// Five fits the panel and is about where the tail stops being informative: a
/// directory whose sixth-largest extension matters is one to drill into rather
/// than summarise.
const TOP_EXTENSIONS: usize = 5;

/// A caveat about the numbers beside it.
///
/// Each is a reason a figure on this panel is not the plain truth, and every
/// one of them is something the user would otherwise have to know about
/// filesystems to work out for themselves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Caveat {
    /// A link, measured as itself. Its target's bytes are somebody else's.
    Link,
    /// A second name for bytes already counted under the first.
    AlreadyCounted,
    /// Sparse or filesystem-compressed: it occupies less than its length.
    OccupiesLess,
    /// The data is not on this disk; the length is what it would be.
    NotHere,
    /// A boundary the scan did not cross, so nothing below it is counted.
    NotDescended,
    /// Could not be read, so this is a lower bound.
    Unreadable,
    /// Something below it could not be read, so the total is a lower bound.
    PartlyUnreadable,
}

impl Caveat {
    /// One line, in the second person, saying what the number is not.
    pub fn line(self) -> &'static str {
        match self {
            Caveat::Link => {
                "A link, measured as the link itself. Whatever it points at is \
                 counted where it actually lives, or not at all."
            }
            Caveat::AlreadyCounted => {
                "Another name for a file already counted under its first name. \
                 It contributes nothing here, and deleting it frees nothing."
            }
            Caveat::OccupiesLess => {
                "Sparse or compressed: it occupies less room than its length. \
                 The difference is a saving, not waste."
            }
            Caveat::NotHere => {
                "Stored in the cloud. The length is what it would take if it \
                 came back; almost none of it is on this disk now."
            }
            Caveat::NotDescended => {
                "On another filesystem, so the scan stopped here. Nothing below \
                 it is in any total on this screen."
            }
            Caveat::Unreadable => "Could not be read, so this is a lower bound.",
            Caveat::PartlyUnreadable => {
                "Something below this could not be read, so the totals are a \
                 lower bound."
            }
        }
    }
}

/// One extension's share of a directory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Extension {
    /// Lowercased, without the dot. Empty means "no extension".
    pub ext: String,
    pub files: u32,
    pub allocated: u64,
}

/// What the panel shows.
#[derive(Clone, PartialEq, Debug)]
pub struct Details {
    pub name: String,
    /// Absolute, so it can be read out or typed somewhere else.
    pub path: String,
    /// "Directory", "File", "Link" — what the entry is, in one word.
    pub kind: &'static str,
    pub category: Category,

    pub allocated: u64,
    pub logical: u64,
    /// On-disk minus apparent: cluster slack, and always at least zero.
    pub slack: u64,
    /// Apparent minus on-disk, for sparse and compressed entries. The mirror
    /// of `slack`, and never non-zero at the same time.
    pub saved: u64,

    /// Share of the directory that contains it, 0.0 to 1.0.
    pub of_parent: f32,
    /// Share of everything the scan measured.
    pub of_scan: f32,
    pub depth: u8,
    pub mtime: u32,

    // --- directories only ---------------------------------------------------
    /// Files anywhere below, which is `subtree_files` and not `child_count`.
    pub files_below: Option<u32>,
    pub directories_below: Option<u32>,
    /// The biggest single child, by what it occupies.
    pub largest_child: Option<(String, u64)>,
    /// Levels below this entry to the deepest thing under it.
    pub deepest_below: Option<u8>,
    /// Biggest extensions first. Empty for a file.
    pub extensions: Vec<Extension>,
    /// Extensions past [`TOP_EXTENSIONS`], pooled.
    pub other_extensions: Option<Extension>,

    pub caveats: Vec<Caveat>,
}

/// Gather everything about `id`.
pub fn of(tree: &SizeTree, id: NodeId) -> Details {
    let node = tree.node(id);
    let flags = node.flags;
    let is_dir = node.is_dir();

    let kind = if flags.contains(Flags::SYMLINK) {
        "Link"
    } else if is_dir {
        "Directory"
    } else {
        "File"
    };

    let mut caveats = Vec::new();
    if flags.contains(Flags::SYMLINK) {
        caveats.push(Caveat::Link);
    }
    if flags.contains(Flags::HARDLINK_DUP) {
        caveats.push(Caveat::AlreadyCounted);
    }
    if flags.contains(Flags::OFFLINE) {
        caveats.push(Caveat::NotHere);
    } else if flags.contains(Flags::SPARSE) {
        // Not both: a cloud placeholder carries the sparse bit too, and saying
        // "occupies less" about a file that is not here at all is a confusing
        // way to say something already said better.
        caveats.push(Caveat::OccupiesLess);
    }
    if flags.contains(Flags::OTHER_DEVICE) {
        caveats.push(Caveat::NotDescended);
    }
    if flags.contains(Flags::UNREADABLE) {
        caveats.push(Caveat::Unreadable);
    } else if flags.contains(Flags::PARTIAL) {
        // `PARTIAL` means something *below* failed. On an entry that is itself
        // unreadable the stronger statement is the true one.
        caveats.push(Caveat::PartlyUnreadable);
    }

    let total = tree.total_allocated();
    let subtree = is_dir.then(|| subtree_shape(tree, id));

    Details {
        name: format::printable(&tree.name_lossy(id)).into_owned(),
        path: tree.path_of(id).display().to_string(),
        kind,
        category: Category::dominant(tree, id),
        allocated: node.allocated,
        logical: node.logical,
        slack: node.allocated.saturating_sub(node.logical),
        saved: node.logical.saturating_sub(node.allocated),
        of_parent: tree.fraction_of_parent(id),
        of_scan: if total == 0 {
            0.0
        } else {
            (node.allocated as f64 / total as f64) as f32
        },
        depth: node.depth,
        mtime: node.mtime,
        files_below: is_dir.then_some(node.subtree_files),
        directories_below: subtree.as_ref().map(|s| s.directories),
        deepest_below: subtree.as_ref().map(|s| s.deepest),
        largest_child: is_dir.then(|| largest_child(tree, id)).flatten(),
        extensions: subtree
            .as_ref()
            .map(|s| s.extensions.clone())
            .unwrap_or_default(),
        other_extensions: subtree.and_then(|s| s.other),
        caveats,
    }
}

struct Shape {
    directories: u32,
    deepest: u8,
    extensions: Vec<Extension>,
    other: Option<Extension>,
}

/// One walk of the subtree, answering every question that needs one.
///
/// The arena is breadth-first and children are contiguous, so a subtree is a
/// contiguous *frontier* rather than a contiguous range — a stack walk is the
/// honest way to cover it, and it is over in microseconds for anything the
/// panel is opened on.
fn subtree_shape(tree: &SizeTree, root: NodeId) -> Shape {
    use rustc_hash::FxHashMap;

    let base = tree.node(root).depth;
    let mut directories = 0u32;
    let mut deepest = base;
    let mut by_ext: FxHashMap<String, (u32, u64)> = FxHashMap::default();

    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = tree.node(id);
        deepest = deepest.max(node.depth);

        if id != root {
            if node.is_dir() {
                directories += 1;
            } else if !node.flags.contains(Flags::HARDLINK_DUP) {
                // A second name for bytes already counted would otherwise make
                // the extension breakdown add up to more than the directory.
                let entry = by_ext.entry(extension_of(tree, id)).or_default();
                entry.0 += 1;
                entry.1 = entry.1.saturating_add(node.allocated);
            }
        }
        stack.extend(tree.children(id));
    }

    let mut extensions: Vec<Extension> = by_ext
        .into_iter()
        .map(|(ext, (files, allocated))| Extension {
            ext,
            files,
            allocated,
        })
        .collect();
    // Biggest first, then by name so two of the same size do not swap places
    // between runs of a program whose hash order is not stable.
    extensions.sort_unstable_by(|a, b| {
        b.allocated
            .cmp(&a.allocated)
            .then_with(|| a.ext.cmp(&b.ext))
    });

    let other = (extensions.len() > TOP_EXTENSIONS).then(|| {
        let rest = extensions.split_off(TOP_EXTENSIONS);
        Extension {
            ext: String::new(),
            files: rest.iter().map(|e| e.files).sum(),
            allocated: rest
                .iter()
                .map(|e| e.allocated)
                .fold(0u64, u64::saturating_add),
        }
    });

    Shape {
        directories,
        deepest: deepest.saturating_sub(base),
        extensions,
        other,
    }
}

/// Lowercased extension without the dot, or empty for a name that has none.
///
/// A leading dot does not start one: `.gitignore` is a name, not an extension,
/// which is the same rule the query language's `ext` field uses.
fn extension_of(tree: &SizeTree, id: NodeId) -> String {
    let name = tree.name_lossy(id);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn largest_child(tree: &SizeTree, id: NodeId) -> Option<(String, u64)> {
    tree.children(id)
        .max_by_key(|&child| tree.node(child).allocated)
        .map(|child| {
            (
                format::printable(&tree.name_lossy(child)).into_owned(),
                tree.node(child).allocated,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kosha_core::{ScanOptions, Scanner};
    use std::fs;

    /// ```text
    /// root/
    ///   media/clip.MP4     400 KB   (uppercase, to pin the lowercasing)
    ///   code/main.rs        40 KB
    ///   code/lib.rs         40 KB
    ///   code/deep/x/y.rs    40 KB
    ///   .gitignore           4 KB   (a dotfile has no extension)
    ///   loose                4 KB   (nor does a name without a dot)
    /// ```
    ///
    /// The sizes are far apart on purpose. An earlier version used one to
    /// eight kilobytes and the orderings came out "wrong" — three `.rs` files
    /// rounded up to twelve kilobytes of *clusters* and beat an eight-kilobyte
    /// video. The code was right and the expectations were not, which is
    /// exactly the confusion a fixture should not invite: these numbers cannot
    /// be flipped by rounding.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("code/deep/x")).unwrap();
        fs::create_dir_all(root.join("media")).unwrap();
        fs::write(root.join("media/clip.MP4"), vec![0u8; 400_000]).unwrap();
        fs::write(root.join("code/main.rs"), vec![0u8; 40_000]).unwrap();
        fs::write(root.join("code/lib.rs"), vec![0u8; 40_000]).unwrap();
        fs::write(root.join("code/deep/x/y.rs"), vec![0u8; 40_000]).unwrap();
        fs::write(root.join(".gitignore"), vec![0u8; 4000]).unwrap();
        fs::write(root.join("loose"), vec![0u8; 4000]).unwrap();
        dir
    }

    fn scan(root: &std::path::Path) -> SizeTree {
        Scanner::new(ScanOptions::default()).scan(root).unwrap()
    }

    fn child(tree: &SizeTree, parent: NodeId, name: &str) -> NodeId {
        tree.children(parent)
            .find(|&id| tree.name_lossy(id) == name)
            .unwrap_or_else(|| panic!("no child `{name}`"))
    }

    #[test]
    fn a_file_reports_what_it_is_and_what_it_costs() {
        let dir = fixture();
        let tree = scan(dir.path());
        let code = child(&tree, tree.root(), "code");
        let main = child(&tree, code, "main.rs");

        let d = of(&tree, main);
        assert_eq!(d.name, "main.rs");
        assert_eq!(d.kind, "File");
        assert_eq!(d.logical, 40_000);
        assert!(d.path.ends_with("main.rs"), "{}", d.path);
        assert!(d.caveats.is_empty(), "{:?}", d.caveats);

        // A file has no subtree, and saying "0 files below" about one would be
        // a true statement that reads as a missing number.
        assert_eq!(d.files_below, None);
        assert_eq!(d.largest_child, None);
        assert!(d.extensions.is_empty());
    }

    #[test]
    fn on_disk_and_apparent_differ_by_slack_and_never_by_both() {
        let dir = fixture();
        let tree = scan(dir.path());
        let code = child(&tree, tree.root(), "code");
        let main = child(&tree, code, "main.rs");

        let d = of(&tree, main);
        assert_eq!(
            d.slack,
            d.allocated.saturating_sub(d.logical),
            "slack is the overhang and nothing else"
        );
        assert!(
            d.slack == 0 || d.saved == 0,
            "a file cannot both waste and save: slack {} saved {}",
            d.slack,
            d.saved
        );
    }

    #[test]
    fn a_directory_counts_what_is_below_it_rather_than_beside_it() {
        let dir = fixture();
        let tree = scan(dir.path());
        let code = child(&tree, tree.root(), "code");

        let d = of(&tree, code);
        assert_eq!(d.kind, "Directory");
        assert_eq!(d.files_below, Some(3), "main.rs, lib.rs and deep/x/y.rs");
        assert_eq!(d.directories_below, Some(2), "deep and x");
        // The file at the bottom is a level too: code -> deep -> x -> y.rs.
        assert_eq!(d.deepest_below, Some(3));
    }

    #[test]
    fn the_deepest_measurement_is_relative_to_the_entry_not_the_scan() {
        let dir = fixture();
        let tree = scan(dir.path());
        let code = child(&tree, tree.root(), "code");
        let deep = child(&tree, code, "deep");

        // Absolute depths differ; the measurement is relative either way.
        assert_eq!(of(&tree, deep).deepest_below, Some(2), "deep -> x -> y.rs");
        assert_eq!(of(&tree, tree.root()).deepest_below, Some(4));
    }

    #[test]
    fn extensions_are_lowercased_grouped_and_ordered_by_size() {
        let dir = fixture();
        let tree = scan(dir.path());

        let d = of(&tree, tree.root());
        let names: Vec<&str> = d.extensions.iter().map(|e| e.ext.as_str()).collect();
        assert_eq!(names.first(), Some(&"mp4"), "biggest first: {names:?}");
        assert!(names.contains(&"rs"), "{names:?}");

        let rs = d.extensions.iter().find(|e| e.ext == "rs").unwrap();
        assert_eq!(rs.files, 3, "three .rs files anywhere below");

        // Ordered by what they occupy, which is not the same as by length.
        let sizes: Vec<u64> = d.extensions.iter().map(|e| e.allocated).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] >= w[1]),
            "not ordered by size: {sizes:?}"
        );
    }

    /// `.gitignore` is a name, not an extension — the same rule the query
    /// language's `ext` field follows, and the two must not disagree.
    #[test]
    fn a_dotfile_and_a_bare_name_share_the_no_extension_group() {
        let dir = fixture();
        let tree = scan(dir.path());

        let d = of(&tree, tree.root());
        let none = d
            .extensions
            .iter()
            .find(|e| e.ext.is_empty())
            .expect("a group for names with no extension");
        assert_eq!(none.files, 2, ".gitignore and loose");
    }

    #[test]
    fn the_largest_child_is_the_biggest_thing_one_level_down() {
        let dir = fixture();
        let tree = scan(dir.path());

        let (name, bytes) = of(&tree, tree.root())
            .largest_child
            .expect("a largest child");
        assert_eq!(name, "media", "400 KB beats code's 120 KB");
        assert!(bytes > 0);
    }

    #[test]
    fn shares_are_fractions_of_the_parent_and_of_the_whole_scan() {
        let dir = fixture();
        let tree = scan(dir.path());
        let root = of(&tree, tree.root());

        assert!(
            (root.of_scan - 1.0).abs() < 0.001,
            "the root is the whole scan: {}",
            root.of_scan
        );

        let media = of(&tree, child(&tree, tree.root(), "media"));
        assert!(
            media.of_scan > 0.0 && media.of_scan < 1.0,
            "{}",
            media.of_scan
        );
        assert!(media.of_parent > 0.0 && media.of_parent <= 1.0);
    }

    #[test]
    fn an_empty_scan_does_not_divide_by_zero() {
        let dir = tempfile::tempdir().unwrap();
        let tree = scan(dir.path());
        assert_eq!(of(&tree, tree.root()).of_scan, 0.0);
    }

    /// Every caveat is a reason a number here is not the plain truth, so each
    /// has to actually say something.
    #[test]
    fn every_caveat_has_a_line_that_says_what_the_number_is_not() {
        for caveat in [
            Caveat::Link,
            Caveat::AlreadyCounted,
            Caveat::OccupiesLess,
            Caveat::NotHere,
            Caveat::NotDescended,
            Caveat::Unreadable,
            Caveat::PartlyUnreadable,
        ] {
            let line = caveat.line();
            assert!(line.len() > 30, "{caveat:?}: {line}");
            assert!(
                line.ends_with('.'),
                "{caveat:?} is a sentence, not a label: {line}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_says_it_is_measured_as_itself() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.bin"), vec![0u8; 4096]).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.bin"), dir.path().join("alias")).unwrap();

        let tree = scan(dir.path());
        let d = of(&tree, child(&tree, tree.root(), "alias"));

        assert_eq!(d.kind, "Link");
        assert!(d.caveats.contains(&Caveat::Link), "{:?}", d.caveats);
    }
}
