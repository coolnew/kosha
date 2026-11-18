// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Rules a user can add without recompiling.
//!
//! The format is the easy half. The hard half — and the whole reason this file
//! is as long as it is — is refusing to let a rule claim a confidence it has
//! not earned.
//!
//! ## Confidence is derived, never declared
//!
//! A pack says *what to match*. It does not get to say how sure that makes us,
//! because the panel ranks by confidence and anybody writing a pack has every
//! incentive to mark their own rule `Certain`. Once one pack does that, the
//! confidence badge means nothing anywhere, including on the built-in rules
//! that earned theirs.
//!
//! So there is no `confidence` field. There is [`Evidence`], which is a
//! property of the matcher itself:
//!
//! | What the rule matches on | Evidence | Confidence |
//! | --- | --- | --- |
//! | a directory name **and** a required sibling file | `Marker` | `Certain` |
//! | an exact path under the home directory | `KnownPath` | `Likely` |
//! | a directory name alone | `NameOnly` | `Possible` |
//!
//! This mirrors the built-in catalogue exactly, and for the reason given there:
//! thousands of directories are named `build`, and a `build` beside a
//! `CMakeLists.txt` is a build directory while a `build` beside nothing is
//! somebody's source folder. `app-cache` is `Likely` rather than `Certain` for
//! the same reason — "Cache" is a naming convention, not a guarantee.
//!
//! ## And a confidence it has not earned cannot carry a destructive action
//!
//! Every `Possible` rule in the built-in catalogue is paired with a
//! non-destructive action, and a test asserts it stays that way. A pack that
//! matches on a bare directory name and asks to move it to the trash is
//! **rejected**, by name, with the reason — not silently demoted to review-only,
//! because an author whose rule quietly stopped doing what they wrote would have
//! no way to find out.
//!
//! ## One bad rule does not discard the file
//!
//! Rules are validated one at a time. The good ones load, the bad ones are
//! reported, and the pack is never rewritten — the same posture the settings
//! file takes, and for the same reason: somebody's hand-edited file is their
//! work, and destroying it to record an objection is not our place.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::insight::{Confidence, Suggested};

/// What a matcher proves, which is what fixes the confidence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Evidence {
    /// A directory of this name, with a named file beside it. The file is the
    /// proof; the name alone never is.
    Marker,
    /// An exact path relative to the home directory. Being where a known tool
    /// puts things is evidence, though weaker than a marker: the path is a
    /// convention the tool could change.
    KnownPath,
    /// A directory name and nothing else.
    NameOnly,
}

impl Evidence {
    /// How sure this kind of match makes us. The whole point of the type.
    pub fn confidence(self) -> Confidence {
        match self {
            Evidence::Marker => Confidence::Certain,
            Evidence::KnownPath => Confidence::Likely,
            Evidence::NameOnly => Confidence::Possible,
        }
    }
}

/// What a pack rule looks for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Matcher {
    /// A directory of this name. `beside` is empty for a `NameOnly` rule and
    /// holds the alternatives for a `Marker` one — any single one of them
    /// beside the directory is proof.
    Directory { name: String, beside: Vec<String> },
    /// An exact directory path relative to the user's home.
    UnderHome { relative: PathBuf },
}

impl Matcher {
    pub fn evidence(&self) -> Evidence {
        match self {
            Matcher::Directory { beside, .. } if !beside.is_empty() => Evidence::Marker,
            Matcher::Directory { .. } => Evidence::NameOnly,
            Matcher::UnderHome { .. } => Evidence::KnownPath,
        }
    }
}

/// One validated rule from a pack.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PackRule {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub subject: String,
    pub matcher: Matcher,
    pub action: Suggested,
}

impl PackRule {
    pub fn confidence(&self) -> Confidence {
        self.matcher.evidence().confidence()
    }
}

/// Why one rule did not load.
///
/// Carries the rule's id wherever there was one, because a message a pack
/// author cannot trace back to a line they wrote is a message they cannot act
/// on.
///
/// `Display` says what is wrong *and how to fix it*, which is what somebody
/// reading a terminal wants and is too long for a status bar. [`Self::summary`]
/// is the same problem in a few words, for where there is one line.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Rejected {
    #[error("rule `{0}` has no {1}")]
    Missing(String, &'static str),
    #[error(
        "rule `{id}` matches on a directory name alone, which is not enough to \
         justify `{action}` — give it a `beside` file that proves what the \
         directory is, or change the action to `review-only`"
    )]
    Unearned { id: String, action: &'static str },
    #[error(
        "rule `{0}` sets both `directory` and `under-home`; a rule matches one way or the other"
    )]
    Ambiguous(String),
    #[error("rule `{0}` matches nothing: it needs either `directory` or `under-home`")]
    MatchesNothing(String),
    #[error("rule `{0}` has an id that is already taken by a built-in rule")]
    ReservedId(String),
    #[error("`{0}` appears twice; ids have to be unique so a finding can be traced to a rule")]
    DuplicateId(String),
    #[error("rule `{0}` has an id that is not lowercase letters, digits and hyphens")]
    MalformedId(String),
    #[error("rule `{0}` has an `under-home` path that leaves the home directory")]
    EscapesHome(String),
    #[error("rule `{0}` has an `under-home` path that is absolute; it has to be relative to home")]
    AbsolutePath(String),
}

impl Rejected {
    /// The problem in a few words, for a single line of interface.
    ///
    /// Deliberately not a truncation of `Display`: cutting a sentence that ends
    /// with the fix leaves exactly the half that does not help.
    pub fn summary(&self) -> String {
        match self {
            Rejected::Missing(id, what) => format!("`{id}` has no {what}"),
            Rejected::Unearned { id, action } => {
                format!("`{id}` needs a `beside` marker to justify `{action}`")
            }
            Rejected::Ambiguous(id) => format!("`{id}` matches two ways at once"),
            Rejected::MatchesNothing(id) => format!("`{id}` matches nothing"),
            Rejected::ReservedId(id) => format!("`{id}` is a built-in rule's id"),
            Rejected::DuplicateId(id) => format!("`{id}` appears twice"),
            Rejected::MalformedId(id) => format!("`{id}` is not a valid id"),
            Rejected::EscapesHome(id) => format!("`{id}` points outside the home directory"),
            Rejected::AbsolutePath(id) => format!("`{id}` needs a relative path"),
        }
    }
}

/// A pack file as written, before anything has been checked.
#[derive(Debug, Deserialize)]
struct RawPack {
    /// Refused rather than guessed at if it is not one we know. A pack written
    /// for a later format would otherwise load with its unknown fields quietly
    /// ignored, which is how a rule silently stops doing what it says.
    version: u32,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawRule {
    id: String,
    title: String,
    detail: String,
    subject: String,
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    beside: Vec<String>,
    #[serde(default)]
    under_home: Option<String>,
    action: RawAction,
}

/// Deliberately not `Suggested`: `Compress` is a built-in action that operates
/// on a whole tree and has nothing to do with a pack's job, and offering it
/// here would mean supporting it everywhere a pack rule can match.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawAction {
    Trash,
    EmptyContents,
    ReviewOnly,
}

impl RawAction {
    fn into_suggested(self) -> Suggested {
        match self {
            RawAction::Trash => Suggested::Trash,
            RawAction::EmptyContents => Suggested::EmptyContents,
            RawAction::ReviewOnly => Suggested::ReviewOnly,
        }
    }

    fn name(self) -> &'static str {
        match self {
            RawAction::Trash => "trash",
            RawAction::EmptyContents => "empty-contents",
            RawAction::ReviewOnly => "review-only",
        }
    }
}

/// The format this build understands.
pub const VERSION: u32 = 1;

/// What reading a pack produced: the rules that loaded, and why the rest did
/// not.
#[derive(Debug, Default)]
pub struct Loaded {
    pub rules: Vec<PackRule>,
    /// Rules that parsed but did not earn what they asked for.
    pub rejected: Vec<Rejected>,
    /// Whole files that could not be read or parsed at all.
    pub unreadable: Vec<PackError>,
}

impl Loaded {
    /// Whether anything at all went wrong, which is what an interface needs to
    /// know before deciding to say so.
    pub fn has_problems(&self) -> bool {
        !self.rejected.is_empty() || !self.unreadable.is_empty()
    }

    /// Every problem in full, with the fix, for somewhere with room.
    pub fn problems(&self) -> Vec<String> {
        self.unreadable
            .iter()
            .map(|e| e.to_string())
            .chain(self.rejected.iter().map(|e| e.to_string()))
            .collect()
    }

    /// Every problem in a few words each, for a single line of interface.
    pub fn summaries(&self) -> Vec<String> {
        self.unreadable
            .iter()
            .map(|e| match e {
                // The path is the useful half; the parser's own complaint is
                // not something a status bar can act on.
                PackError::Unreadable { path, .. } => format!("could not read {}", path.display()),
                PackError::Malformed { path, .. } => {
                    format!("{} is not valid JSON", path.display())
                }
                PackError::Version { path, found } => {
                    format!("{} is version {found}", path.display())
                }
            })
            .chain(self.rejected.iter().map(Rejected::summary))
            .collect()
    }
}

/// Why a whole file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("could not read `{path}`")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("`{path}` is not valid JSON: {message}")]
    Malformed { path: PathBuf, message: String },
    #[error("`{path}` is a version {found} pack and this build understands version {VERSION}")]
    Version { path: PathBuf, found: u32 },
}

/// Where packs live: `rules.d` beside the settings file.
///
/// A directory rather than one file, so a pack can be dropped in or taken out
/// without editing anything, and so two sources of rules — one written by hand,
/// one shared by a team — do not have to be merged by the person using them.
/// Beside the settings file rather than under the data directory because a pack
/// is configuration: hand-written, worth backing up, and meaningless without
/// the person who wrote it.
pub fn directory() -> Option<PathBuf> {
    Some(kosha_core::dirs::config_dir()?.join("rules.d"))
}

/// Load every pack in [`directory`], in a fixed order.
///
/// Sorted by file name so two runs load the same rules in the same order, and
/// so a duplicate id is rejected from the same file every time rather than
/// from whichever the filesystem happened to hand back first. Determinism is
/// the whole reason the rule engine can be tested at all, and a pack that
/// loaded differently on alternate runs would take that away.
///
/// A missing directory is not an error — almost nobody has one.
pub fn load_all() -> Loaded {
    let mut loaded = Loaded::default();
    let Some(dir) = directory() else {
        return loaded;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return loaded;
    };

    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .collect();
    files.sort();

    let reserved: Vec<String> = crate::registered_rules()
        .iter()
        .map(|id| id.as_str().to_string())
        .collect();

    for file in files {
        let taken: Vec<&str> = reserved
            .iter()
            .map(String::as_str)
            // Ids already taken by an earlier pack are reserved too, so two
            // packs cannot both define `my-cache` and leave which one wins to
            // directory order.
            .chain(loaded.rules.iter().map(|r| r.id.as_str()))
            .collect();
        match read(&file, &taken) {
            Ok(one) => {
                loaded.rules.extend(one.rules);
                loaded.rejected.extend(one.rejected);
            }
            Err(why) => loaded.unreadable.push(why),
        }
    }
    loaded
}

/// Read and validate one pack file.
///
/// A file that cannot be parsed at all is an error — there is nothing to
/// salvage and no rule to name. A file that parses but contains rules that do
/// not earn what they ask for loads the rest and reports them.
pub fn read(path: &Path, reserved: &[&str]) -> Result<Loaded, PackError> {
    let text = std::fs::read_to_string(path).map_err(|source| PackError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text, path, reserved)
}

/// The whole of reading a pack, minus the file. Separated so every rule about
/// what a pack may claim is testable from a string.
pub fn parse(text: &str, path: &Path, reserved: &[&str]) -> Result<Loaded, PackError> {
    let raw: RawPack = serde_json::from_str(text).map_err(|e| PackError::Malformed {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    if raw.version != VERSION {
        return Err(PackError::Version {
            path: path.to_path_buf(),
            found: raw.version,
        });
    }

    let mut loaded = Loaded::default();
    for rule in raw.rules {
        match validate(rule, reserved, &loaded.rules) {
            Ok(rule) => loaded.rules.push(rule),
            Err(why) => loaded.rejected.push(why),
        }
    }
    Ok(loaded)
}

/// Everything a rule has to satisfy before it is allowed to say anything about
/// somebody's disk.
fn validate(raw: RawRule, reserved: &[&str], so_far: &[PackRule]) -> Result<PackRule, Rejected> {
    let id = raw.id.trim().to_string();
    if id.is_empty() {
        return Err(Rejected::Missing(String::from("(unnamed)"), "id"));
    }
    // The same shape the built-in ids have, so a finding's provenance reads the
    // same whichever catalogue it came from.
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        // The one rejection that quotes text nothing has checked: every other
        // variant carries an id that passed the test just above, which is
        // lowercase letters, digits and hyphens. A front end prints these
        // straight — `rule pack: {problem}` — so the id is made printable here,
        // where it is stored, rather than at each place that shows it.
        return Err(Rejected::MalformedId(
            kosha_core::format::printable(&id).into_owned(),
        ));
    }
    if reserved.contains(&id.as_str()) {
        return Err(Rejected::ReservedId(id));
    }
    if so_far.iter().any(|r| r.id == id) {
        return Err(Rejected::DuplicateId(id));
    }

    for (value, what) in [
        (&raw.title, "title"),
        (&raw.detail, "detail"),
        (&raw.subject, "subject"),
    ] {
        if value.trim().is_empty() {
            return Err(Rejected::Missing(id, what));
        }
    }

    let matcher = match (raw.directory, raw.under_home) {
        (Some(_), Some(_)) => return Err(Rejected::Ambiguous(id)),
        (None, None) => return Err(Rejected::MatchesNothing(id)),
        (Some(name), None) => {
            if name.trim().is_empty() {
                return Err(Rejected::Missing(id, "directory name"));
            }
            Matcher::Directory {
                name: name.trim().to_string(),
                // An empty string beside a directory proves nothing and would
                // match a file with no name, so it is dropped rather than
                // counted as evidence.
                beside: raw
                    .beside
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            }
        }
        (None, Some(relative)) => {
            let relative = relative.trim();
            if relative.is_empty() {
                return Err(Rejected::Missing(id, "under-home path"));
            }
            let drive_letter = relative.len() >= 2
                && relative.as_bytes()[1] == b':'
                && relative.as_bytes()[0].is_ascii_alphabetic();
            let relative = PathBuf::from(relative.replace('\\', "/"));
            // A drive letter is checked by hand rather than left to
            // `is_absolute`, which answers for the platform it is running
            // on: `C:/Windows` is absolute on Windows and an ordinary
            // relative directory called `C:` on Linux, so one pack file
            // would mean two different things on two machines. A pack is a
            // file people copy between computers, and a path that is a
            // mistake on one of them is a mistake on all of them.
            if drive_letter || relative.is_absolute() || relative.has_root() {
                return Err(Rejected::AbsolutePath(id));
            }
            // `..` would let a pack point anywhere at all — `C:\Windows` is
            // three of them from most home directories. The safety guards would
            // refuse to act on it, but a rule that can only ever be refused is
            // one to reject while somebody can still fix it.
            if relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(Rejected::EscapesHome(id));
            }
            Matcher::UnderHome { relative }
        }
    };

    // The rule this module exists for.
    let action = raw.action.into_suggested();
    if matcher.evidence().confidence() == Confidence::Possible && action.is_destructive() {
        return Err(Rejected::Unearned {
            id,
            action: raw.action.name(),
        });
    }

    Ok(PackRule {
        id,
        title: raw.title.trim().to_string(),
        detail: raw.detail.trim().to_string(),
        subject: raw.subject.trim().to_string(),
        matcher,
        action,
    })
}

// ------------------------------------------------------------- evaluating

use kosha_core::{NodeId, SizeTree};
use rustc_hash::FxHashSet;

use crate::context::Context;
use crate::insight::{Insight, RuleId, Severity};
use crate::rules::{Rule, util};

impl PackRule {
    fn rule_id(&self) -> RuleId {
        RuleId::owned(self.id.clone())
    }
}

impl Rule for PackRule {
    fn id(&self) -> RuleId {
        self.rule_id()
    }

    /// Exactly the shape the built-in rules have, and deliberately no more.
    ///
    /// A pack rule is a *pure function of the tree*, like every other rule: it
    /// reads names and paths out of the arena and returns nodes. It cannot open
    /// a file, run a command or reach anywhere the built-ins cannot, because it
    /// is evaluated by this code rather than by anything the pack supplies.
    /// That is the other half of the safety story — the validation decides what
    /// a rule may *claim*, and this decides what it may *do*.
    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let found = match &self.matcher {
            Matcher::Directory { name, beside } => self.match_directories(tree, name, beside),
            Matcher::UnderHome { relative } => self.match_under_home(tree, ctx, relative),
        };
        if found.is_empty() {
            return Vec::new();
        }

        let reclaimable = util::total_allocated(tree, &found);
        // A finding too small to matter is dropped here rather than shown,
        // exactly as the built-ins drop theirs. A pack cannot opt out of the
        // user's own threshold.
        if reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.rule_id(),
            title: self.title.clone(),
            detail: self.detail.clone(),
            subject: self.subject.clone(),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            confidence: self.confidence(),
            action: self.action,
            // A user's own rule is theirs to care about, but it has not earned a
            // place above the built-in findings in the panel's urgency ordering.
            severity: Severity::Info,
        }]
    }
}

impl PackRule {
    /// Directories of this name, with proof beside them where proof is required.
    fn match_directories(&self, tree: &SizeTree, name: &str, beside: &[String]) -> Vec<NodeId> {
        let wanted = std::ffi::OsStr::new(name);
        let mut claimed: FxHashSet<NodeId> = FxHashSet::default();
        let mut found = Vec::new();

        // Arena order is breadth-first, so a parent is always seen before its
        // children and `claimed` is populated by the time it is consulted.
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if !node.is_dir() || tree.name(id) != wanted || !util::is_countable(tree, id) {
                continue;
            }
            let proven = beside.is_empty()
                || beside
                    .iter()
                    .any(|s| tree.has_sibling_named(id, std::ffi::OsStr::new(s)));
            if !proven {
                continue;
            }
            // One of these inside another is already inside the bytes about to
            // be reported.
            if util::has_claimed_ancestor(tree, id, &claimed) {
                continue;
            }
            claimed.insert(id);
            found.push(id);
        }
        found
    }

    /// One exact directory under the user's home.
    fn match_under_home(&self, tree: &SizeTree, ctx: &Context, relative: &Path) -> Vec<NodeId> {
        let Some(home) = ctx.home.as_ref() else {
            return Vec::new();
        };
        let target = home.join(relative);
        let Some(leaf) = target.file_name() else {
            return Vec::new();
        };

        // Only build a path for nodes whose name could possibly end the
        // pattern: reconstruction is cheap but not free, and this takes it from
        // every directory in the tree to a handful.
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if !node.is_dir() || tree.name(id) != leaf || !util::is_countable(tree, id) {
                continue;
            }
            if tree.path_of(id) == target {
                return vec![id];
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILT_IN: &[&str] = &["build-artifacts", "package-manager-cache"];

    fn at() -> &'static Path {
        Path::new("rules.json")
    }

    fn one(body: &str) -> Result<Loaded, PackError> {
        parse(
            &format!(r#"{{ "version": 1, "rules": [ {body} ] }}"#),
            at(),
            BUILT_IN,
        )
    }

    /// The shape a pack author actually writes, and the three kinds of evidence
    /// it can offer.
    #[test]
    fn a_pack_loads_and_its_confidence_comes_from_its_evidence() {
        let loaded = parse(
            r#"{
                "version": 1,
                "rules": [
                    {
                        "id": "render-output",
                        "title": "Render output",
                        "detail": "Regenerated by re-rendering the scene.",
                        "subject": "project",
                        "directory": "renders",
                        "beside": ["project.blend"],
                        "action": "trash"
                    },
                    {
                        "id": "my-tool-cache",
                        "title": "My tool's cache",
                        "detail": "Rebuilt on next run.",
                        "subject": "cache directory",
                        "under-home": ".config/mytool/cache",
                        "action": "empty-contents"
                    },
                    {
                        "id": "scratch",
                        "title": "Scratch directories",
                        "detail": "Named like working space.",
                        "subject": "directory",
                        "directory": "scratch",
                        "action": "review-only"
                    }
                ]
            }"#,
            at(),
            BUILT_IN,
        )
        .expect("a well-formed pack loads");

        assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
        let by = |id: &str| loaded.rules.iter().find(|r| r.id == id).unwrap();

        // A marker file is proof, so it earns the top badge.
        assert_eq!(by("render-output").matcher.evidence(), Evidence::Marker);
        assert_eq!(by("render-output").confidence(), Confidence::Certain);

        // Being where a tool puts things is weaker: the tool could move.
        assert_eq!(by("my-tool-cache").matcher.evidence(), Evidence::KnownPath);
        assert_eq!(by("my-tool-cache").confidence(), Confidence::Likely);

        // A name is not evidence.
        assert_eq!(by("scratch").matcher.evidence(), Evidence::NameOnly);
        assert_eq!(by("scratch").confidence(), Confidence::Possible);
    }

    /// The rule this module exists for. A pack cannot buy `Certain` by asking
    /// for it, because there is nowhere to ask.
    #[test]
    fn a_pack_cannot_declare_its_own_confidence() {
        let refused = one(r#"{
                "id": "wishful",
                "title": "Definitely junk",
                "detail": "Trust me.",
                "subject": "directory",
                "directory": "stuff",
                "confidence": "certain",
                "action": "review-only"
            }"#);
        // `deny_unknown_fields`: the field does not exist, and a pack that
        // thinks it set something must be told rather than quietly ignored.
        let message = format!("{}", refused.unwrap_err());
        assert!(message.contains("confidence"), "{message}");
    }

    /// And the other half: a confidence it did not earn cannot carry a
    /// destructive action.
    #[test]
    fn matching_on_a_name_alone_cannot_ask_to_delete() {
        for action in ["trash", "empty-contents"] {
            let loaded = one(&format!(
                r#"{{
                    "id": "bare-name",
                    "title": "Anything called build",
                    "detail": "Looks like build output.",
                    "subject": "directory",
                    "directory": "build",
                    "action": "{action}"
                }}"#
            ))
            .unwrap();

            assert!(loaded.rules.is_empty(), "a bare name was allowed to delete");
            assert!(matches!(
                loaded.rejected.as_slice(),
                [Rejected::Unearned { id, .. }] if id == "bare-name"
            ));
            // The message has to say how to fix it, not merely that it is
            // wrong: the author is looking at a file they wrote.
            let why = format!("{}", loaded.rejected[0]);
            assert!(why.contains("beside"), "{why}");
            assert!(why.contains("review-only"), "{why}");
        }
    }

    /// The same rule with proof attached is allowed exactly what it was
    /// refused, which is the point: the discipline is about evidence, not about
    /// distrusting packs.
    #[test]
    fn the_same_rule_with_a_marker_is_allowed_to_delete() {
        let loaded = one(r#"{
                "id": "bare-name",
                "title": "Build output",
                "detail": "Sits beside the manifest that produced it.",
                "subject": "project",
                "directory": "build",
                "beside": ["CMakeLists.txt"],
                "action": "trash"
            }"#)
        .unwrap();

        assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
        assert_eq!(loaded.rules[0].action, Suggested::Trash);
        assert_eq!(loaded.rules[0].confidence(), Confidence::Certain);
    }

    /// A name-only rule may still describe — it just may not propose.
    #[test]
    fn matching_on_a_name_alone_may_still_point_something_out() {
        let loaded = one(r#"{
                "id": "bare-name",
                "title": "Directories called build",
                "detail": "Worth a look.",
                "subject": "directory",
                "directory": "build",
                "action": "review-only"
            }"#)
        .unwrap();
        assert!(loaded.rejected.is_empty());
        assert_eq!(loaded.rules[0].confidence(), Confidence::Possible);
    }

    /// `..` in an under-home path would let a pack point at anything on the
    /// machine. The safety guards would refuse to act, but a rule that can only
    /// ever be refused is one to reject while the author can still fix it.
    #[test]
    fn an_under_home_path_cannot_leave_the_home_directory() {
        for escape in ["../../Windows", "..", "sub/../../elsewhere"] {
            let loaded = one(&format!(
                r#"{{
                    "id": "escape",
                    "title": "Elsewhere",
                    "detail": "Not under home at all.",
                    "subject": "directory",
                    "under-home": "{escape}",
                    "action": "review-only"
                }}"#
            ))
            .unwrap();
            assert!(
                matches!(loaded.rejected.as_slice(), [Rejected::EscapesHome(_)]),
                "`{escape}` was accepted"
            );
        }
    }

    #[test]
    fn an_under_home_path_has_to_be_relative() {
        for absolute in [r"C:/Windows", "/etc"] {
            let loaded = one(&format!(
                r#"{{
                    "id": "absolute",
                    "title": "Somewhere",
                    "detail": "Given absolutely.",
                    "subject": "directory",
                    "under-home": "{absolute}",
                    "action": "review-only"
                }}"#
            ))
            .unwrap();
            assert!(
                matches!(
                    loaded.rejected.as_slice(),
                    [Rejected::AbsolutePath(_) | Rejected::EscapesHome(_)]
                ),
                "`{absolute}` was accepted: {:?}",
                loaded.rejected
            );
        }
    }

    /// A pack rule that shadowed a built-in id would make a finding's
    /// provenance a lie — the panel names the rule that produced it.
    #[test]
    fn a_pack_cannot_take_a_built_in_id() {
        let loaded = one(r#"{
                "id": "build-artifacts",
                "title": "Mine now",
                "detail": "Shadowing the built-in.",
                "subject": "project",
                "directory": "renders",
                "beside": ["project.blend"],
                "action": "trash"
            }"#)
        .unwrap();
        assert!(matches!(
            loaded.rejected.as_slice(),
            [Rejected::ReservedId(id)] if id == "build-artifacts"
        ));
    }

    #[test]
    fn two_rules_cannot_share_an_id() {
        let body = r#"{
            "id": "same", "title": "One", "detail": "First.", "subject": "directory",
            "directory": "a", "action": "review-only"
        }, {
            "id": "same", "title": "Two", "detail": "Second.", "subject": "directory",
            "directory": "b", "action": "review-only"
        }"#;
        let loaded = one(body).unwrap();
        assert_eq!(loaded.rules.len(), 1, "the first one loads");
        assert!(matches!(
            loaded.rejected.as_slice(),
            [Rejected::DuplicateId(id)] if id == "same"
        ));
    }

    #[test]
    fn a_rule_matches_one_way_or_the_other_and_never_neither() {
        let both = one(r#"{
                "id": "both", "title": "Both", "detail": "Two matchers.",
                "subject": "directory", "directory": "a", "under-home": "b",
                "action": "review-only"
            }"#)
        .unwrap();
        assert!(matches!(both.rejected.as_slice(), [Rejected::Ambiguous(_)]));

        let neither = one(r#"{
                "id": "neither", "title": "Neither", "detail": "No matcher.",
                "subject": "directory", "action": "review-only"
            }"#)
        .unwrap();
        assert!(matches!(
            neither.rejected.as_slice(),
            [Rejected::MatchesNothing(_)]
        ));
    }

    /// One bad rule does not cost the author the rest of their file — the same
    /// posture the settings file takes.
    #[test]
    fn a_bad_rule_does_not_discard_the_good_ones() {
        let loaded = one(r#"{
                "id": "good-one", "title": "Fine", "detail": "Has a marker.",
                "subject": "project", "directory": "renders",
                "beside": ["project.blend"], "action": "trash"
            }, {
                "id": "bad-one", "title": "Not fine", "detail": "No proof.",
                "subject": "directory", "directory": "stuff", "action": "trash"
            }, {
                "id": "good-two", "title": "Also fine", "detail": "Describes only.",
                "subject": "directory", "directory": "notes", "action": "review-only"
            }"#)
        .unwrap();

        assert_eq!(loaded.rules.len(), 2);
        assert_eq!(loaded.rejected.len(), 1);
        assert_eq!(loaded.rules[0].id, "good-one");
        assert_eq!(loaded.rules[1].id, "good-two");
    }

    /// A pack from a later format is refused rather than half-understood.
    /// Loading it and ignoring the fields we do not know is how a rule silently
    /// stops doing what its author wrote.
    #[test]
    fn a_pack_from_another_version_is_refused() {
        let err = parse(r#"{ "version": 2, "rules": [] }"#, at(), BUILT_IN).unwrap_err();
        assert!(matches!(err, PackError::Version { found: 2, .. }));
        let message = format!("{err}");
        assert!(message.contains('2') && message.contains('1'), "{message}");
    }

    #[test]
    fn a_file_that_is_not_json_names_the_file() {
        let err = parse("not json at all", Path::new("mine.json"), BUILT_IN).unwrap_err();
        assert!(matches!(err, PackError::Malformed { .. }));
        assert!(format!("{err}").contains("mine.json"));
    }

    #[test]
    fn an_empty_pack_is_valid_and_does_nothing() {
        let loaded = parse(r#"{ "version": 1 }"#, at(), BUILT_IN).unwrap();
        assert!(loaded.rules.is_empty());
        assert!(loaded.rejected.is_empty());
    }

    /// Ids read the same whichever catalogue a finding came from.
    #[test]
    fn an_id_has_to_look_like_a_rule_id() {
        for bad in [
            "Has Capitals",
            "under_scores",
            "spaces here",
            "punctuation!",
        ] {
            let loaded = one(&format!(
                r#"{{
                    "id": "{bad}", "title": "T", "detail": "D", "subject": "s",
                    "directory": "a", "action": "review-only"
                }}"#
            ))
            .unwrap();
            assert!(
                matches!(loaded.rejected.as_slice(), [Rejected::MalformedId(_)]),
                "`{bad}` was accepted"
            );
        }
    }

    #[test]
    fn a_rule_with_nothing_to_say_is_refused() {
        for (field, body) in [
            (
                "title",
                r#"{ "id": "x", "title": "  ", "detail": "D", "subject": "s", "directory": "a", "action": "review-only" }"#,
            ),
            (
                "detail",
                r#"{ "id": "x", "title": "T", "detail": "", "subject": "s", "directory": "a", "action": "review-only" }"#,
            ),
            (
                "subject",
                r#"{ "id": "x", "title": "T", "detail": "D", "subject": " ", "directory": "a", "action": "review-only" }"#,
            ),
        ] {
            let loaded = one(body).unwrap();
            assert!(
                matches!(loaded.rejected.as_slice(), [Rejected::Missing(_, what)] if *what == field),
                "missing {field} was accepted: {:?}",
                loaded.rejected
            );
        }
    }

    /// An empty string beside a directory proves nothing, so it must not be
    /// counted as the proof that unlocks a destructive action.
    #[test]
    fn an_empty_marker_is_not_evidence() {
        let loaded = one(r#"{
                "id": "sneaky", "title": "Sneaky", "detail": "Empty marker.",
                "subject": "directory", "directory": "stuff", "beside": ["", "  "],
                "action": "trash"
            }"#)
        .unwrap();
        assert!(
            matches!(loaded.rejected.as_slice(), [Rejected::Unearned { .. }]),
            "an empty marker bought a destructive action: {:?}",
            loaded.rejected
        );
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    /// A status bar gets one line. Truncating the full message would cut the
    /// half that says what to do, so the short form is written separately and
    /// has to stay short.
    #[test]
    fn every_problem_has_a_form_that_fits_one_line() {
        let loaded = parse(
            r#"{
                "version": 1,
                "rules": [
                    { "id": "a", "title": "T", "detail": "D", "subject": "s",
                      "directory": "x", "action": "trash" },
                    { "id": "b", "title": "T", "detail": "D", "subject": "s",
                      "directory": "x", "under-home": "y", "action": "review-only" },
                    { "id": "c", "title": "T", "detail": "D", "subject": "s",
                      "action": "review-only" },
                    { "id": "Bad Id", "title": "T", "detail": "D", "subject": "s",
                      "directory": "x", "action": "review-only" },
                    { "id": "d", "title": "T", "detail": "D", "subject": "s",
                      "under-home": "../escape", "action": "review-only" }
                ]
            }"#,
            Path::new("rules.json"),
            &[],
        )
        .unwrap();

        assert_eq!(loaded.rejected.len(), 5, "{:?}", loaded.rejected);
        for (short, full) in loaded.summaries().iter().zip(loaded.problems()) {
            assert!(!short.is_empty());
            assert!(
                short.len() <= 70,
                "`{short}` is {} characters; it has to fit a status bar",
                short.len()
            );
            assert!(
                short.len() < full.len(),
                "the short form is not shorter than the full one"
            );
        }
    }

    /// The short form still names the rule, which is the one thing it cannot
    /// drop: an author with several rules needs to know which one.
    #[test]
    fn the_short_form_still_names_the_rule() {
        let loaded = parse(
            r#"{ "version": 1, "rules": [
                { "id": "my-rule", "title": "T", "detail": "D", "subject": "s",
                  "directory": "x", "action": "trash" }
            ] }"#,
            Path::new("rules.json"),
            &[],
        )
        .unwrap();
        assert!(loaded.summaries()[0].contains("my-rule"));
    }
}
