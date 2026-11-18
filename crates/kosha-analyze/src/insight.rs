// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! What a rule produces.

use kosha_core::NodeId;

/// Stable identifier for a rule, used in tests, filters and the audit log.
///
/// Borrowed for the built-in catalogue, whose ids are literals known at compile
/// time, and owned for a rule that came out of a user's [pack](crate::pack) —
/// which is why this is a `Cow` and not a `&'static str`. Giving every pack
/// finding one shared id would have avoided the indirection and cost the thing
/// the id is for: a finding you cannot trace back to the rule that made it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RuleId(std::borrow::Cow<'static, str>);

impl RuleId {
    /// The usual case: an id fixed in the source.
    pub const fn new(name: &'static str) -> Self {
        RuleId(std::borrow::Cow::Borrowed(name))
    }

    /// An id read from a pack at runtime.
    pub fn owned(name: impl Into<String>) -> Self {
        RuleId(std::borrow::Cow::Owned(name.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How sure we are that the bytes are genuinely reclaimable.
///
/// This is not decoration: it weights the ranking, and it decides whether the
/// UI offers an action at all. Nothing below `Likely` gets a delete button.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Confidence {
    /// Interesting, but the user has to decide. Never auto-actionable.
    Possible,
    /// Strong evidence. Offered as an action, with the evidence shown.
    Likely,
    /// True by definition — a build directory, a package manager's own cache.
    Certain,
}

impl Confidence {
    /// Ranking weight. A certain 2 GB should outrank a speculative 6 GB.
    pub(crate) fn weight(self) -> f64 {
        match self {
            Confidence::Certain => 1.0,
            Confidence::Likely => 0.55,
            Confidence::Possible => 0.2,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Info,
    Notice,
    Important,
}

/// What the UI should offer to do about a finding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Suggested {
    /// Move the whole node to the platform trash.
    Trash,
    /// Delete the contents but keep the directory — correct for caches that
    /// the owning tool expects to still exist.
    EmptyContents,
    /// Enable filesystem compression in place.
    Compress,
    /// Show it, explain it, offer no destructive action.
    ReviewOnly,
}

impl Suggested {
    /// Whether acting on this needs read-write mode at all.
    pub fn is_destructive(self) -> bool {
        matches!(self, Suggested::Trash | Suggested::EmptyContents)
    }

    /// True when there is something for a button to do.
    ///
    /// Distinct from [`Self::is_destructive`], and the distinction matters now
    /// that compression is implemented: it changes the filesystem without
    /// removing anything, so it needs a button but not a warning. The UI asked
    /// "is it destructive?" when it meant "does it act", which was the same
    /// question only while every action deleted something.
    pub fn is_actionable(self) -> bool {
        !matches!(self, Suggested::ReviewOnly)
    }

    /// What to call this where somebody reads it.
    ///
    /// Here rather than in a front end because both front ends were doing it
    /// separately and only one of them was doing it: the window matched on
    /// the variants for its button captions, and the command line printed the
    /// `Debug` name, so a finding that is for looking at came out as
    /// `ReviewOnly` and a cache came out as `EmptyContents`. A variant renamed
    /// for the sake of the code would have changed what a person read.
    pub fn label(self) -> &'static str {
        match self {
            Suggested::Trash => "Move to trash",
            Suggested::EmptyContents => "Empty",
            Suggested::Compress => "Compress",
            Suggested::ReviewOnly => "Review",
        }
    }
}

/// One finding: what, where, how much, and how sure.
///
/// `title`, `detail` and `subject` are printed as they stand by both front ends,
/// and both are given them by the engine rather than by a rule directly: on the
/// way out they have their control characters replaced, so no rule — and no rule
/// pack somebody downloaded — can forge a row or move the cursor. See
/// `engine::only_printable_text`.
#[derive(Clone, Debug)]
pub struct Insight {
    pub rule: RuleId,
    /// One line, no trailing period, and **no counts or byte figures**.
    ///
    /// The engine trims overlapping findings and recomputes `reclaimable`
    /// afterwards, so any number baked into prose here would be a number from
    /// before that happened. Titles name the thing; the view renders the
    /// figures from the structured fields, once, in a consistent place.
    pub title: String,
    /// One sentence explaining *why* this is safe or worth reviewing. Subject
    /// to the same rule as `title`: no counts, no byte figures.
    pub detail: String,
    /// Singular noun for what `nodes` holds — "project", "cache directory",
    /// "installer". The view pluralises it against the live count, so the
    /// wording stays rule-specific while the number stays true.
    ///
    /// Owned, like `title` and `detail` beside it, because a rule read from a
    /// user's [pack](crate::pack) supplies its own and the alternative was
    /// leaking a string on every rescan.
    pub subject: String,
    /// Every node this finding is about, ordered largest-on-disk first.
    ///
    /// Complete, never truncated. `reclaimable` is derived from exactly this
    /// list, so the engine can trim overlaps between findings and recompute the
    /// total without the two drifting apart. Views that want a short list take
    /// from the front.
    pub nodes: Vec<NodeId>,
    /// Nodes dropped from this finding because a more confident one already
    /// claimed them. Shown by the UI so a shrunken count is explained rather
    /// than merely odd.
    pub superseded: usize,
    /// On-disk bytes recovered by acting — never logical bytes, because
    /// allocated is what the user actually gets back.
    ///
    /// The sum over `nodes`, **less any bytes a more confident finding has
    /// already claimed inside them**. Those two are the same number in almost
    /// every panel, and where they are not, this is the smaller one: a directory
    /// holds its whole subtree in `allocated`, so a finding about something
    /// inside it would otherwise have its bytes counted twice — once on its own
    /// and once inside the directory above. Summing across findings is safe
    /// because of that subtraction and not merely because no node appears in two
    /// of them, which it does not.
    pub reclaimable: u64,
    pub confidence: Confidence,
    pub action: Suggested,
    pub severity: Severity,
}

impl Insight {
    /// Score used to order the insight panel.
    pub fn score(&self) -> f64 {
        self.reclaimable as f64 * self.confidence.weight()
    }

    /// How many nodes this finding covers.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}
