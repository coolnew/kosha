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
//! | an exact structured location | `KnownPath` | `Likely` |
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

use std::io::Read;
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
    /// An exact recorded directory relative to an explicit location base.
    Location(crate::location::Location),
}

impl Matcher {
    pub fn evidence(&self) -> Evidence {
        match self {
            Matcher::Directory { beside, .. } if !beside.is_empty() => Evidence::Marker,
            Matcher::Directory { .. } => Evidence::NameOnly,
            Matcher::Location(_) => Evidence::KnownPath,
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
    #[error("rule `{id}` exceeds its authoring limit: {what}")]
    Limit { id: String, what: &'static str },
    #[error("rule `{0}` has no {1}")]
    Missing(String, &'static str),
    #[error(
        "rule `{id}` matches on a directory name alone, which is not enough to \
         justify `{action}` — give it a `beside` file that proves what the \
         directory is, or change the action to `review-only`"
    )]
    Unearned { id: String, action: &'static str },
    #[error("rule `{0}` sets both `directory` and `location`; a rule matches one way or the other")]
    Ambiguous(String),
    #[error("rule `{0}` matches nothing: it needs either `directory` or `location`")]
    MatchesNothing(String),
    #[error("rule `{0}` has an id that is already taken by a built-in rule")]
    ReservedId(String),
    #[error("`{0}` appears twice; ids have to be unique so a finding can be traced to a rule")]
    DuplicateId(String),
    #[error("rule `{0}` has an id that is not lowercase letters, digits and hyphens")]
    MalformedId(String),
    #[error("rule `{id}` has an invalid location: {message}")]
    InvalidLocation { id: String, message: &'static str },
}

impl Rejected {
    /// The problem in a few words, for a single line of interface.
    ///
    /// Deliberately not a truncation of `Display`: cutting a sentence that ends
    /// with the fix leaves exactly the half that does not help.
    pub fn summary(&self) -> String {
        match self {
            Rejected::Limit { id, .. } => format!("`{id}` exceeds a rule limit"),
            Rejected::Missing(id, what) => format!("`{id}` has no {what}"),
            Rejected::Unearned { id, action } => {
                format!("`{id}` needs a `beside` marker to justify `{action}`")
            }
            Rejected::Ambiguous(id) => format!("`{id}` matches two ways at once"),
            Rejected::MatchesNothing(id) => format!("`{id}` matches nothing"),
            Rejected::ReservedId(id) => format!("`{id}` is a built-in rule's id"),
            Rejected::DuplicateId(id) => format!("`{id}` appears twice"),
            Rejected::MalformedId(id) => format!("`{id}` is not a valid id"),
            Rejected::InvalidLocation { id, .. } => format!("`{id}` has an invalid location"),
        }
    }
}

/// A pack file as written, before anything has been checked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPack {
    /// Refused rather than guessed at if it is not one we know. A pack written
    /// for a later format would otherwise load with its unknown fields quietly
    /// ignored, which is how a rule silently stops doing what it says.
    version: u32,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
    location: Option<crate::location::Location>,
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
/// Local rule packs are bounded data, never executable plugins.
pub const MAX_PACK_BYTES: usize = 256 * 1024;
pub const MAX_PACK_RULES: usize = 32;
pub const MAX_PACK_FILES: usize = 32;
pub const MAX_TOTAL_RULES: usize = 64;

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
                PackError::Limit { path, message } => format!("{}: {message}", path.display()),
            })
            .chain(self.rejected.iter().map(Rejected::summary))
            .collect()
    }
}

/// Why a whole file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("rule pack `{path}` refused: {message}")]
    Limit { path: PathBuf, message: String },
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
    let Some(dir) = directory() else {
        return Loaded::default();
    };
    load_directory(&dir)
}

fn limit(path: &Path, message: impl Into<String>) -> PackError {
    PackError::Limit {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn load_directory(dir: &Path) -> Loaded {
    let mut loaded = Loaded::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return loaded,
        Err(source) => {
            loaded.unreadable.push(PackError::Unreadable {
                path: dir.to_path_buf(),
                source,
            });
            return loaded;
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(source) => {
                loaded.unreadable.push(PackError::Unreadable {
                    path: dir.to_path_buf(),
                    source,
                });
                return loaded;
            }
        };
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            if files.len() == MAX_PACK_FILES {
                loaded.unreadable.push(limit(
                    dir,
                    format!("at most {MAX_PACK_FILES} JSON files are supported; no files loaded"),
                ));
                return loaded;
            }
            files.push(path);
        }
    }
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
                if loaded.rules.len() + one.rules.len() > MAX_TOTAL_RULES {
                    loaded.unreadable.push(limit(&file, format!("at most {MAX_TOTAL_RULES} rules across rules.d; this file was not loaded")));
                    continue;
                }
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
    let metadata = std::fs::symlink_metadata(path).map_err(|source| PackError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(limit(
            path,
            "expected a regular file, not a link or directory",
        ));
    }
    if metadata.len() > MAX_PACK_BYTES as u64 {
        return Err(limit(
            path,
            format!("maximum file size is {MAX_PACK_BYTES} bytes"),
        ));
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_PACK_BYTES as u64 + 1)
                .read_to_string(&mut text)
        })
        .map_err(|source| PackError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
    parse(&text, path, reserved)
}

/// The whole of reading a pack, minus the file. Separated so every rule about
/// what a pack may claim is testable from a string.
pub fn parse(text: &str, path: &Path, reserved: &[&str]) -> Result<Loaded, PackError> {
    if text.len() > MAX_PACK_BYTES {
        return Err(limit(
            path,
            format!("maximum file size is {MAX_PACK_BYTES} bytes"),
        ));
    }
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
    if raw.rules.len() > MAX_PACK_RULES {
        return Err(limit(
            path,
            format!("at most {MAX_PACK_RULES} rules per file are supported"),
        ));
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
    if id.len() > 80 {
        return Err(Rejected::Limit {
            id: "(overlong id)".into(),
            what: "id: 80 UTF-8 bytes",
        });
    }
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
    for (value, maximum, what) in [
        (raw.title.as_str(), 160, "title: 160 UTF-8 bytes"),
        (raw.detail.as_str(), 4096, "detail: 4096 UTF-8 bytes"),
        (raw.subject.as_str(), 80, "subject: 80 UTF-8 bytes"),
        (
            raw.directory.as_deref().unwrap_or(""),
            255,
            "directory: 255 UTF-8 bytes",
        ),
    ] {
        if value.len() > maximum {
            return Err(Rejected::Limit { id, what });
        }
    }
    if raw.beside.len() > 8 || raw.beside.iter().any(|name| name.len() > 255) {
        return Err(Rejected::Limit {
            id,
            what: "beside: 8 names, each at most 255 UTF-8 bytes",
        });
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

    let matcher = match (raw.directory, raw.location) {
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
        (None, Some(location)) => {
            if !raw.beside.is_empty() {
                return Err(Rejected::InvalidLocation {
                    id,
                    message: "beside applies only to a directory matcher",
                });
            }
            let location = location
                .validate()
                .map_err(|message| Rejected::InvalidLocation {
                    id: id.clone(),
                    message,
                })?;
            if !matches!(raw.action, RawAction::ReviewOnly) {
                return Err(Rejected::InvalidLocation {
                    id,
                    message: "location rules must use review-only",
                });
            }
            Matcher::Location(location)
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
            Matcher::Location(location) => self.match_location(tree, ctx, location),
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

    /// Resolve only the selected path in the immutable index. This never opens
    /// the path or reads metadata outside the already recorded scan.
    fn match_location(
        &self,
        tree: &SizeTree,
        ctx: &Context,
        location: &crate::location::Location,
    ) -> Vec<NodeId> {
        let Some(target) = location.target(tree, ctx) else {
            return Vec::new();
        };
        let Ok(rest) = target.strip_prefix(tree.root_path()) else {
            return Vec::new();
        };
        // The scanner already sorts direct children by encoded name bytes.
        // Resolve only the requested path with binary searches, instead of
        // traversing every arena entry once for every location rule.
        let mut id = tree.root();
        for component in rest.components() {
            let Some(child) = tree.child_named(id, component.as_os_str()) else {
                return Vec::new();
            };
            id = child;
        }
        if tree.node(id).is_dir() && util::is_countable(tree, id) {
            vec![id]
        } else {
            Vec::new()
        }
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
                        "location":{"base":"home","path":".config/mytool/cache"},
                        "action": "review-only"
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

    /// `..` in a home-based location would let a pack point at anything on the
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
                    "location":{{"base":"home","path":"{escape}"}},
                    "action": "review-only"
                }}"#
            ))
            .unwrap();
            assert!(
                matches!(
                    loaded.rejected.as_slice(),
                    [Rejected::InvalidLocation { .. }]
                ),
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
                    "location":{{"base":"home","path":"{absolute}"}},
                    "action": "review-only"
                }}"#
            ))
            .unwrap();
            assert!(
                matches!(
                    loaded.rejected.as_slice(),
                    [Rejected::InvalidLocation { .. }]
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
                "subject": "directory", "directory": "a", "location":{"base":"home","path":"b"},
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
                      "directory": "x", "location":{"base":"home","path":"y"}, "action": "review-only" },
                    { "id": "c", "title": "T", "detail": "D", "subject": "s",
                      "action": "review-only" },
                    { "id": "Bad Id", "title": "T", "detail": "D", "subject": "s",
                      "directory": "x", "action": "review-only" },
                    { "id": "d", "title": "T", "detail": "D", "subject": "s",
                      "location":{"base":"home","path":"../escape"}, "action": "review-only" }
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

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn document(first: usize, count: usize) -> String {
        serde_json::json!({"version":1,"rules":(first..first+count).map(|id|
            serde_json::json!({"id":format!("rule-{id}"),"title":"Review","detail":"Keep valuable data",
                "subject":"directory","directory":"sessions","action":"review-only"})
        ).collect::<Vec<_>>()}).to_string()
    }

    #[test]
    fn byte_and_rule_limits_refuse_the_whole_document_without_a_partial_success() {
        let path = Path::new("pack.json");
        let mut text = document(0, 1);
        text.extend(std::iter::repeat_n(' ', MAX_PACK_BYTES + 1 - text.len()));
        assert!(matches!(
            parse(&text, path, &[]),
            Err(PackError::Limit { .. })
        ));
        assert_eq!(
            parse(&document(0, MAX_PACK_RULES), path, &[])
                .unwrap()
                .rules
                .len(),
            MAX_PACK_RULES
        );
        assert!(matches!(
            parse(&document(0, MAX_PACK_RULES + 1), path, &[]),
            Err(PackError::Limit { .. })
        ));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.json");
        std::fs::write(&path, text).unwrap();
        assert!(matches!(read(&path, &[]), Err(PackError::Limit { .. })));
    }

    #[test]
    fn unknown_root_contract_is_not_silently_ignored() {
        assert!(matches!(
            parse(
                r#"{"version":1,"rules":[],"execute":"a command"}"#,
                Path::new("unknown.json"),
                &[]
            ),
            Err(PackError::Malformed { .. })
        ));
    }

    #[test]
    fn directory_budgets_are_deterministic_and_do_not_rewrite_inputs() {
        let dir = tempfile::tempdir().unwrap();
        // Create in reverse order: loading still reserves the first 64 IDs by filename.
        for index in (0..3).rev() {
            std::fs::write(
                dir.path().join(format!("{index}.json")),
                document(index * 32, 32),
            )
            .unwrap();
        }
        let loaded = load_directory(dir.path());
        assert_eq!(loaded.rules.len(), 64);
        assert_eq!(loaded.rules.first().unwrap().id, "rule-0");
        assert_eq!(loaded.rules.last().unwrap().id, "rule-63");
        assert_eq!(loaded.unreadable.len(), 1);
        assert!(loaded.problems()[0].contains("2.json"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("2.json")).unwrap(),
            document(64, 32)
        );
        for index in 3..=MAX_PACK_FILES {
            std::fs::write(dir.path().join(format!("{index}.json")), "{}").unwrap();
        }
        let refused = load_directory(dir.path());
        assert!(refused.rules.is_empty());
        assert!(refused.problems()[0].contains("no files loaded"));
    }

    #[test]
    fn excessive_marker_fanout_rejects_only_that_rule_and_diagnostics_remain_printable() {
        let mut value: serde_json::Value = serde_json::from_str(&document(0, 2)).unwrap();
        value["rules"][0]["beside"] =
            serde_json::json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]);
        let loaded = parse(&value.to_string(), Path::new("markers.json"), &[]).unwrap();
        assert_eq!(loaded.rules.len(), 1);
        assert_eq!(loaded.rules[0].id, "rule-1");
        assert!(matches!(loaded.rejected[0], Rejected::Limit { .. }));
        value["rules"][0]["id"] = serde_json::json!("bad\nforged");
        assert!(
            parse(&value.to_string(), Path::new("markers.json"), &[])
                .unwrap()
                .problems()
                .iter()
                .all(|s| !s.chars().any(char::is_control))
        );
    }

    #[test]
    fn directories_are_refused_before_attempting_a_read() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read(dir.path(), &[]),
            Err(PackError::Limit { .. })
        ));
    }

    #[test]
    fn field_limits_count_utf8_bytes_and_preserve_other_rules() {
        for (field, value) in [
            ("id", "x".repeat(81)),
            ("title", "é".repeat(81)),
            ("detail", "x".repeat(4097)),
            ("subject", "x".repeat(81)),
            ("directory", "x".repeat(256)),
        ] {
            let mut value_doc: serde_json::Value = serde_json::from_str(&document(0, 2)).unwrap();
            value_doc["rules"][0][field] = serde_json::json!(value);
            let result = parse(&value_doc.to_string(), Path::new("fields.json"), &[]).unwrap();
            assert_eq!(result.rules.len(), 1, "{field}");
            assert_eq!(result.rules[0].id, "rule-1", "{field}");
            assert!(
                matches!(result.rejected[0], Rejected::Limit { .. }),
                "{field}"
            );
        }
        let mut value: serde_json::Value = serde_json::from_str(&document(0, 1)).unwrap();
        value["rules"][0]["title"] = serde_json::json!("é".repeat(80));
        assert_eq!(
            parse(&value.to_string(), Path::new("boundary.json"), &[])
                .unwrap()
                .rules
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_pack_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.json");
        let linked = dir.path().join("linked.json");
        std::fs::write(&original, document(0, 1)).unwrap();
        std::os::unix::fs::symlink(&original, &linked).unwrap();
        assert!(matches!(read(&linked, &[]), Err(PackError::Limit { .. })));
        assert_eq!(std::fs::read_to_string(original).unwrap(), document(0, 1));
    }
}
