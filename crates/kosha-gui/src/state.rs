// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Everything the window knows, and the rules for changing it.
//!
//! Kept apart from rendering so that "what happens when a scan finishes" is
//! readable without wading through layout code, and so the state transitions
//! can be reasoned about on their own.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::SharedString;
use kosha_analyze::Insight;
use kosha_analyze::duplicates::DuplicateReport;
use kosha_core::{NodeId, Progress, ScanOptions, SizeTree};
use kosha_fsops::{Mode, Session, UserConfirmed};
use kosha_query::Query;
use rustc_hash::FxHashMap;

/// Where the window is in its lifecycle.
// Ready is much larger than the other variants, and deliberately not boxed:
// exactly one Phase exists for the life of the window, so the unused bytes are
// a few hundred once, while boxing would put an indirection in front of every
// `ready()` — which the render path calls repeatedly per frame. Speed before
// memory, as everywhere else here.
#[allow(clippy::large_enum_variant)]
pub enum Phase {
    /// Nothing scanned yet. The window is fully interactive here — this is the
    /// first frame, and it must never be a spinner on an empty canvas.
    Idle,
    /// A scan is running. Results stream in; the UI stays live throughout.
    Scanning {
        path: PathBuf,
        progress: Option<Progress>,
        cancel: Arc<AtomicBool>,
    },
    /// A tree is loaded and browsable.
    Ready(Ready),
    Failed {
        path: PathBuf,
        message: String,
        /// What to do about it, where there is something to do.
        ///
        /// Separate from `message` because the two are different kinds of
        /// sentence and the window draws them differently — and because the
        /// answer is the window's to give, not the scanner's. The core can say
        /// "kosha was not asked to read images"; only this crate knows that
        /// here the way to ask is Shift or a setting, where on the command line
        /// it is a flag.
        hint: Option<String>,
    },
}

/// The options a scan runs with when somebody picked a disk image.
///
/// **Choosing a file out of a dialog headed "Read a disk image" is the
/// request**, so this turns image reading on whether or not the setting is —
/// the same rule as the Shift modifier, and for the same reason: it only ever
/// turns a thing *on*, and a dialog somebody just used says what they want
/// more plainly than a setting they may never have found.
///
/// The Master File Table strategy is deliberately left alone. That one opens a
/// *volume* on the running machine and needs administrator to do it; an image
/// is a file, and turning it on here would ask for a privilege that could not
/// help.
///
/// A pure function rather than three lines inside the picker's callback, for
/// the reason [`key_intent`] gives about itself: the picker is a modal the
/// platform owns, so no test that drives the window can reach what happens
/// after it closes.
pub fn image_scan_options(base: kosha_core::ScanOptions) -> kosha_core::ScanOptions {
    kosha_core::ScanOptions {
        use_image: true,
        ..base
    }
}

/// Why a hunt for duplicates cannot run over this tree, if it cannot.
///
/// **A hunt reads bytes, and two kinds of tree have none to read.** A
/// snapshot records sizes; a disk image's paths name parts of one file rather
/// than files, so every read fails. The window has refused the first from the
/// start. It did not refuse the second, and measured on an image holding one
/// identical pair, the command line's version of the same code said:
///
/// ```text
/// examined 2 candidates, read 0 B
/// 2 files could not be read and were excluded
/// no identical files found
/// ```
///
/// The last line is false and is the line a person reads.
///
/// Refused rather than repaired: repairing it means reading file contents out
/// of every image format kosha opens, and the readers map structure rather
/// than data.
///
/// One function for both because they are one question — is there anything
/// here to read? — and because a callback that needs a window is a callback
/// no test can drive, which is the same reason [`key_intent`] and [`opening`]
/// are shaped this way.
pub fn no_bytes_to_hunt(past: bool, strategy: kosha_core::Strategy) -> Option<&'static str> {
    if past {
        return Some(
            "A snapshot records sizes, not contents. Scan the folder as it is now to \
             look for duplicates in it.",
        );
    }
    if strategy == kosha_core::Strategy::DiskImage {
        return Some(
            "A disk image is read for its shape, not its contents. A hunt for \
             duplicates has to read the files themselves, and these are inside \
             the image.",
        );
    }
    None
}

/// What to do with a path somebody handed the window.
///
/// **A path given to the window is an instruction, and an image handed to it
/// was refused.** `kosha-gui some.iso` came back "That scan could not start —
/// it is a disk image, and kosha was not asked to read images", and the advice
/// under it — "Hold Shift when you start a scan to read it, or turn on reading
/// disk images in settings" — is about a scan you start yourself. This one was
/// started for you, before the window drew anything, so the only way to take
/// the advice was to start the whole thing again. The command line's own
/// comment beside that message says the window "reads images by default",
/// which is what this makes true.
///
/// It only ever turns a thing *on*, which is the rule the picker and the Shift
/// modifier already follow: a path somebody typed says what they want more
/// plainly than a setting they may never have found, and reading images costs
/// nothing on a path that is not one — `read_image`'s own doc makes that
/// argument for leaving it on.
///
/// A pure function for the reason [`key_intent`] gives about itself: what
/// happens when the window is handed a path cannot be reached by any test that
/// drives a window.
pub fn opening(path: &std::path::Path, base: kosha_core::ScanOptions) -> Opening {
    if kosha_core::snapshot::is_snapshot(path) {
        Opening::Snapshot
    } else {
        Opening::Scan(image_scan_options(base))
    }
}

/// What [`opening`] decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Opening {
    /// A record: browse it, never scan the path it names.
    Snapshot,
    /// A disk: scan it with these options.
    Scan(kosha_core::ScanOptions),
}

/// What a key press means.
///
/// Decided as a pure function of the situation so it can be tested without a
/// window, because the bug it exists to prevent cannot be reproduced with
/// synthetic input: GPUI ignores posted key messages, so "press Enter in the
/// query box" is not something an automated check can do here. Reasoning about
/// the rule and testing the rule are different things, and this is the one that
/// fails when it is wrong.
///
/// The bug: the root view handles keys for the table, and key events bubble
/// from whatever has focus. So typing a query and pressing Enter also ran the
/// table's Enter — which descended into a directory, cleared the search, and
/// changed the results underneath the person typing. Backspace navigated up.
/// The arrows moved the selection instead of the cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyIntent {
    /// A text box has the keyboard and this key belongs to the text.
    LeaveToText,
    CloseHelp,
    CloseSearch,
    /// Hand the keyboard to the results, so the arrows work straight after.
    CommitSearch,
    AcceptSuggestion,
    /// Nothing is being edited: the key is the table's.
    Navigate,
    /// The command palette is open, so it has the keyboard.
    ClosePalette,
    PaletteMove(isize),
    RunCommand,
    /// The quick-look panel is showing and Escape should dismiss it.
    CloseDetails,
    CloseOwners,
    CloseSettings,
    CloseComparison,
    /// Escape while the duplicate hunt runs stops it.
    StopHunt,
    StopScan,
    /// Escape with a pending confirmation puts the dialog away.
    CancelAction,
    /// And with the question about read-write mode up, it is a No.
    CancelUnlock,
}

/// What is on screen when a key arrives.
///
/// A struct rather than a run of booleans: there are ten of them now, and ten
/// positional `bool`s at a call site is a bug waiting for somebody to swap two.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Keys {
    pub editing_text: bool,
    pub has_suggestions: bool,
    /// The things Escape can put away, in no particular order here — the order
    /// is [`key_intent`]'s, and it is the order they are stacked on screen.
    pub palette_open: bool,
    /// The question about read-write mode, which is drawn over everything
    /// including the palette that may have asked it.
    pub unlock_pending: bool,
    pub action_pending: bool,
    pub owners_open: bool,
    pub help_open: bool,
    pub settings_open: bool,
    pub details_open: bool,
    pub comparison_open: bool,
    pub hunting: bool,
    pub scanning: bool,
    pub filter_open: bool,
}

pub fn key_intent(key: &str, keys: Keys) -> KeyIntent {
    // The palette is modal and takes the keyboard whole. It is checked before
    // `editing_text` because its own search box is what holds focus, so every
    // key would otherwise be handed straight back to the text.
    // Over the palette, and so decided before it: running "Switch to
    // read-write mode" from the palette leaves both on screen for a moment,
    // and Escape then means the question, which is the thing in front.
    if keys.unlock_pending && key == "escape" {
        return KeyIntent::CancelUnlock;
    }
    if keys.palette_open {
        return match key {
            "escape" => KeyIntent::ClosePalette,
            "enter" => KeyIntent::RunCommand,
            "up" => KeyIntent::PaletteMove(-1),
            "down" => KeyIntent::PaletteMove(1),
            "pageup" => KeyIntent::PaletteMove(-8),
            "pagedown" => KeyIntent::PaletteMove(8),
            _ => KeyIntent::LeaveToText,
        };
    }
    // Escape means "put away what is in the way", and it is answered here for
    // everything rather than half here and half in the view — which is where
    // it used to live, and why a comparison was dismissed before the modal
    // panel sitting on top of it.
    //
    // The order is the order `KoshaApp::render` stacks them, front to back. It
    // is taken whether or not a text box has the keyboard: with something over
    // the window, dismissing that is what Escape most obviously means, and the
    // search box is itself last in the list.
    if key == "escape" {
        if keys.action_pending {
            return KeyIntent::CancelAction;
        }
        if keys.owners_open {
            return KeyIntent::CloseOwners;
        }
        if keys.help_open {
            return KeyIntent::CloseHelp;
        }
        if keys.settings_open {
            return KeyIntent::CloseSettings;
        }
        // Quick look is not modal — the table keeps the keyboard under it, so a
        // hand can arrow down the rows and watch the panel follow — but it is
        // in front of the table, so it goes before the things behind it.
        if keys.details_open {
            return KeyIntent::CloseDetails;
        }
        if keys.comparison_open {
            return KeyIntent::CloseComparison;
        }
        // The one thing on screen that is *running*. The key that cancels a
        // scan has to stop the other long job too.
        if keys.scanning {
            return KeyIntent::StopScan;
        }
        if keys.hunting {
            return KeyIntent::StopHunt;
        }
        if keys.filter_open {
            return KeyIntent::CloseSearch;
        }
        return KeyIntent::Navigate;
    }
    if !keys.editing_text {
        return KeyIntent::Navigate;
    }
    match key {
        "enter" => KeyIntent::CommitSearch,
        "tab" if keys.has_suggestions => KeyIntent::AcceptSuggestion,
        // Everything else — characters, backspace, the arrows, home and end —
        // is the text's, and the table must not see it.
        _ => KeyIntent::LeaveToText,
    }
}

/// Two trees held against each other, arranged for display.
///
/// Not rows of the table, and it cannot be: the table renders nodes of the tree
/// on screen, and half of what a comparison has to say is about paths that are
/// *not* in it. A file that was deleted has no node anywhere. So this is its own
/// list — which is also the honest shape, because a comparison is not a
/// directory listing with extra columns, it is a different kind of answer.
#[derive(Clone, Default)]
pub struct Comparison {
    /// What the older side was, in words: "the snapshot from 3 days ago".
    pub against: SharedString,
    /// And what the newer side is: "this scan", or another snapshot's age.
    pub now: SharedString,
    /// Every change above the floor, largest first.
    pub changes: Arc<Vec<kosha_core::compare::Change>>,
    /// Built when filters change, never by cloning every row per frame.
    pub visible_indices: Arc<Vec<usize>>,
    pub change_kind: Option<kosha_core::compare::Kind>,
    pub against_root: PathBuf,
    pub now_root: PathBuf,
    pub against_taken_ms: Option<u64>,
    pub now_taken_ms: Option<u64>,
    pub elapsed: std::time::Duration,
    /// Net across files only, so a directory and the files inside it are never
    /// both counted.
    pub net: i64,
    /// How many were dropped for being too small to matter.
    pub below_floor: usize,
    /// What the comparison rests on, when that needs saying.
    ///
    /// The record is of another folder, or most of it is not here at all —
    /// which is when a list of differences reads as a list of deletions. It sits
    /// in the header rather than in the notice banner because the banner is one
    /// line that truncates, and this was the half of it that got cut.
    pub footing: Option<SharedString>,
    /// The floor that dropped them, so the view can name it.
    pub floor: u64,
    /// Which kinds of row to show. Directories say *where* and files say
    /// *what*, and somebody chasing a disk that filled up wants one or the
    /// other at different moments.
    pub show_dirs: bool,
    pub show_files: bool,
}

impl Comparison {
    /// The rows to draw, after the directory and file switches.
    pub fn rows(&self) -> impl Iterator<Item = &kosha_core::compare::Change> {
        self.visible_indices
            .iter()
            .filter_map(|&index| self.changes.get(index))
    }

    pub fn rebuild_visible(&mut self) {
        self.visible_indices = Arc::new(
            self.changes
                .iter()
                .enumerate()
                .filter(|(_, change)| {
                    if self.change_kind.is_some_and(|kind| kind != change.kind) {
                        return false;
                    }
                    if change.is_dir {
                        self.show_dirs
                    } else {
                        self.show_files
                    }
                })
                .map(|(index, _)| index)
                .collect(),
        );
    }

    pub fn visible(&self) -> usize {
        self.visible_indices.len()
    }
}

/// A duplicate hunt in flight.
///
/// Separate from [`Duplicates`], which is what a *finished* one turns into:
/// one is a job with a stop button, the other is a table. Keeping them apart
/// is what lets the window offer the right control for each, and what stops a
/// half-finished hunt from ever being rendered as a result.
pub struct Hunting {
    /// The latest word from the background pool. `None` for the moment between
    /// asking and the first update, which on a large tree is the time pass 1
    /// takes to group a million files by size.
    pub progress: Option<kosha_analyze::duplicates::Progress>,
    /// Set to stop it. Shared with the job, which reads it between groups.
    pub cancel: Arc<AtomicBool>,
}

/// A finished duplicate hunt, arranged for display.
///
/// Groups are flattened into one list rather than shown as nested sections,
/// because the table renders uniform rows and a section header is not one. The
/// grouping survives as a tint on each row: consecutive groups alternate, which
/// separates them without a separator.
pub struct Duplicates {
    /// Every copy, group by group, biggest group first.
    pub rows: Vec<NodeId>,
    /// Which group each row belongs to. Sorting keeps groups together, so this
    /// is what it groups by.
    pub group_of: FxHashMap<NodeId, usize>,
    /// Which of two tints each row takes.
    ///
    /// Held separately from the group number because it is a property of where
    /// a group ended up on screen, not of which group it is: sorting reorders
    /// the groups, and two that end up adjacent must differ whatever their
    /// numbers happen to be.
    tint_of: FxHashMap<NodeId, u8>,
    /// Copies that are not the one kept — what could actually be removed.
    pub redundant: Vec<NodeId>,
    pub groups: usize,
    /// On-disk bytes recovered by keeping one of each.
    pub reclaimable: u64,
    /// Files that reached the hashing passes.
    pub examined: usize,
    pub bytes_read: u64,
    /// Files excluded because they could not be read.
    pub unreadable: usize,
    /// Files skipped because their data lives in the cloud.
    pub not_local: usize,
    /// Files smaller than the floor, and the floor itself. The commonest
    /// reason a hunt finds nothing, and the only one the user can change.
    pub below_min: usize,
    pub min_size: u64,
    /// Same-size files left out because the hunt reached its candidate ceiling,
    /// the largest size among them, and the ceiling. Never silent: see
    /// `DuplicateReport::over_ceiling`.
    pub over_ceiling: usize,
    pub over_ceiling_up_to: u64,
    pub max_read_bytes: u64,
}

impl Duplicates {
    pub fn from_report(report: DuplicateReport) -> Self {
        let mut rows = Vec::new();
        let mut redundant = Vec::new();
        let mut group_of = FxHashMap::default();

        for (index, group) in report.groups.iter().enumerate() {
            for (position, &id) in group.nodes.iter().enumerate() {
                rows.push(id);
                group_of.insert(id, index);
                // The first copy is the one kept; the rest are what deleting
                // would actually recover.
                if position > 0 {
                    redundant.push(id);
                }
            }
        }

        let mut found = Duplicates {
            groups: report.groups.len(),
            reclaimable: report.total_reclaimable(),
            examined: report.candidates_examined,
            bytes_read: report.bytes_hashed,
            unreadable: report.unreadable,
            not_local: report.not_local,
            below_min: report.below_min,
            min_size: report.min_size,
            over_ceiling: report.over_ceiling,
            over_ceiling_up_to: report.over_ceiling_up_to,
            max_read_bytes: report.max_read_bytes,
            rows,
            group_of,
            tint_of: FxHashMap::default(),
            redundant,
        };
        found.retint();
        found
    }

    /// Which of two tints a row takes, so adjacent groups are distinguishable.
    pub fn tint(&self, id: NodeId) -> u8 {
        self.tint_of.get(&id).copied().unwrap_or(0)
    }

    /// Recompute the tints from the order the rows are actually in.
    ///
    /// Walks the rows once and flips whenever the group changes, so the two
    /// tints alternate down the screen no matter how the groups got there.
    fn retint(&mut self) {
        self.tint_of.clear();
        let mut tint = 0u8;
        let mut previous: Option<usize> = None;
        for &id in &self.rows {
            let group = self.group_of.get(&id).copied().unwrap_or(0);
            if previous.is_some_and(|last| last != group) {
                tint ^= 1;
            }
            previous = Some(group);
            self.tint_of.insert(id, tint);
        }
    }

    /// Reorder the rows, keeping each group whole.
    ///
    /// A flat sort would interleave the groups and leave the tint striping
    /// nothing, which is the one thing the display depends on. So the rows are
    /// sorted within each group, the groups are ordered by their leading row,
    /// and the tints are recomputed for where everything landed. Every copy in
    /// a group is the same size, so a sort by size orders the groups by size —
    /// which is what somebody sorting a duplicate list is asking for.
    fn reorder(
        &mut self,
        mut rows: Vec<NodeId>,
        compare: impl Fn(NodeId, NodeId) -> std::cmp::Ordering,
    ) {
        rows.sort_unstable_by(|&a, &b| compare(a, b));

        let mut rank: FxHashMap<usize, usize> = FxHashMap::default();
        for (position, &id) in rows.iter().enumerate() {
            let group = self.group_of.get(&id).copied().unwrap_or(0);
            rank.entry(group).or_insert(position);
        }

        rows.sort_by(|&a, &b| {
            let ra = self.group_of.get(&a).and_then(|g| rank.get(g)).copied();
            let rb = self.group_of.get(&b).and_then(|g| rank.get(g)).copied();
            ra.cmp(&rb).then_with(|| compare(a, b))
        });

        self.rows = rows;
        self.retint();
    }
}

/// The most results a query will display at once.
///
/// A query matching two hundred thousand entries is a query that needs
/// narrowing, not a table that needs scrolling. The count is still reported in
/// full, so the cap is visible rather than silent.
const RESULT_LIMIT: usize = 2_000;

/// Which column the table is ordered by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    /// Bytes on disk. The default, because it is the question being asked.
    Allocated,
    Logical,
    Items,
    Modified,
}

impl SortKey {
    /// The direction that is useful when you first pick this column.
    ///
    /// Sizes and counts are interesting from the top; names are interesting
    /// from A. Getting this wrong means every user's first click is followed by
    /// a second one to undo it.
    fn natural_descending(self) -> bool {
        !matches!(self, SortKey::Name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Sort {
    /// Apply a click on a column header: same column flips direction, a new
    /// column adopts its natural one.
    pub fn toggled(self, key: SortKey) -> Sort {
        if self.key == key {
            Sort {
                key,
                descending: !self.descending,
            }
        } else {
            Sort {
                key,
                descending: key.natural_descending(),
            }
        }
    }

    /// One ordering shared by worker top-K selection and visible row sorting.
    pub fn compare(self, tree: &SizeTree, a: NodeId, b: NodeId) -> std::cmp::Ordering {
        let (na, nb) = (tree.node(a), tree.node(b));
        let ordering = match self.key {
            SortKey::Name => tree.name(a).cmp(tree.name(b)),
            SortKey::Allocated => na.allocated.cmp(&nb.allocated),
            SortKey::Logical => na.logical.cmp(&nb.logical),
            SortKey::Items => na.subtree_files.cmp(&nb.subtree_files),
            SortKey::Modified => na.mtime.cmp(&nb.mtime),
        };
        let ordering = if self.descending {
            ordering.reverse()
        } else {
            ordering
        };
        // Name breaks every tie, so the order is total and a rescan cannot
        // reshuffle equal rows under the user.
        ordering.then_with(|| tree.name(a).cmp(tree.name(b)))
    }

    /// The arrow to draw beside a header, if this is the sorted column.
    pub fn indicator(self, key: SortKey) -> Option<&'static str> {
        (self.key == key).then_some(if self.descending { "↓" } else { "↑" })
    }
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            key: SortKey::Allocated,
            descending: true,
        }
    }
}

/// Where the tree on screen came from.
///
/// The single most important thing this window can say about itself, and the
/// one the roadmap was right to be afraid of getting wrong: a tree from a file
/// shown as though it were the disk in front of somebody is how a person
/// deletes the wrong thing. So it is a field rather than something derived —
/// the title bar, the status bar and every action all ask the same value.
#[derive(Clone, Debug)]
pub enum Source {
    /// Scanned from the filesystem, however long ago that was.
    Disk,
    /// Read back from a snapshot. This is the past, and it says so everywhere.
    Snapshot(Box<SnapshotSource>),
    /// A verified remote measurement whose paths are display labels.
    Remote(Box<kosha_core::snapshot::Meta>),
}

/// A snapshot being browsed: where it came from and what it says about itself.
#[derive(Clone, Debug)]
pub struct SnapshotSource {
    pub path: PathBuf,
    pub meta: kosha_core::snapshot::Meta,
    pub automatic: bool,
}

impl Source {
    pub fn snapshot(&self) -> Option<&SnapshotSource> {
        match self {
            Source::Disk | Source::Remote(_) => None,
            Source::Snapshot(one) => Some(one),
        }
    }

    pub fn remote(&self) -> Option<&kosha_core::snapshot::Meta> {
        match self {
            Self::Remote(meta) => Some(meta),
            _ => None,
        }
    }

    /// True when what is on screen is a record rather than a disk.
    ///
    /// Every refusal in the window reads this rather than matching on the enum
    /// itself, so that adding a third kind of source cannot silently become a
    /// third kind of thing you are allowed to delete files from.
    pub fn is_past(&self) -> bool {
        !matches!(self, Source::Disk)
    }
}

impl From<kosha_core::snapshot::RemoteRecord> for Ready {
    fn from(record: kosha_core::snapshot::RemoteRecord) -> Self {
        let (meta, tree) = record.into_parts();
        Self::from_source(Arc::new(tree), Source::Remote(Box::new(meta)))
    }
}

/// Bounded measurements for one folder view, tied to its live tree.
pub struct DeepLayout {
    pub scope: NodeId,
    pub report: kosha_analyze::fragmented::Review,
    files: rustc_hash::FxHashMap<NodeId, kosha_core::Fragmentation>,
    folders: rustc_hash::FxHashMap<NodeId, (usize, usize)>,
    pub descending: Option<bool>,
}

impl DeepLayout {
    pub fn new(tree: &SizeTree, scope: NodeId, report: kosha_analyze::fragmented::Review) -> Self {
        let mut files = rustc_hash::FxHashMap::default();
        let mut folders = rustc_hash::FxHashMap::<NodeId, (usize, usize)>::default();
        for file in &report.checked {
            files.insert(file.node, file.layout);
            // A truncated map with fewer than two observed extents is inconclusive.
            if file.layout.truncated && !file.layout.is_fragmented() {
                continue;
            }
            let mut current = file.node;
            while current != scope {
                current = NodeId::from_index(tree.node(current).parent as usize);
                let (fragmented, checked) = folders.entry(current).or_default();
                *checked += 1;
                *fragmented += usize::from(file.layout.is_fragmented());
            }
        }
        Self {
            scope,
            report,
            files,
            folders,
            descending: None,
        }
    }

    pub fn cell(&self, id: NodeId) -> Option<String> {
        if let Some(file) = self.files.get(&id) {
            let prefix = if file.truncated { ">=" } else { "" };
            return Some(format!(
                "{prefix}{}",
                kosha_core::format::plural(file.extents as usize, "extent")
            ));
        }
        let &(fragmented, checked) = self.folders.get(&id)?;
        let percent = (fragmented * 100 + checked / 2) / checked;
        Some(format!("{percent}%"))
    }

    pub fn folder_coverage(&self, tree: &SizeTree, id: NodeId) -> Option<String> {
        let &(fragmented, checked) = self.folders.get(&id)?;
        let total = tree.node(id).subtree_files;
        Some(format!(
            "{fragmented} of {checked} checked files fragmented. {checked} of {total} recorded files checked."
        ))
    }

    fn key(&self, id: NodeId) -> Option<(bool, u64)> {
        if let Some(&(fragmented, checked)) = self.folders.get(&id) {
            Some((true, (fragmented as u64 * 1_000_000) / checked as u64))
        } else {
            self.files.get(&id).map(|file| (false, file.extents))
        }
    }

    fn compare(&self, a: NodeId, b: NodeId, descending: bool) -> std::cmp::Ordering {
        match (self.key(a), self.key(b)) {
            (Some((a_folder, a)), Some((b_folder, b))) => b_folder
                .cmp(&a_folder)
                .then_with(|| if descending { b.cmp(&a) } else { a.cmp(&b) }),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    }
}

pub struct Ready {
    pub tree: Arc<SizeTree>,
    /// Whether this is the disk or a record of it.
    pub source: Source,
    /// The directory currently being displayed.
    pub focus: NodeId,
    /// The row highlighted in the table, if any.
    pub selected: Option<NodeId>,
    /// `None` while analysis is still running in the background.
    pub insights: Option<Arc<Vec<Insight>>>,
    pub deep_layout: Option<DeepLayout>,
    /// The focused directory's children, filtered and ordered for display.
    /// Cached and rebuilt on change rather than recomputed per frame.
    pub rows: Vec<NodeId>,
    /// What the user typed into the query box. Empty means show everything.
    pub filter: String,
    /// What that text turned out to be, once parsed.
    pub search: Search,
    /// GUI searches install worker-produced rows; sorting those rows must never
    /// rerun a whole-tree query on the UI thread.
    external_search: bool,
    /// A finished duplicate hunt, while it is what the table is showing.
    pub duplicates: Option<Duplicates>,
    /// A finished comparison, while it is what the middle of the window is
    /// showing. Replaces the treemap and the table rather than filtering them,
    /// because it is a list of changes and not a list of entries.
    pub comparison: Option<Comparison>,
    /// The hunt in flight, if one is. Reading file contents is the one slow
    /// thing the interface offers, and the only other job besides a scan that
    /// has to be watchable and stoppable.
    pub hunt: Option<Hunting>,
    /// The search that navigating away discarded, so it can be picked up again.
    ///
    /// Searching, opening a result and losing the search is the difference
    /// between a tool you can work through a list with and one you retype a
    /// query into for every row.
    pub previous_search: Option<String>,
    pub sort: Sort,
    /// "Now" captured for the current search. Refreshed on a new edit/request,
    /// then fixed while its worker evaluates and the view rebuilds.
    pub now: u32,
}

/// Identity of the exact view a background search is allowed to replace.
pub struct SearchTicket {
    revision: u64,
    load_generation: u64,
    tree: Arc<SizeTree>,
    focus: NodeId,
    filter: String,
    sort: Sort,
    now: u32,
}

impl SearchTicket {
    pub fn new(ready: &Ready, revision: u64, load_generation: u64) -> Self {
        Self {
            revision,
            load_generation,
            tree: Arc::clone(&ready.tree),
            focus: ready.focus,
            filter: ready.filter.clone(),
            sort: ready.sort,
            now: ready.now,
        }
    }

    pub fn accepts(&self, ready: &Ready, revision: u64, load_generation: u64) -> bool {
        self.revision == revision
            && self.load_generation == load_generation
            && Arc::ptr_eq(&self.tree, &ready.tree)
            && self.focus == ready.focus
            && self.filter == ready.filter
            && self.sort == ready.sort
            && self.now == ready.now
    }
}

/// How the current query text is being interpreted.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Search {
    /// Nothing typed: the table lists the focused directory.
    #[default]
    Browsing,
    /// Plain text: the focused directory, narrowed by name.
    ///
    /// Deliberately not a whole-tree search. Typing a few letters into a filter
    /// box means "narrow what I am looking at", and silently replacing the view
    /// with results from elsewhere would lose the user's place for what they
    /// meant as a squint.
    Filtering { hidden: usize },
    /// A real query: results from anywhere in the tree.
    Results {
        /// How many matched in total, which can exceed what is shown.
        matched: usize,
        /// On-disk bytes across the matches, counting nested ones once.
        bytes: u64,
        /// True when the display was capped.
        capped: bool,
    },
    /// The text does not parse. The table shows nothing and the box explains.
    Invalid { message: String },
    /// A worker owns evaluation; no previous selection remains actionable.
    Pending,
}

impl Ready {
    pub fn new(tree: Arc<SizeTree>) -> Self {
        Ready::from_source(tree, Source::Disk)
    }

    pub fn from_source(tree: Arc<SizeTree>, source: Source) -> Self {
        let focus = tree.root();
        let mut ready = Ready {
            tree,
            source,
            focus,
            selected: None,
            insights: None,
            deep_layout: None,
            rows: Vec::new(),
            filter: String::new(),
            search: Search::Browsing,
            external_search: false,
            duplicates: None,
            comparison: None,
            hunt: None,
            previous_search: None,
            sort: Sort::default(),
            now: kosha_core::format::now_unix(),
        };
        ready.rebuild_rows();
        ready.selected = ready.rows.first().copied();
        ready
    }

    /// Recompute the visible rows from the focus, the filter and the sort.
    ///
    /// The one place row order is decided, and it runs when something changes
    /// rather than once per frame. Sorting a directory of 90,000 entries at
    /// 120 Hz would be the only thing in the render path that scales with
    /// directory size.
    pub fn rebuild_rows(&mut self) {
        let text = self.filter.trim().to_string();
        if text.is_empty() {
            self.external_search = false;
        }

        let mut rows = if self.external_search {
            self.rows.clone()
        } else if text.is_empty() {
            match &self.duplicates {
                // Duplicates are their own view of the whole tree, so they
                // replace the directory listing rather than filtering it.
                Some(found) => {
                    self.search = Search::Browsing;
                    found.rows.clone()
                }
                None => {
                    self.search = Search::Browsing;
                    self.tree.children(self.focus).collect()
                }
            }
        } else {
            match Query::parse(&text) {
                // Plain text narrows the directory in view rather than
                // searching the whole tree — see `Search::Filtering`.
                Ok(query) if query.is_plain_text() => {
                    let rows: Vec<NodeId> = self
                        .tree
                        .children(self.focus)
                        .filter(|&id| query.matches(&self.tree, id, self.now))
                        .collect();
                    let total = self.tree.node(self.focus).child_count as usize;
                    self.search = Search::Filtering {
                        hidden: total.saturating_sub(rows.len()),
                    };
                    rows
                }
                Ok(query) => {
                    let results = query.run(&self.tree, self.now, RESULT_LIMIT);
                    self.search = Search::Results {
                        matched: results.total_matched,
                        bytes: results.total_allocated,
                        capped: results.total_matched > results.matches.len(),
                    };
                    results.matches
                }
                Err(error) => {
                    self.search = Search::Invalid {
                        message: error.to_string(),
                    };
                    Vec::new()
                }
            }
        };

        let tree = Arc::clone(&self.tree);
        let sort = self.sort;
        // Duplicate ordering keeps its groups together.
        if let Some(mut found) = self.duplicates.take() {
            found.reorder(rows, |a, b| sort.compare(&tree, a, b));
            self.rows = found.rows.clone();
            self.duplicates = Some(found);
        } else {
            let layout_order = self
                .visible_deep_layout()
                .and_then(|layout| layout.descending.map(|descending| (layout, descending)));
            rows.sort_unstable_by(|&a, &b| {
                layout_order
                    .map_or(std::cmp::Ordering::Equal, |(layout, descending)| {
                        layout.compare(a, b, descending)
                    })
                    .then_with(|| sort.compare(&tree, a, b))
            });
            self.rows = rows;
        }

        // A selection filtered out of view is worse than no selection: the
        // keyboard would move relative to a row nobody can see.
        if self.selected.is_some_and(|id| !self.rows.contains(&id)) {
            self.selected = self.rows.first().copied();
        }
    }

    #[cfg(test)]
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.external_search = false;
        self.filter = filter.into();
        if !self.filter.is_empty() {
            // A search is a different question from "what do I have twice?".
            self.duplicates = None;
            // A new search replaces the old one; offering to go back to a query
            // the user has already moved on from would be noise.
            self.previous_search = None;
        }
        self.rebuild_rows();
    }

    /// Start a whole-scan search without evaluating it in the event callback.
    pub fn begin_search(&mut self, dsl: String) {
        self.filter = dsl;
        self.external_search = true;
        self.duplicates = None;
        self.previous_search = None;
        self.rows.clear();
        self.selected = None;
        self.search = Search::Pending;
    }

    /// Empty editing leaves navigation's resume chip and a duplicate view
    /// intact. The explicit clear action uses `close_query_view` instead.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.external_search = false;
        self.rebuild_rows();
    }

    pub fn finish_search(&mut self, results: kosha_query::Results) {
        self.search = Search::Results {
            matched: results.total_matched,
            bytes: results.total_allocated,
            capped: results.total_matched > results.matches.len(),
        };
        self.rows = results.matches;
        self.rebuild_rows();
        self.selected = self.rows.first().copied();
    }

    pub fn reject_search(&mut self, text: String, message: String) {
        self.begin_search(text);
        self.search = Search::Invalid { message };
    }

    pub fn visible_deep_layout(&self) -> Option<&DeepLayout> {
        self.deep_layout.as_ref().filter(|layout| {
            layout.scope == self.focus && !self.showing_results() && !self.source.is_past()
        })
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if let Some(layout) = &mut self.deep_layout {
            layout.descending = None;
        }
        self.sort = self.sort.toggled(key);
        self.rebuild_rows();
    }

    /// Take back the search that navigating discarded.
    ///
    /// Taken rather than copied, so the offer disappears once accepted and a
    /// stale query cannot be resumed twice.
    pub fn resume_search(&mut self) -> Option<String> {
        self.previous_search.take()
    }

    /// True when the table is showing results from across the tree rather than
    /// the contents of one directory.
    pub fn showing_results(&self) -> bool {
        matches!(self.search, Search::Results { .. }) || self.duplicates.is_some()
    }

    /// Show a finished duplicate hunt.
    pub fn show_duplicates(&mut self, found: Duplicates) {
        self.filter.clear();
        self.search = Search::Browsing;
        self.duplicates = Some(found);
        self.hunt = None;
        self.rebuild_rows();
        self.selected = self.rows.first().copied();
    }

    /// True while a hunt is reading file contents.
    pub fn is_hunting(&self) -> bool {
        self.hunt.is_some()
    }

    /// Show what two trees said about each other.
    pub fn show_comparison(&mut self, mut comparison: Comparison) {
        comparison.rebuild_visible();
        self.comparison = Some(comparison);
    }

    /// Back to the tree.
    pub fn clear_comparison(&mut self) {
        self.comparison = None;
    }

    /// Ask the hunt to stop. It checks between groups, so this takes effect
    /// within a handful of files rather than at the end of the tree.
    ///
    /// Does not clear `hunt`: the job is still running until it says it has
    /// finished, and a window that forgot about it here would offer to start a
    /// second one on top of the first.
    pub fn stop_hunt(&self) {
        if let Some(hunt) = &self.hunt {
            hunt.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Go back to the directory listing.
    pub fn clear_duplicates(&mut self) {
        self.duplicates = None;
        self.rebuild_rows();
        self.selected = self.rows.first().copied();
    }

    /// Close the query row and everything that keeps it open.
    ///
    /// Three separate things hold that row on screen — a filter, a duplicate
    /// hunt, and the offer to resume a search navigating discarded — and its
    /// close button has to mean "put this away" for all of them. Clearing only
    /// the filter is what it used to do, and in the duplicate view that is a
    /// button which empties an already-empty box: the row stays, the duplicates
    /// stay, and nothing on screen moves.
    pub fn close_query_view(&mut self) {
        self.filter.clear();
        self.previous_search = None;
        self.search = Search::Browsing;
        self.duplicates = None;
        self.rebuild_rows();
        self.selected = self.rows.first().copied();
    }

    /// Move the view to a different directory, refreshing the cached rows.
    ///
    /// Clears the filter: it was typed about the directory you were in, and
    /// carrying it into a new one silently hides most of what you just opened.
    pub fn focus_on(&mut self, id: NodeId) {
        if !self.tree.node(id).is_dir() {
            return;
        }
        self.focus = id;
        // Kept rather than dropped: the user is very likely to come back for
        // the next result, and retyping a query per row is not working through
        // a list, it is doing it again.
        if !self.filter.is_empty() {
            self.previous_search = Some(std::mem::take(&mut self.filter));
        }
        self.duplicates = None;
        self.search = Search::Browsing;
        self.selected = None;
        self.rebuild_rows();
        self.selected = self.rows.first().copied();
    }

    /// Go up one level, keeping the directory we came from selected so that
    /// navigating out and back in does not lose the user's place.
    pub fn ascend(&mut self) {
        if self.focus == self.tree.root() {
            return;
        }
        let came_from = self.focus;
        let parent = NodeId::from_index(self.tree.node(self.focus).parent as usize);
        self.focus_on(parent);
        self.selected = Some(came_from);
    }

    /// Enter the selected directory. Files are not enterable; selecting one and
    /// pressing Enter should do nothing rather than something surprising.
    pub fn descend(&mut self) {
        if let Some(id) = self.selected
            && self.tree.node(id).is_dir()
            && self.tree.node(id).child_count > 0
        {
            self.focus_on(id);
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| self.rows.iter().position(|&r| r == id))
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.rows.len() as isize - 1);
        self.selected = Some(self.rows[next as usize]);
    }

    /// Leave results and go to where a match actually lives.
    ///
    /// A result is somewhere else in the tree, so opening it has to move the
    /// view there — showing its parent with it selected, which is what "reveal"
    /// means everywhere else.
    pub fn reveal(&mut self, id: NodeId) {
        let parent = if id == self.tree.root() {
            self.tree.root()
        } else {
            NodeId::from_index(self.tree.node(id).parent as usize)
        };
        self.focus_on(parent);
        self.selected = Some(id);
    }

    /// The breadcrumb: root first, current directory last.
    pub fn trail(&self) -> Vec<NodeId> {
        self.tree.ancestors(self.focus)
    }
}

pub struct State {
    pub phase: Phase,
    /// Owns the read-only/read-write mode. Nothing here can write without
    /// taking a grant from it.
    pub session: Session,
    pub scan_options: ScanOptions,
    /// Set when a destructive action is awaiting confirmation.
    pub pending: Option<PendingAction>,
    /// Set while the reader is being asked whether this window may change
    /// files. See [`State::ask_to_unlock`].
    pub asking_to_unlock: bool,
    /// Transient message for the status bar.
    pub notice: Option<String>,
}

/// A built plan waiting for the user to approve it.
///
/// The plan is constructed and dry-run *before* the dialog appears, so the
/// numbers on the dialog come out of the real code path rather than an
/// estimate.
pub struct PendingAction {
    pub title: String,
    pub plan: kosha_fsops::ActionPlan,
    pub preview: kosha_fsops::ExecutionReport,
    /// The tree the plan was built from, so "Skip cloud-only items" can
    /// rebuild it against the same scan.
    pub tree: Arc<SizeTree>,
}

impl State {
    pub fn new() -> Self {
        State {
            phase: Phase::Idle,
            session: Session::new(std::env::current_dir().unwrap_or_default()),
            scan_options: ScanOptions::default(),
            pending: None,
            asking_to_unlock: false,
            notice: None,
        }
    }

    /// Ask before this window is allowed to change anything.
    ///
    /// Switching to read-write mode with `Ctrl+Shift+M` must always confirm.
    /// A grant comes only from `Session::enable_read_write`, which requires
    /// confirmation, and `UserConfirmed::from_dialog_acceptance`
    /// says in its own doc: "Call this only from the branch where the user
    /// accepted the confirmation dialog — never to satisfy the type checker."
    /// The toggle constructed the token on the spot, with a comment saying the
    /// real product shows a dialog.
    ///
    /// So the question is asked here, and the token is made in exactly one
    /// place: [`State::unlock_confirmed`], which nothing but the dialog's own
    /// button reaches. Leaving read-write needs no question — going back to
    /// read-only takes power away, and a confirmation for that is a dialog
    /// that can only be a nuisance.
    pub fn ask_to_unlock(&mut self) {
        match self.session.mode() {
            Mode::ReadOnly => self.asking_to_unlock = true,
            Mode::ReadWrite => {
                self.session.return_to_read_only();
                self.notice = Some("Back to read-only.".into());
            }
        }
    }

    /// The reader said yes. The one place a `UserConfirmed` is constructed.
    pub fn unlock_confirmed(&mut self) {
        self.asking_to_unlock = false;
        self.session
            .enable_read_write(UserConfirmed::from_dialog_acceptance());
        self.notice = Some("Read-write mode enabled. Actions still require approval.".into());
    }

    /// They said no, or pressed Escape.
    pub fn cancel_unlock(&mut self) {
        self.asking_to_unlock = false;
    }

    pub fn ready(&self) -> Option<&Ready> {
        match &self.phase {
            Phase::Ready(ready) => Some(ready),
            _ => None,
        }
    }

    pub fn ready_mut(&mut self) -> Option<&mut Ready> {
        match &mut self.phase {
            Phase::Ready(ready) => Some(ready),
            _ => None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.session.mode()
    }

    /// Stop the running scan, including workers reading the current level.
    pub fn cancel_scan(&mut self) {
        if let Phase::Scanning { cancel, .. } = &self.phase {
            cancel.store(true, Ordering::Relaxed);
            self.notice = Some("Stopping scan…".into());
        }
    }
}

impl Default for State {
    fn default() -> Self {
        State::new()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn escape_stops_a_scan_before_dismissing_a_focused_search() {
        let keys = super::Keys {
            scanning: true,
            editing_text: true,
            filter_open: true,
            ..Default::default()
        };
        assert_eq!(
            super::key_intent("escape", keys),
            super::KeyIntent::StopScan
        );
        assert_eq!(
            super::key_intent(
                "escape",
                super::Keys {
                    settings_open: true,
                    ..keys
                }
            ),
            super::KeyIntent::CloseSettings
        );
    }
    use super::*;
    use kosha_core::{ScanOptions, Scanner};

    /// ```text
    /// root/
    ///   big/          (largest, so it sorts first)
    ///     deep/x.bin  8 KiB
    ///   mid/y.bin     4 KiB
    ///   small.txt     16 bytes
    ///   empty/        (a directory with nothing in it)
    /// ```
    fn fixture() -> (tempfile::TempDir, Ready) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("big/deep")).unwrap();
        std::fs::create_dir_all(root.join("mid")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("big/deep/x.bin"), vec![0u8; 8192]).unwrap();
        std::fs::write(root.join("mid/y.bin"), vec![0u8; 4096]).unwrap();
        std::fs::write(root.join("small.txt"), b"sixteen bytes.. ").unwrap();

        let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();
        let ready = Ready::new(Arc::new(tree));
        (dir, ready)
    }

    fn name(ready: &Ready, id: NodeId) -> String {
        kosha_core::format::printable(&ready.tree.name_lossy(id)).into_owned()
    }

    #[test]
    fn deep_layout_rolls_up_checked_files_and_sorts_unknown_last() {
        use kosha_analyze::fragmented::{CheckedFile, Review};
        use std::ffi::OsStr;
        let (_directory, mut ready) = fixture();
        let tree = Arc::clone(&ready.tree);
        let root = tree.root();
        let big = tree.child_named(root, OsStr::new("big")).unwrap();
        let mid = tree.child_named(root, OsStr::new("mid")).unwrap();
        let deep = tree.child_named(big, OsStr::new("deep")).unwrap();
        let x = tree.child_named(deep, OsStr::new("x.bin")).unwrap();
        let y = tree.child_named(mid, OsStr::new("y.bin")).unwrap();
        let empty = tree.child_named(root, OsStr::new("empty")).unwrap();

        let report = Review {
            eligible: 3,
            checked: vec![
                CheckedFile {
                    node: x,
                    layout: kosha_core::Fragmentation {
                        extents: 3,
                        ..Default::default()
                    },
                },
                CheckedFile {
                    node: y,
                    layout: kosha_core::Fragmentation {
                        extents: 1,
                        ..Default::default()
                    },
                },
            ],
            ..Default::default()
        };
        let mut layout = DeepLayout::new(&tree, root, report);
        assert_eq!(layout.cell(root).as_deref(), Some("50%"));
        assert_eq!(layout.cell(big).as_deref(), Some("100%"));
        assert_eq!(layout.cell(mid).as_deref(), Some("0%"));
        assert_eq!(layout.cell(x).as_deref(), Some("3 extents"));
        assert!(layout.cell(empty).is_none());
        assert_eq!(
            layout.folder_coverage(&tree, root).as_deref(),
            Some("1 of 2 checked files fragmented. 2 of 3 recorded files checked.")
        );
        assert_eq!(
            layout.folder_coverage(&tree, mid).as_deref(),
            Some("0 of 1 checked files fragmented. 1 of 1 recorded files checked.")
        );
        assert!(layout.folder_coverage(&tree, x).is_none());
        assert!(layout.folder_coverage(&tree, empty).is_none());
        layout.descending = Some(false);
        ready.deep_layout = Some(layout);
        ready.rebuild_rows();
        assert_eq!(&ready.rows[..2], &[mid, big]);
        ready.set_sort(SortKey::Name);
        assert_eq!(ready.rows[0], big);
    }

    #[test]
    fn verified_ssh_reply_installs_as_a_read_only_remote_source() {
        use kosha_core::snapshot::{self, Meta, ReadLimits, RemoteRecord};

        let (_directory, original) = fixture();
        let meta = Meta::of(&original.tree, std::time::UNIX_EPOCH, "Peer label");
        let mut bytes = Vec::new();
        snapshot::write(&original.tree, &meta, &mut bytes).unwrap();
        let record = RemoteRecord::read(bytes.as_slice(), ReadLimits::default()).unwrap();
        let remote = Ready::from(record);
        assert!(remote.source.is_past());
        assert!(remote.source.snapshot().is_none());
        assert_eq!(remote.source.remote().unwrap().label, "SSH snapshot");
        assert_eq!(
            remote.tree.root_path(),
            std::path::Path::new("[SSH snapshot]")
        );
        assert_eq!(remote.tree.total_logical(), original.tree.total_logical());
    }

    /// A text box has the keyboard and nothing modal is open.
    /// A search box with the keyboard.
    ///
    /// `filter_open` as well, because the two go together in the window: the
    /// box being edited is the one the filter row holds. Escape is decided by
    /// what is *open* rather than by what has focus, so leaving it out here
    /// would be modelling a state that cannot happen.
    fn editing() -> Keys {
        Keys {
            editing_text: true,
            filter_open: true,
            ..Keys::default()
        }
    }

    #[test]
    fn opens_at_the_root_with_the_largest_child_selected() {
        let (_dir, ready) = fixture();

        assert_eq!(ready.focus, ready.tree.root());
        assert_eq!(name(&ready, ready.selected.unwrap()), "big");
    }

    /// The close button on the query row has to put the row *away*, and three
    /// different things keep it open. It used to clear only the filter, which
    /// in the duplicate view is an already-empty box: the row stayed, the
    /// duplicates stayed, and the button looked broken.
    #[test]
    fn closing_the_query_row_leaves_the_duplicate_view() {
        let (_dir, _) = fixture();

        // A real hunt over a real pair, so the test exercises the path the
        // window actually takes rather than a hand-built view.
        let root = _dir.path();
        // Over the default floor, which is a megabyte: below it the hunt
        // correctly declines to consider them and there is no view to close.
        std::fs::write(root.join("a.bin"), vec![7u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(root.join("b.bin"), vec![7u8; 2 * 1024 * 1024]).unwrap();
        let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();
        let mut ready = Ready::new(Arc::new(tree));
        let listing = ready.rows.clone();

        let report = kosha_analyze::duplicates::find(
            &ready.tree,
            &kosha_analyze::duplicates::DuplicateOptions::default(),
        );
        let found = Duplicates::from_report(report);
        assert_eq!(found.groups, 1, "the two copies are one set");
        ready.show_duplicates(found);
        assert!(ready.duplicates.is_some());
        assert!(ready.showing_results(), "the table is showing the hunt");

        ready.close_query_view();

        assert!(ready.duplicates.is_none(), "the hunt is put away");
        assert!(!ready.showing_results(), "the table is a directory again");
        assert_eq!(ready.rows, listing, "and it is the listing it was before");
    }

    /// The same button, with a search in the box rather than a hunt on screen.
    #[test]
    fn closing_the_query_row_clears_the_search() {
        let (_dir, mut ready) = fixture();
        let listing = ready.rows.clone();

        ready.set_filter("x.bin");
        assert_ne!(ready.rows, listing);

        ready.close_query_view();

        assert!(ready.filter.is_empty());
        assert_eq!(ready.rows, listing);
    }

    #[test]
    fn worker_search_clears_actionable_selection_and_installs_bounded_rows() {
        let (_dir, mut ready) = fixture();
        let query = Query::parse("file").unwrap();
        let results = query.run(&ready.tree, ready.now, 2);
        let matched = results.total_matched;
        ready.begin_search("file".into());
        assert_eq!(ready.search, Search::Pending);
        assert!(ready.rows.is_empty());
        assert!(ready.selected.is_none());
        ready.finish_search(results);
        assert_eq!(ready.rows.len(), 2);
        assert!(
            matches!(ready.search, Search::Results { matched: n, capped: true, .. } if n == matched)
        );
        assert!(ready.rows.contains(&ready.selected.unwrap()));
        let selected_rows = ready.rows.clone();
        ready.set_sort(SortKey::Name);
        assert_eq!(ready.rows.len(), selected_rows.len());
        assert!(ready.rows.iter().all(|row| selected_rows.contains(row)));
    }

    #[test]
    fn invalid_worker_search_has_no_selection_and_empty_edit_preserves_resume() {
        let (_dir, mut ready) = fixture();
        let listing = ready.rows.clone();
        ready.reject_search(
            "files whose contents contain secrets".into(),
            "Content search is unavailable".into(),
        );
        assert!(ready.rows.is_empty());
        assert!(ready.selected.is_none());
        assert!(matches!(ready.search, Search::Invalid { .. }));
        ready.previous_search = Some("file and size > 10mib".into());
        ready.clear_filter();
        assert_eq!(ready.rows, listing);
        assert_eq!(
            ready.previous_search.as_deref(),
            Some("file and size > 10mib")
        );
        assert_eq!(ready.search, Search::Browsing);
    }

    #[test]
    fn obsolete_workers_cannot_replace_a_new_query_sort_or_tree() {
        let (_dir, mut ready) = fixture();
        ready.begin_search("file".into());
        let ticket = SearchTicket::new(&ready, 7, 3);
        assert!(ticket.accepts(&ready, 7, 3));
        assert!(!ticket.accepts(&ready, 8, 3), "new edit");
        assert!(!ticket.accepts(&ready, 7, 4), "new scan requested");
        let original_clock = ready.now;
        ready.now = original_clock.saturating_add(1);
        assert!(
            !ticket.accepts(&ready, 7, 3),
            "different relative-date clock"
        );
        ready.now = original_clock;
        ready.set_sort(SortKey::Name);
        assert!(!ticket.accepts(&ready, 7, 3), "different sort order");
        ready.sort = Sort::default();
        ready.clear_filter();
        assert!(!ticket.accepts(&ready, 7, 3), "clear search");
        let (_other_dir, mut other) = fixture();
        other.begin_search("file".into());
        assert!(!ticket.accepts(&other, 7, 3), "different immutable arena");
    }

    #[test]
    fn rows_are_ordered_largest_first() {
        let (_dir, ready) = fixture();

        let sizes: Vec<u64> = ready
            .rows
            .iter()
            .map(|&id| ready.tree.node(id).allocated)
            .collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
    }

    #[test]
    fn descending_enters_the_selected_directory() {
        let (_dir, mut ready) = fixture();
        ready.descend();

        assert_eq!(name(&ready, ready.focus), "big");
        assert_eq!(name(&ready, ready.selected.unwrap()), "deep");
    }

    #[test]
    fn descending_into_a_file_does_nothing() {
        // Selecting a file and pressing Enter should be inert, not surprising.
        let (_dir, mut ready) = fixture();
        let file = ready
            .rows
            .iter()
            .copied()
            .find(|&id| !ready.tree.node(id).is_dir())
            .expect("a file at the root");
        ready.selected = Some(file);

        let before = ready.focus;
        ready.descend();
        assert_eq!(ready.focus, before);
    }

    #[test]
    fn descending_into_an_empty_directory_does_nothing() {
        // Entering it would show a blank pane with no way to tell whether the
        // click registered.
        let (_dir, mut ready) = fixture();
        let empty = ready
            .rows
            .iter()
            .copied()
            .find(|&id| name(&ready, id) == "empty")
            .unwrap();
        ready.selected = Some(empty);

        let before = ready.focus;
        ready.descend();
        assert_eq!(ready.focus, before);
    }

    #[test]
    fn ascending_keeps_your_place() {
        // Going up and back down should not lose where you were.
        let (_dir, mut ready) = fixture();
        ready.descend();
        let was = ready.focus;

        ready.ascend();
        assert_eq!(ready.focus, ready.tree.root());
        assert_eq!(
            ready.selected,
            Some(was),
            "the directory we came from should be selected"
        );
    }

    #[test]
    fn ascending_from_the_root_stays_at_the_root() {
        let (_dir, mut ready) = fixture();
        ready.ascend();
        assert_eq!(ready.focus, ready.tree.root());
    }

    #[test]
    fn selection_moves_and_clamps_at_both_ends() {
        let (_dir, mut ready) = fixture();
        let first = ready.rows[0];
        let last = *ready.rows.last().unwrap();

        ready.move_selection(1);
        assert_eq!(ready.selected, Some(ready.rows[1]));

        // Past the end, and past the beginning, must not wrap or panic.
        ready.move_selection(isize::MAX / 2);
        assert_eq!(ready.selected, Some(last));

        ready.move_selection(isize::MIN / 2);
        assert_eq!(ready.selected, Some(first));
    }

    #[test]
    fn selection_moves_are_safe_in_an_empty_directory() {
        let (_dir, mut ready) = fixture();
        let empty = ready
            .rows
            .iter()
            .copied()
            .find(|&id| name(&ready, id) == "empty")
            .unwrap();
        ready.focus_on(empty);

        assert!(ready.rows.is_empty());
        assert_eq!(ready.selected, None);
        ready.move_selection(1);
        ready.move_selection(-1);
        assert_eq!(ready.selected, None);
    }

    #[test]
    fn focusing_a_file_is_refused() {
        let (_dir, mut ready) = fixture();
        let file = ready
            .rows
            .iter()
            .copied()
            .find(|&id| !ready.tree.node(id).is_dir())
            .unwrap();

        let before = ready.focus;
        ready.focus_on(file);
        assert_eq!(ready.focus, before);
    }

    #[test]
    fn the_trail_runs_root_first_and_focus_last() {
        let (_dir, mut ready) = fixture();
        ready.descend(); // big
        ready.descend(); // big/deep

        let trail = ready.trail();
        assert_eq!(trail.first(), Some(&ready.tree.root()));
        assert_eq!(trail.last(), Some(&ready.focus));
        assert_eq!(name(&ready, trail[1]), "big");
    }

    // ------------------------------------------------------------ filtering

    #[test]
    fn filtering_narrows_the_rows() {
        let (_dir, mut ready) = fixture();
        let before = ready.rows.len();

        ready.set_filter("mid");
        assert_eq!(ready.rows.len(), 1);
        assert_eq!(name(&ready, ready.rows[0]), "mid");
        assert_eq!(ready.search, Search::Filtering { hidden: before - 1 });
    }

    #[test]
    fn filtering_ignores_case_and_surrounding_space() {
        let (_dir, mut ready) = fixture();

        ready.set_filter("  MID  ");
        assert_eq!(ready.rows.len(), 1);
    }

    #[test]
    fn clearing_the_filter_restores_every_row() {
        let (_dir, mut ready) = fixture();
        let before = ready.rows.len();

        ready.set_filter("mid");
        ready.set_filter("");

        assert_eq!(ready.rows.len(), before);
        assert_eq!(ready.search, Search::Browsing);
    }

    #[test]
    fn a_filter_matching_nothing_leaves_no_selection() {
        // Keeping a selection that is not on screen would leave the keyboard
        // moving relative to a row nobody can see.
        let (_dir, mut ready) = fixture();

        ready.set_filter("no-such-entry");
        assert!(ready.rows.is_empty());
        assert_eq!(ready.selected, None);
    }

    #[test]
    fn a_selection_filtered_out_of_view_moves_to_the_first_match() {
        let (_dir, mut ready) = fixture();
        let small = ready
            .rows
            .iter()
            .copied()
            .find(|&id| name(&ready, id) == "small.txt")
            .unwrap();
        ready.selected = Some(small);

        ready.set_filter("mid");
        assert_eq!(name(&ready, ready.selected.unwrap()), "mid");
    }

    #[test]
    fn navigating_clears_the_filter() {
        // It was typed about the directory you were in; carrying it into a new
        // one silently hides most of what you just opened.
        let (_dir, mut ready) = fixture();
        ready.set_filter("deep");
        assert_eq!(ready.rows.len(), 0, "no root child is called deep");

        ready.focus_on(
            ready
                .tree
                .child_named(ready.tree.root(), std::ffi::OsStr::new("big"))
                .unwrap(),
        );

        assert!(ready.filter.is_empty());
        assert_eq!(ready.rows.len(), 1, "big/deep is visible again");
    }

    // -------------------------------------------------------------- sorting

    #[test]
    fn sorting_by_name_is_ascending_first() {
        let (_dir, mut ready) = fixture();
        ready.set_sort(SortKey::Name);

        assert!(!ready.sort.descending, "names read from A");
        let names: Vec<String> = ready.rows.iter().map(|&id| name(&ready, id)).collect();
        let mut expected = names.clone();
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn sorting_by_size_is_descending_first() {
        let (_dir, mut ready) = fixture();
        ready.set_sort(SortKey::Name);
        ready.set_sort(SortKey::Allocated);

        assert!(ready.sort.descending, "sizes read from the top");
        let sizes: Vec<u64> = ready
            .rows
            .iter()
            .map(|&id| ready.tree.node(id).allocated)
            .collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
    }

    #[test]
    fn clicking_the_same_column_twice_reverses_it() {
        let (_dir, mut ready) = fixture();
        let first = ready.rows[0];

        ready.set_sort(SortKey::Allocated); // already the key, so this flips it
        assert!(!ready.sort.descending);
        assert_eq!(*ready.rows.last().unwrap(), first);
    }

    #[test]
    fn the_sort_indicator_marks_only_the_active_column() {
        let sort = Sort::default();
        assert_eq!(sort.indicator(SortKey::Allocated), Some("↓"));
        assert_eq!(sort.indicator(SortKey::Name), None);

        let flipped = sort.toggled(SortKey::Allocated);
        assert_eq!(flipped.indicator(SortKey::Allocated), Some("↑"));
    }

    #[test]
    fn sorting_survives_a_filter_and_the_reverse() {
        let (_dir, mut ready) = fixture();
        ready.set_sort(SortKey::Name);
        ready.set_filter("m");

        assert_eq!(ready.sort.key, SortKey::Name);
        let names: Vec<String> = ready.rows.iter().map(|&id| name(&ready, id)).collect();
        let mut expected = names.clone();
        expected.sort();
        assert_eq!(names, expected, "the filter reset the ordering");
    }

    #[test]
    fn ordering_is_total_so_rescans_do_not_reshuffle() {
        // Every key ties eventually; name breaks the tie so equal rows keep a
        // stable position across rescans.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["c.bin", "a.bin", "b.bin"] {
            std::fs::write(root.join(name), vec![0u8; 1024]).unwrap();
        }
        let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();

        let mut ready = Ready::new(Arc::new(tree));
        ready.set_sort(SortKey::Items); // every file ties at 1
        let order: Vec<String> = ready.rows.iter().map(|&id| name(&ready, id)).collect();

        ready.rebuild_rows();
        let again: Vec<String> = ready.rows.iter().map(|&id| name(&ready, id)).collect();
        assert_eq!(order, again);
    }

    // ------------------------------------------------------------- queries

    #[test]
    fn plain_text_narrows_the_directory_rather_than_searching_everywhere() {
        // Typing a few letters into a box means "narrow what I am looking at".
        // Replacing the view with results from elsewhere would lose the user's
        // place for what they meant as a squint.
        let (_dir, mut ready) = fixture();
        ready.set_filter("deep");

        assert!(matches!(ready.search, Search::Filtering { .. }));
        assert!(
            ready.rows.is_empty(),
            "`deep` is not a child of the root, so nothing matches here"
        );
    }

    #[test]
    fn a_real_query_searches_the_whole_tree() {
        let (_dir, mut ready) = fixture();
        ready.set_filter("name ~ deep");

        assert!(ready.showing_results());
        assert_eq!(ready.rows.len(), 1);
        assert_eq!(name(&ready, ready.rows[0]), "deep");
    }

    #[test]
    fn a_query_reports_what_it_matched() {
        let (_dir, mut ready) = fixture();
        ready.set_filter("file");

        match ready.search {
            Search::Results {
                matched,
                bytes,
                capped,
            } => {
                assert_eq!(matched, 3, "three files in the fixture");
                assert!(bytes > 0);
                assert!(!capped);
            }
            other => panic!("expected results, got {other:?}"),
        }
    }

    #[test]
    fn a_broken_query_explains_itself_and_shows_nothing() {
        // Showing stale rows beside a broken query would suggest they are the
        // answer to it.
        let (_dir, mut ready) = fixture();
        ready.set_filter("size > ");

        assert!(ready.rows.is_empty());
        match &ready.search {
            Search::Invalid { message } => assert!(!message.is_empty()),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn clearing_a_query_returns_to_browsing() {
        let (_dir, mut ready) = fixture();
        ready.set_filter("size > 1");
        assert!(ready.showing_results());

        ready.set_filter("");
        assert_eq!(ready.search, Search::Browsing);
        assert!(!ready.rows.is_empty());
    }

    #[test]
    fn revealing_a_result_moves_to_where_it_lives() {
        let (_dir, mut ready) = fixture();
        ready.set_filter("name ~ x.bin");
        let found = ready.rows[0];

        ready.reveal(found);

        assert_eq!(name(&ready, ready.focus), "deep", "moved to its parent");
        assert_eq!(ready.selected, Some(found));
        assert_eq!(ready.search, Search::Browsing, "and left results behind");
    }

    #[test]
    fn query_results_respect_the_chosen_sort() {
        let (_dir, mut ready) = fixture();
        ready.set_sort(SortKey::Name);
        ready.set_filter("file");

        let names: Vec<String> = ready.rows.iter().map(|&id| name(&ready, id)).collect();
        let mut expected = names.clone();
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn a_query_is_not_confused_by_the_focused_directory() {
        // Results come from the whole tree, so navigating first must not change
        // what a query finds.
        let (_dir, mut ready) = fixture();
        ready.set_filter("name ~ x.bin");
        let from_root = ready.rows.clone();

        ready.set_filter("");
        ready.descend();
        ready.set_filter("name ~ x.bin");

        assert_eq!(ready.rows, from_root);
    }

    // -------------------------------------------------- navigation and state

    /// Every way of moving the view must leave the search behind.
    ///
    /// Enumerated rather than spot-checked: the bug this pins is a *new*
    /// navigation method forgetting, and a test that names each one fails the
    /// moment somebody adds a way to move without clearing.
    #[test]
    fn every_navigation_clears_the_active_search() {
        /// One way of moving the view, named so a failure says which.
        type Navigation = (&'static str, fn(&mut Ready));

        let moves: Vec<Navigation> = vec![
            ("focus_on", |ready| {
                let target = ready
                    .tree
                    .child_named(ready.tree.root(), std::ffi::OsStr::new("big"))
                    .unwrap();
                ready.focus_on(target);
            }),
            ("descend", |ready| {
                ready.selected = ready
                    .tree
                    .child_named(ready.tree.root(), std::ffi::OsStr::new("big"));
                ready.descend();
            }),
            ("reveal", |ready| {
                let target = ready
                    .tree
                    .child_named(ready.tree.root(), std::ffi::OsStr::new("big"))
                    .unwrap();
                ready.reveal(target);
            }),
        ];

        for (name, apply) in moves {
            let (_dir, mut ready) = fixture();
            ready.set_filter("file");
            assert!(ready.showing_results(), "{name}: setup failed");

            apply(&mut ready);

            assert!(
                ready.filter.is_empty(),
                "{name} left the search text behind"
            );
            assert_eq!(
                ready.search,
                Search::Browsing,
                "{name} left the view in results mode"
            );
        }
    }

    #[test]
    fn ascending_clears_the_search_too() {
        let (_dir, mut ready) = fixture();
        ready.descend();
        ready.set_filter("file");
        ready.ascend();

        assert!(ready.filter.is_empty());
        assert_eq!(ready.search, Search::Browsing);
    }

    #[test]
    fn navigating_keeps_the_search_to_come_back_to() {
        // Searching, opening a result and losing the query is the difference
        // between working through a list and retyping for every row.
        let (_dir, mut ready) = fixture();
        ready.set_filter("name ~ x.bin");
        let found = ready.rows[0];

        ready.reveal(found);

        assert_eq!(ready.previous_search.as_deref(), Some("name ~ x.bin"));
    }

    #[test]
    fn resuming_returns_the_search_once() {
        let (_dir, mut ready) = fixture();
        ready.set_filter("file");
        ready.focus_on(ready.tree.root());

        assert_eq!(ready.resume_search().as_deref(), Some("file"));
        assert_eq!(
            ready.resume_search(),
            None,
            "a search should not be resumable twice"
        );
    }

    #[test]
    fn navigating_without_a_search_offers_nothing_to_resume() {
        let (_dir, mut ready) = fixture();
        ready.descend();
        assert_eq!(ready.previous_search, None);
    }

    #[test]
    fn a_new_search_supersedes_the_one_set_aside() {
        // Offering to go back to a query the user has already moved on from is
        // noise, not help.
        let (_dir, mut ready) = fixture();
        ready.set_filter("file");
        ready.focus_on(ready.tree.root());
        assert!(ready.previous_search.is_some());

        ready.set_filter("dir");
        assert_eq!(ready.previous_search, None);
    }

    #[test]
    fn clearing_the_box_does_not_count_as_a_new_search() {
        // Clearing is how the box is emptied on navigation; treating it as a
        // new search would throw away the very thing being kept.
        let (_dir, mut ready) = fixture();
        ready.set_filter("file");
        ready.focus_on(ready.tree.root());
        let kept = ready.previous_search.clone();

        ready.set_filter("");
        assert_eq!(ready.previous_search, kept);
    }

    // ------------------------------------------------------ keyboard routing

    /// The keys that broke: typing a query and pressing Enter navigated the
    /// tree, which cleared the search and changed the results underneath the
    /// person typing. Backspace went up a directory. The arrows moved the
    /// table's selection instead of the text cursor.
    #[test]
    fn editing_keys_never_reach_the_table() {
        for key in [
            "enter",
            "backspace",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "pageup",
            "pagedown",
            "a",
            "space",
            "delete",
        ] {
            let intent = key_intent(key, editing());
            assert_ne!(
                intent,
                KeyIntent::Navigate,
                "`{key}` while typing would have moved the view"
            );
        }
    }

    #[test]
    fn the_same_keys_navigate_when_nothing_is_being_typed() {
        for key in ["enter", "backspace", "up", "down", "left", "right"] {
            assert_eq!(
                key_intent(key, Keys::default()),
                KeyIntent::Navigate,
                "`{key}` should move the view when the table has the keyboard"
            );
        }
    }

    #[test]
    fn enter_commits_the_search_rather_than_navigating() {
        // Handing the keyboard to the results is what a search box is expected
        // to do, and it is what makes the arrows work immediately afterwards.
        assert_eq!(key_intent("enter", editing()), KeyIntent::CommitSearch);
    }

    #[test]
    fn escape_closes_the_help_before_the_search() {
        // The help is on top; closing what is underneath it would be a strange
        // thing to do while looking at it.
        assert_eq!(
            key_intent(
                "escape",
                Keys {
                    help_open: true,
                    ..editing()
                }
            ),
            KeyIntent::CloseHelp
        );
        assert_eq!(key_intent("escape", editing()), KeyIntent::CloseSearch);
    }

    /// Escape puts away the thing in front, and "in front" is a fact about how
    /// `KoshaApp::render` stacks them rather than an opinion.
    ///
    /// This is the test the rule was moved here for. The order used to be split
    /// between `key_intent` and the view's own key handler, and the halves
    /// disagreed: a comparison — which fills the middle of the window — was
    /// dismissed before the owners panel sitting on top of it, and the settings
    /// panel was not dismissed at all.
    #[test]
    fn escape_puts_away_whatever_is_in_front() {
        // Everything at once. Peeling one off at a time must walk the stack
        // from the top down, and the list below *is* the stacking order.
        let mut open = Keys {
            editing_text: true,
            unlock_pending: true,
            action_pending: true,
            owners_open: true,
            help_open: true,
            settings_open: true,
            details_open: true,
            comparison_open: true,
            hunting: true,
            filter_open: true,
            ..Keys::default()
        };
        let order = [
            (KeyIntent::CancelUnlock, "unlock_pending"),
            (KeyIntent::CancelAction, "action_pending"),
            (KeyIntent::CloseOwners, "owners_open"),
            (KeyIntent::CloseHelp, "help_open"),
            (KeyIntent::CloseSettings, "settings_open"),
            (KeyIntent::CloseDetails, "details_open"),
            (KeyIntent::CloseComparison, "comparison_open"),
            (KeyIntent::StopHunt, "hunting"),
            (KeyIntent::CloseSearch, "filter_open"),
        ];
        for (expected, field) in order {
            assert_eq!(
                key_intent("escape", open),
                expected,
                "with everything from `{field}` down still open"
            );
            match field {
                "unlock_pending" => open.unlock_pending = false,
                "action_pending" => open.action_pending = false,
                "owners_open" => open.owners_open = false,
                "help_open" => open.help_open = false,
                "settings_open" => open.settings_open = false,
                "details_open" => open.details_open = false,
                "comparison_open" => open.comparison_open = false,
                "hunting" => open.hunting = false,
                "filter_open" => open.filter_open = false,
                _ => unreachable!(),
            }
        }
        // Nothing left in the way: the key belongs to the window again, which
        // is where cancelling a scan and clearing a notice live.
        assert_eq!(key_intent("escape", open), KeyIntent::Navigate);
    }

    /// Asking is not agreeing: nothing can be written while the question is up.
    ///
    /// The point of the dialog, and the only part of it worth a test — the
    /// wording is on screen and this is not. `UserConfirmed` exists so that
    /// the grant can be traced to somebody saying yes, and for as long as the
    /// toggle constructed one itself, it could not be.
    #[test]
    fn the_window_cannot_write_while_it_is_still_asking() {
        let mut state = State::new();
        assert!(state.session.grant().is_err(), "a new window is read-only");

        state.ask_to_unlock();
        assert!(state.asking_to_unlock, "the question is on screen");
        assert!(
            state.session.grant().is_err(),
            "the session unlocked itself before anybody answered"
        );

        // No.
        state.cancel_unlock();
        assert!(!state.asking_to_unlock);
        assert!(state.session.grant().is_err(), "a refusal left it unlocked");

        // Yes.
        state.ask_to_unlock();
        state.unlock_confirmed();
        assert!(!state.asking_to_unlock);
        assert!(state.session.grant().is_ok(), "yes did not unlock it");

        // And going back the other way asks nothing: taking the power away
        // needs no permission.
        state.ask_to_unlock();
        assert!(
            !state.asking_to_unlock,
            "leaving read-write asked a question"
        );
        assert!(state.session.grant().is_err());
    }

    /// The palette owns the keyboard whole, so it is decided before the stack.
    #[test]
    fn escape_closes_the_palette_before_anything_under_it() {
        let open = Keys {
            palette_open: true,
            action_pending: true,
            owners_open: true,
            ..Keys::default()
        };
        assert_eq!(key_intent("escape", open), KeyIntent::ClosePalette);

        // Except the one thing drawn over the palette itself.
        let asking = Keys {
            unlock_pending: true,
            ..open
        };
        assert_eq!(key_intent("escape", asking), KeyIntent::CancelUnlock);
    }

    /// Escape is answered whether or not a text box has the keyboard.
    ///
    /// The help panel has a search box of its own, so `editing_text` is true
    /// while it is up; if Escape were only considered for an unfocused window
    /// the panel could not be dismissed from the keyboard at all.
    #[test]
    fn escape_does_not_wait_for_the_text_box_to_let_go() {
        for editing in [false, true] {
            let open = Keys {
                editing_text: editing,
                settings_open: true,
                ..Keys::default()
            };
            assert_eq!(key_intent("escape", open), KeyIntent::CloseSettings);
        }
    }

    /// ...and no other key is taken from a text box by any of this.
    #[test]
    fn only_escape_is_decided_by_what_is_open() {
        let open = Keys {
            editing_text: true,
            owners_open: true,
            comparison_open: true,
            hunting: true,
            ..Keys::default()
        };
        for key in ["a", "backspace", "left", "right", "home", "up", "down"] {
            assert_eq!(key_intent(key, open), KeyIntent::LeaveToText, "{key}");
        }
    }

    #[test]
    fn tab_takes_a_suggestion_only_when_there_is_one() {
        assert_eq!(
            key_intent(
                "tab",
                Keys {
                    has_suggestions: true,
                    ..editing()
                }
            ),
            KeyIntent::AcceptSuggestion
        );
        assert_eq!(
            key_intent("tab", editing()),
            KeyIntent::LeaveToText,
            "with nothing to accept, Tab belongs to the text box"
        );
    }

    #[test]
    fn the_palette_takes_the_keyboard_whole() {
        // Its own search box holds focus, so `editing_text` is true throughout;
        // without the palette check first, every one of these would be handed
        // back to the text and the list would be unusable from the keyboard.
        let open = Keys {
            editing_text: true,
            palette_open: true,
            ..Keys::default()
        };

        assert_eq!(key_intent("escape", open), KeyIntent::ClosePalette);
        assert_eq!(key_intent("enter", open), KeyIntent::RunCommand);
        assert_eq!(key_intent("up", open), KeyIntent::PaletteMove(-1));
        assert_eq!(key_intent("down", open), KeyIntent::PaletteMove(1));
        assert_eq!(key_intent("pageup", open), KeyIntent::PaletteMove(-8));
        assert_eq!(key_intent("pagedown", open), KeyIntent::PaletteMove(8));

        // Everything else is still the text being typed into it.
        for key in ["a", "z", "9", "backspace", "left", "right", "tab", "space"] {
            assert_eq!(
                key_intent(key, open),
                KeyIntent::LeaveToText,
                "`{key}` was taken from the palette's search box"
            );
        }
    }

    #[test]
    fn the_palette_outranks_the_help_and_the_suggestions() {
        // Both open at once: escape must close the palette, which is the thing
        // in front, and Tab must not take a suggestion belonging to the box
        // behind it.
        let both = Keys {
            editing_text: true,
            palette_open: true,
            help_open: true,
            has_suggestions: true,
            ..Keys::default()
        };
        assert_eq!(key_intent("escape", both), KeyIntent::ClosePalette);
        assert_eq!(key_intent("tab", both), KeyIntent::LeaveToText);
        assert_eq!(key_intent("enter", both), KeyIntent::RunCommand);
    }

    /// Quick look is not modal: with it open the table keeps the keyboard, so
    /// a hand on the arrows walks the rows and the panel follows. Only Escape
    /// is taken from it.
    #[test]
    fn quick_look_takes_only_escape() {
        let open = Keys {
            details_open: true,
            ..Keys::default()
        };
        assert_eq!(key_intent("escape", open), KeyIntent::CloseDetails);
        for key in ["up", "down", "left", "right", "enter", "space", "a"] {
            assert_eq!(
                key_intent(key, open),
                KeyIntent::Navigate,
                "`{key}` was taken from the table"
            );
        }
    }

    /// Escape with a search box focused *and* the panel up dismisses the panel
    /// — it is the thing in front, and it is what Escape most obviously means.
    /// The palette still outranks it, being properly modal.
    #[test]
    fn quick_look_outranks_the_search_box_and_yields_to_the_palette() {
        let editing_with_panel = Keys {
            details_open: true,
            ..editing()
        };
        assert_eq!(
            key_intent("escape", editing_with_panel),
            KeyIntent::CloseDetails
        );
        // And a space is still a space, which is the property that matters
        // most: `Space` is the panel's own key everywhere else.
        assert_eq!(
            key_intent("space", editing_with_panel),
            KeyIntent::LeaveToText
        );

        let palette_too = Keys {
            palette_open: true,
            ..editing_with_panel
        };
        assert_eq!(key_intent("escape", palette_too), KeyIntent::ClosePalette);
    }

    #[test]
    fn ordinary_characters_are_always_the_texts() {
        for key in ["a", "z", "0", "9", "-", "~", ">", "space"] {
            assert_eq!(
                key_intent(
                    key,
                    Keys {
                        has_suggestions: true,
                        ..editing()
                    }
                ),
                KeyIntent::LeaveToText,
                "`{key}` was taken from the text box"
            );
        }
    }

    /// Two sets of copies, deliberately sized so that sorting by name pulls
    /// them apart and sorting by size interleaves nothing.
    ///
    /// ```text
    /// dupes/
    ///   a1.bin, m_a2.bin, z_a3.bin   3 x 2 MiB, identical
    ///   a_b1.bin, b2.bin             2 x 3 MiB, identical
    ///   only.bin                     3 MiB, different content
    /// ```
    /// The names interleave alphabetically on purpose: `a1`, `a_b1`, `b2`,
    /// `m_a2`, `only`, `z_a3`. A flat sort by name would take both sets apart.
    fn duplicate_fixture() -> (tempfile::TempDir, Ready) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let two = vec![7u8; 2 * 1024 * 1024];
        let three = vec![9u8; 3 * 1024 * 1024];
        let mut other = vec![9u8; 3 * 1024 * 1024];
        // Differs only at the very end, so the edge hash has to read the tail.
        *other.last_mut().unwrap() = 1;

        for file in ["a1.bin", "m_a2.bin", "z_a3.bin"] {
            std::fs::write(root.join(file), &two).unwrap();
        }
        for file in ["a_b1.bin", "b2.bin"] {
            std::fs::write(root.join(file), &three).unwrap();
        }
        std::fs::write(root.join("only.bin"), &other).unwrap();

        let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();
        let mut ready = Ready::new(Arc::new(tree));
        let report = kosha_analyze::duplicates::find(
            &ready.tree,
            &kosha_analyze::duplicates::DuplicateOptions::default(),
        );
        ready.show_duplicates(Duplicates::from_report(report));
        (dir, ready)
    }

    /// The rows of each group, in the order they are displayed.
    fn displayed_groups(ready: &Ready) -> Vec<Vec<String>> {
        let found = ready.duplicates.as_ref().unwrap();
        let mut groups: Vec<Vec<String>> = Vec::new();
        let mut current: Option<usize> = None;
        for &id in &ready.rows {
            let group = found.group_of[&id];
            if current != Some(group) {
                groups.push(Vec::new());
                current = Some(group);
            }
            groups.last_mut().unwrap().push(name(ready, id));
        }
        groups
    }

    #[test]
    fn duplicates_replace_the_directory_listing() {
        let (_dir, ready) = duplicate_fixture();

        let found = ready.duplicates.as_ref().unwrap();
        assert_eq!(found.groups, 2, "two sets of identical files");
        // Five copies across two sets; `only.bin` matches nothing.
        assert_eq!(ready.rows.len(), 5);
        assert!(ready.showing_results());
        assert!(
            !ready.rows.iter().any(|&id| name(&ready, id) == "only.bin"),
            "a file with no twin is not a duplicate"
        );
    }

    #[test]
    fn only_the_copies_past_the_first_are_offered_up() {
        let (_dir, ready) = duplicate_fixture();

        let found = ready.duplicates.as_ref().unwrap();
        // One kept per set: five copies, two sets, three removable.
        assert_eq!(found.redundant.len(), 3);
        assert_eq!(found.reclaimable, 2 * 2 * 1024 * 1024 + 3 * 1024 * 1024);
    }

    #[test]
    fn sorting_never_takes_a_group_apart() {
        let (_dir, mut ready) = duplicate_fixture();

        for key in [
            SortKey::Name,
            SortKey::Allocated,
            SortKey::Logical,
            SortKey::Modified,
            SortKey::Items,
        ] {
            // Toggled twice, so both directions are covered.
            for _ in 0..2 {
                ready.set_sort(key);
                let groups = displayed_groups(&ready);
                assert_eq!(
                    groups.len(),
                    2,
                    "sorting by {key:?} split the sets: {groups:?}"
                );
                let mut sizes: Vec<usize> = groups.iter().map(Vec::len).collect();
                sizes.sort_unstable();
                assert_eq!(
                    sizes,
                    vec![2, 3],
                    "sorting by {key:?} moved rows between sets"
                );
            }
        }
    }

    #[test]
    fn sorting_by_name_still_orders_within_each_set() {
        let (_dir, mut ready) = duplicate_fixture();

        ready.sort = Sort {
            key: SortKey::Name,
            descending: false,
        };
        ready.rebuild_rows();

        for group in displayed_groups(&ready) {
            let mut sorted = group.clone();
            sorted.sort();
            assert_eq!(group, sorted, "names out of order inside a set");
        }
    }

    #[test]
    fn adjacent_sets_always_get_different_tints() {
        let (_dir, mut ready) = duplicate_fixture();

        for key in [SortKey::Name, SortKey::Allocated, SortKey::Modified] {
            ready.set_sort(key);
            let found = ready.duplicates.as_ref().unwrap();
            let tints: Vec<u8> = ready.rows.iter().map(|&id| found.tint(id)).collect();
            let groups: Vec<usize> = ready.rows.iter().map(|&id| found.group_of[&id]).collect();

            for window in 0..groups.len() - 1 {
                let same_group = groups[window] == groups[window + 1];
                assert_eq!(
                    tints[window] == tints[window + 1],
                    same_group,
                    "row {window} and the next disagree about being one set \
                     (sorted by {key:?}): {groups:?} / {tints:?}"
                );
            }
        }
    }

    #[test]
    fn searching_leaves_the_duplicate_view() {
        let (_dir, mut ready) = duplicate_fixture();

        ready.set_filter("only");
        assert!(
            ready.duplicates.is_none(),
            "a search is a different question from what do I have twice"
        );
        assert_eq!(ready.rows.len(), 1);
        assert_eq!(name(&ready, ready.rows[0]), "only.bin");
    }

    #[test]
    fn leaving_and_coming_back_restores_the_directory() {
        let (_dir, mut ready) = duplicate_fixture();

        ready.clear_duplicates();
        assert!(ready.duplicates.is_none());
        assert_eq!(ready.rows.len(), 6, "every file in the directory is back");
    }
}

/// The window could reach the handful of places in the sidebar and nothing
/// else — no picker, no drag and drop — so anything outside that list needed
/// kosha started again with it on the command line.
#[cfg(test)]
mod choosing_what_to_scan {
    use super::image_scan_options;
    use kosha_core::ScanOptions;

    #[test]
    fn picking_an_image_reads_images_whether_or_not_the_setting_is_on() {
        let off = ScanOptions {
            use_image: false,
            ..ScanOptions::default()
        };
        assert!(image_scan_options(off).use_image);

        let on = ScanOptions {
            use_image: true,
            ..ScanOptions::default()
        };
        assert!(image_scan_options(on).use_image);
    }

    /// It only ever turns a thing on, which is the same promise the Shift
    /// modifier makes. A picker that quietly took a scan's accuracy away would
    /// be a worse trade than one more click.
    #[test]
    fn nothing_else_about_the_scan_changes() {
        let base = ScanOptions {
            use_image: false,
            use_mft: false,
            cross_filesystems: true,
            exact_allocated: true,
            dedup_hardlinks: Some(true),
            collect_owners: true,
            max_depth: Some(3),
        };
        let chosen = image_scan_options(base);
        assert_eq!(
            ScanOptions {
                use_image: false,
                ..chosen
            },
            base,
            "something other than image reading was changed"
        );
    }

    /// A path handed to the window is read as what it is.
    ///
    /// A snapshot is browsed — told apart by its first eight bytes, the same
    /// way the command line tells them apart — and anything else is scanned
    /// with images turned on, because `kosha-gui some.iso` used to end at "it
    /// is a disk image, and kosha was not asked to read images" — under advice
    /// about holding Shift while starting a scan, which is no use to somebody
    /// whose scan was started by the path they typed.
    /// A hunt is refused where there are no bytes to read, and only there.
    ///
    /// The image half is the one that was missing. Measured on an ISO holding
    /// exactly one identical pair, the hunt opened paths that name parts of a
    /// file, failed on every one, and finished with "2 files could not be
    /// read and were excluded / no identical files found" — an answer about
    /// an image that does contain a duplicate.
    #[test]
    fn a_hunt_is_refused_where_there_is_nothing_to_read() {
        use kosha_core::Strategy;

        // A live tree, walked or read from the Master File Table, has bytes.
        for strategy in [Strategy::DirectoryWalk, Strategy::MasterFileTable] {
            assert_eq!(
                super::no_bytes_to_hunt(false, strategy),
                None,
                "{strategy:?}"
            );
        }

        let image = super::no_bytes_to_hunt(false, Strategy::DiskImage)
            .expect("an image has no bytes to hunt");
        assert!(image.contains("disk image"), "{image}");

        let past = super::no_bytes_to_hunt(true, Strategy::DirectoryWalk)
            .expect("a snapshot has no bytes to hunt");
        assert!(past.contains("snapshot"), "{past}");

        // A snapshot *of* an image is still first of all a snapshot: the
        // reason it cannot be hunted is that it is a record, whatever the
        // scan behind it did.
        assert_eq!(
            super::no_bytes_to_hunt(true, Strategy::DiskImage),
            Some(past)
        );
    }

    #[test]
    fn a_path_handed_to_the_window_is_opened_as_what_it_is() {
        use super::{Opening, opening};

        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let base = ScanOptions {
            use_image: false,
            ..ScanOptions::default()
        };

        // Not a snapshot, so a scan — and one that can read an image.
        match opening(&folder, base) {
            Opening::Scan(options) => {
                assert!(
                    options.use_image,
                    "an image handed to the window is refused"
                );
                assert_eq!(
                    ScanOptions {
                        use_image: false,
                        ..options
                    },
                    base,
                    "something other than image reading changed"
                );
            }
            Opening::Snapshot => panic!("a folder is not a snapshot"),
        }

        // A file that is not a snapshot is still a scan: whether it is an
        // image is the scanner's question, and it has the flag now to answer
        // it either way.
        let file = dir.path().join("plain.txt");
        std::fs::write(&file, b"not a snapshot").unwrap();
        assert!(matches!(opening(&file, base), Opening::Scan(_)));
    }

    /// The Master File Table opens a *volume* and needs administrator to do
    /// it. An image is a file, so asking for that here would request a
    /// privilege that could not help even if it were granted.
    #[test]
    fn reading_a_volume_directly_is_not_turned_on_with_it() {
        let base = ScanOptions {
            use_mft: false,
            ..ScanOptions::default()
        };
        assert!(!image_scan_options(base).use_mft);
    }
}
