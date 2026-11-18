// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Build a plan, show it, then execute it.
//!
//! Nothing is ever deleted straight from a click. The UI assembles an
//! [`ActionPlan`] — concrete paths, a total, a disposition per item — runs it as
//! a dry run to populate the confirmation dialog, and only executes after the
//! user has read what it says. "Delete 91,204 files" should be a sentence you
//! get to read before it happens.

use std::fs;
use std::path::{Path, PathBuf};

use kosha_core::{NodeId, SizeTree};

use crate::audit::AuditLog;
use crate::guard::{self, Refusal};
use crate::mode::{ReadWriteGrant, Session};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Move to the platform trash. Reversible, and the default for everything.
    Trash,
    /// Unlink. Not reversible; needs its own confirmation in the UI.
    DeletePermanently,
    /// Remove everything inside a directory but keep the directory, which is
    /// what a cache's owning tool expects to still find.
    EmptyContents,
    /// Turn on filesystem compression. Nothing is deleted and nothing moves.
    ///
    /// The odd one out here: every other action reclaims space by removing
    /// something, and this one reclaims it by storing the same bytes in fewer.
    /// The file stays exactly where it is, readable by everything, and the
    /// action undoes cleanly — which is why it is the only mutation the app
    /// offers that is not about losing something.
    Compress,
}

impl Action {
    pub fn is_reversible(self) -> bool {
        matches!(self, Action::Trash | Action::Compress)
    }

    /// True when the action removes something rather than storing it smaller.
    ///
    /// The dialog leans on this: "compress 4,102 files" does not deserve the
    /// same warning as "delete 4,102 files", and treating them alike would
    /// teach people to click through the warning that matters.
    pub fn is_destructive(self) -> bool {
        !matches!(self, Action::Compress)
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Trash => "Move to trash",
            Action::DeletePermanently => "Delete permanently",
            Action::EmptyContents => "Empty contents",
            Action::Compress => "Compress",
        }
    }
}

/// What we believed about a path when the plan was built.
///
/// Re-checked at execution time. If any of it changed, the item is skipped
/// rather than acted on: a scan is an observation, and acting on a stale
/// observation is how a tool deletes the wrong thing.
///
/// **The timestamp is kept whole, which it was not.** It used to be truncated to
/// Unix seconds, copying the width the *tree* stores — and a tree stores one
/// number per entry for a million entries, which is a reason this struct does
/// not have. One item's fingerprint is compared against nothing but itself, so
/// the precision is free, and without it a file rewritten to the same length
/// inside the same second read as unchanged and was deleted. Between building a
/// plan and running it there is a confirmation dialog, so the window is normally
/// wider than a second — but a plan executed straight away had one, and a cache
/// file being rewritten by the program that owns it is the ordinary case rather
/// than a contrived one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fingerprint {
    pub len: u64,
    /// Whole precision, straight from the filesystem, compared for equality and
    /// never arithmetic. `None` where the platform does not keep one, which
    /// compares equal to `None` and leaves the rest to do the work.
    pub modified: Option<std::time::SystemTime>,
    pub is_dir: bool,
    /// What the filesystem itself calls this file, where it will say: the device
    /// and inode on Unix.
    ///
    /// This is the one part of a fingerprint that a *different* file cannot
    /// imitate. Length, type and timestamp can all match by coincidence or by
    /// restoration — `rsync -t` and an unpacked archive both put an old
    /// timestamp on new bytes — and then nothing else here would notice.
    ///
    /// `None` on Windows, and that is a gap rather than a decision. `file_index`
    /// and `volume_serial_number` are `std::os::windows::fs::MetadataExt`
    /// methods that are still unstable, and the stable way to them is
    /// `GetFileInformationByHandle` — a second `unsafe` block in the crate that
    /// allows exactly one, for a check the whole-precision timestamp already
    /// covers on every filesystem with a timestamp finer than the window. That
    /// does not justify adding unsafe code to the mutation boundary.
    pub identity: Option<(u64, u64)>,
}

impl Fingerprint {
    fn of(path: &Path) -> std::io::Result<Self> {
        let md = fs::symlink_metadata(path)?;
        Ok(Fingerprint {
            len: md.len(),
            modified: md.modified().ok(),
            is_dir: md.is_dir(),
            identity: identity(&md),
        })
    }
}

/// The device and inode, which name a file rather than describe it.
#[cfg(unix)]
fn identity(md: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((md.dev(), md.ino()))
}

/// See [`Fingerprint::identity`]: not available from stable `std`, and not worth
/// a second block of `unsafe` in this crate for what it would add here.
#[cfg(not(unix))]
fn identity(_md: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub path: PathBuf,
    pub action: Action,
    /// On-disk bytes this item is expected to free.
    pub bytes: u64,
    pub expected: Fingerprint,
}

/// A reviewed, executable set of changes.
#[derive(Clone, Debug)]
pub struct ActionPlan {
    root: PathBuf,
    items: Vec<PlanItem>,
    /// Items that could not be planned at all, kept so the UI can explain the
    /// gap rather than quietly showing a smaller number.
    rejected: Vec<(PathBuf, String)>,
}

impl ActionPlan {
    /// Build a plan from nodes of a scanned tree.
    ///
    /// Guard checks run here, so a plan can only ever contain paths that were
    /// already acceptable — and they run again at execution time, because the
    /// filesystem may have changed in between.
    pub fn build(tree: &SizeTree, nodes: &[NodeId], action: Action, session: &Session) -> Self {
        let root = session.root().to_path_buf();
        let mut items = Vec::with_capacity(nodes.len());
        let mut rejected = Vec::new();

        for &id in nodes {
            let path = tree.path_of(id);
            match guard::check_for(&path, &root, Some(action)) {
                Ok(resolved) => match Fingerprint::of(&resolved) {
                    Ok(expected) => items.push(PlanItem {
                        path: resolved,
                        action,
                        bytes: tree.node(id).allocated,
                        expected,
                    }),
                    Err(e) => rejected.push((path, e.to_string())),
                },
                Err(refusal) => rejected.push((path, refusal.to_string())),
            }
        }

        ActionPlan {
            root,
            items,
            rejected,
        }
    }

    pub fn items(&self) -> &[PlanItem] {
        &self.items
    }

    /// The action every item in this plan carries.
    ///
    /// A plan is built with one action for all of its items, so this is total
    /// rather than a sample — and the dialog needs it to know whether the
    /// figure it is about to show is a promise or an unknown.
    pub fn action(&self) -> Option<Action> {
        self.items.first().map(|item| item.action)
    }

    pub fn rejected(&self) -> &[(PathBuf, String)] {
        &self.rejected
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Total on-disk bytes this plan expects to free.
    pub fn total_bytes(&self) -> u64 {
        self.items
            .iter()
            .map(|i| i.bytes)
            .fold(0u64, u64::saturating_add)
    }

    /// True when any item cannot be undone from the trash.
    pub fn has_irreversible(&self) -> bool {
        self.items.iter().any(|i| !i.action.is_reversible())
    }

    /// Validate everything and report what *would* happen, touching nothing.
    ///
    /// Needs no grant, because it changes nothing. The UI runs this to fill the
    /// confirmation dialog, so the numbers the user approves come out of the
    /// real code path rather than a parallel estimate that can drift.
    pub fn dry_run(&self) -> ExecutionReport {
        self.run(None)
    }

    /// Execute. Requires read-write mode, expressed as a grant.
    pub fn execute(&self, grant: &ReadWriteGrant<'_>, audit: &mut AuditLog) -> ExecutionReport {
        self.run(Some((grant, audit)))
    }

    fn run(&self, live: Option<(&ReadWriteGrant<'_>, &mut AuditLog)>) -> ExecutionReport {
        let dry_run = live.is_none();
        let (_grant, mut audit) = match live {
            Some((g, a)) => (Some(g), Some(a)),
            None => (None, None),
        };

        let mut report = ExecutionReport {
            dry_run,
            ..Default::default()
        };

        for item in &self.items {
            let mut reclaimed = item.bytes;
            let outcome = match self.verify(item) {
                Err(reason) => Outcome::Skipped(reason),
                Ok(()) if dry_run => Outcome::WouldDo(item.action),
                Ok(()) => match apply(item) {
                    // What an action actually reclaimed, which for compression
                    // is not knowable until it has run: the ratio depends on
                    // the bytes. Deleting reclaims what the plan said.
                    Ok(Applied::Whole(bytes)) => {
                        reclaimed = bytes;
                        Outcome::Done(item.action)
                    }
                    Ok(Applied::Partial { freed, why }) => {
                        reclaimed = freed;
                        Outcome::Partial {
                            action: item.action,
                            why: why.to_string(),
                        }
                    }
                    Err(e) => Outcome::Failed(e.to_string()),
                },
            };

            // A partial is recorded too, and for the reason the log exists:
            // the files it did remove are gone, and a log that lists only the
            // clean cases is a log that cannot be used to work out what
            // happened.
            if let Some(audit) = audit.as_deref_mut()
                && matches!(outcome, Outcome::Done(_) | Outcome::Partial { .. })
            {
                audit.record(&item.path, item.action, reclaimed, &outcome);
            }

            report.tally(item, reclaimed, outcome);
        }
        report
    }

    /// Re-check an item immediately before acting on it.
    fn verify(&self, item: &PlanItem) -> Result<(), SkipReason> {
        let resolved = guard::check_for(&item.path, &self.root, Some(item.action))
            .map_err(SkipReason::Refused)?;
        if resolved != item.path {
            return Err(SkipReason::Moved);
        }
        let current = Fingerprint::of(&item.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SkipReason::Vanished
            } else {
                SkipReason::Refused(Refusal::Unresolvable {
                    path: item.path.clone(),
                    source: e,
                })
            }
        })?;
        if current != item.expected {
            return Err(SkipReason::Changed);
        }
        Ok(())
    }
}

/// Carry out one item, and report the on-disk bytes it actually reclaimed.
///
/// Returning the figure rather than assuming the plan's is what lets
/// compression be honest: how much a file shrinks depends entirely on its
/// bytes, and is not knowable until it has happened.
fn apply(item: &PlanItem) -> std::io::Result<Applied> {
    match item.action {
        Action::Trash => trash::delete(&item.path)
            .map(|()| Applied::Whole(item.bytes))
            .map_err(|e| std::io::Error::other(e.to_string())),
        Action::DeletePermanently => {
            // Asked of the filesystem rather than of the plan, because the
            // plan's `is_dir` came from a scan and says *what a link points
            // at*, not what the link is. A junction is a directory to Win32 and
            // a not-directory to the scanner, and taking the plan's word for it
            // sent a junction to `remove_file`, which answers "Access is
            // denied" — measured, and it is how deleting a junction failed.
            let kind = fs::symlink_metadata(&item.path)?.file_type();
            match remove_one(&item.path, kind) {
                Ok(()) => Ok(Applied::Whole(item.bytes)),
                // **`remove_dir_all` is not all-or-nothing.** Measured, a
                // directory of three files with one of them held open: the
                // other two and the subdirectory holding them were permanently
                // deleted, the call returned the error for the one that
                // stayed, and the report said "did nothing across 0 items, 1
                // failed" about 12 KiB that no longer existed. Same sentence
                // as the empty's, on the action that cannot be undone.
                //
                // Nothing having gone is still a plain failure, which is what
                // a file that could not be deleted at all comes back as: there
                // is nothing there to have freed.
                Err(why) => match freed_from(&item.path, item.bytes) {
                    0 => Err(why),
                    freed => Ok(Applied::Partial { freed, why }),
                },
            }
        }
        Action::Compress => compress_tree(&item.path),
        Action::EmptyContents => {
            // The directory itself survives; only what is inside it goes.
            //
            // Removed outright rather than trashed, for two reasons. The trash
            // lives on the same volume, so moving 250 GB of cache into it frees
            // exactly nothing — which is the entire thing the user asked for.
            // And this action is already typed irreversible, so trashing here
            // would have the implementation quietly contradict the type and the
            // warning the confirmation dialog shows.
            //
            // What makes that acceptable is what the action is restricted to:
            // contents of a directory a cache rule matched, listed in full and
            // approved before anything runs.
            let mut last_error = None;
            let mut removed = 0usize;
            for entry in fs::read_dir(&item.path)? {
                let entry = entry?;
                let outcome = match entry.file_type() {
                    Ok(kind) => remove_one(&entry.path(), kind),
                    Err(e) => Err(e),
                };
                match outcome {
                    Ok(()) => removed += 1,
                    Err(e) => last_error = Some(e),
                }
            }
            match (last_error, removed) {
                (None, _) => Ok(Applied::Whole(item.bytes)),
                // Nothing went, so there is nothing to report but the reason.
                (Some(e), 0) => Err(e),
                // **Some of it went, and saying "did nothing" about that is a
                // false statement concerning files that no longer exist.**
                // Measured, emptying a directory with one file held open the
                // way a running browser holds its own cache: two of three files
                // permanently deleted, and the report read `succeeded=0
                // failed=1, freed=0` over the sentence "did nothing across 0
                // items, 1 failed" — while the audit log, which records only
                // `Done`, recorded nothing at all.
                (Some(why), _) => Ok(Applied::Partial {
                    freed: freed_from(&item.path, item.bytes),
                    why,
                }),
            }
        }
    }
}

/// What an action managed.
enum Applied {
    /// All of it, reclaiming this many bytes.
    Whole(u64),
    /// Some of it, with the reason the rest did not go.
    Partial { freed: u64, why: std::io::Error },
}

/// How much of a directory's bytes are gone, by measuring what is left.
///
/// The figure has to be about what actually happened rather than about what
/// was planned, and the honest way to get it is to ask the disk the same
/// question the scan asked it: on-disk bytes, by the same scanner, so the two
/// numbers are comparable at all. What is left after an empty is the handful of
/// entries that would not go, so this costs a walk of those and nothing else.
///
/// `saturating_sub`, because a directory somebody else is writing to can end up
/// holding more than the scan saw, and a negative saving is not a thing to
/// report. A scan that cannot be done at all gives zero, which understates
/// rather than invents.
fn freed_from(path: &Path, was: u64) -> u64 {
    let left = kosha_core::Scanner::new(kosha_core::ScanOptions::default())
        .scan(path)
        .map(|tree| tree.total_allocated())
        .unwrap_or(was);
    was.saturating_sub(left)
}

/// Remove one entry, whatever kind it is, and never what a link points at.
///
/// **A link is not the thing it names, and Windows calls one of them a
/// directory.** `remove_file` on a junction answers "Access is denied", which
/// is how two things failed: deleting a junction outright, and emptying any
/// directory that contained one — the second reporting "Access is denied" over
/// a directory whose other contents it had already deleted.
///
/// A junction rather than a symlink because that is what a Windows user can
/// actually make: a symlink needs developer mode or an elevated process, a
/// junction needs neither, and `mklink /J` is in every "move your cache
/// elsewhere" recipe on the internet.
///
/// `remove_dir` on the link, never `remove_dir_all`: the link is an empty
/// directory as far as the call is concerned, and what it points at is not
/// this plan's to touch. Measured: the target and its contents survive.
fn remove_one(path: &Path, kind: fs::FileType) -> std::io::Result<()> {
    if kind.is_symlink() {
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTypeExt;
            if kind.is_symlink_dir() {
                return fs::remove_dir(path);
            }
        }
        return fs::remove_file(path);
    }
    if kind.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Compress a file, or everything inside a directory.
///
/// Setting the attribute on a directory affects only files written to it
/// afterwards, so compressing a directory somebody pointed at means walking it.
/// The attribute is set on the directory too, so what lands there later is
/// compressed without being asked again.
///
/// Iterative rather than recursive: a plan can name a directory of arbitrary
/// depth, and the scanner avoids recursion for exactly this reason.
///
/// Symbolic links are never followed. Compressing a link's target would act on
/// something outside the plan the user approved, which is the one thing every
/// guard in this crate exists to prevent.
///
/// A file that cannot be compressed does not stop the rest. Compression is not
/// destructive, so a partial result is a partial saving rather than a
/// half-finished deletion — and it is reported as one: the saving it did make,
/// with the error beside it. It used to return the error alone, which threw
/// the saving away and told somebody who had just recovered a gigabyte that
/// the item had failed and saved nothing.
fn compress_tree(root: &Path) -> std::io::Result<Applied> {
    let mut saved: u64 = 0;
    let mut last_error = None;
    let mut stack = vec![root.to_path_buf()];

    while let Some(path) = stack.pop() {
        let md = match fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        };
        if md.file_type().is_symlink() {
            continue;
        }

        if md.is_dir() {
            // Failing to set it on the directory is not fatal: the files inside
            // are the space, and the attribute is only about future ones.
            if let Err(e) = crate::compress::set(&path, true) {
                last_error = Some(e);
            }
            match fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => stack.push(entry.path()),
                            Err(e) => last_error = Some(e),
                        }
                    }
                }
                Err(e) => last_error = Some(e),
            }
            continue;
        }

        // Measured, not estimated: `FSCTL_SET_COMPRESSION` compresses the data
        // already in the file before it returns, so the difference either side
        // of the call is exactly what was saved.
        let before = kosha_core::allocated_size(&path).unwrap_or(0);
        match crate::compress::set(&path, true) {
            Ok(()) => {
                let after = kosha_core::allocated_size(&path).unwrap_or(before);
                saved = saved.saturating_add(before.saturating_sub(after));
            }
            Err(e) => last_error = Some(e),
        }
    }

    match (last_error, saved) {
        (None, saved) => Ok(Applied::Whole(saved)),
        // Nothing was compressed, so there is nothing but the reason.
        (Some(e), 0) => Err(e),
        // **Some of it was, and it saved what it saved.** The same shape as an
        // empty that could not finish: a walk of many files, one of them held
        // open by whatever is using it, and a report that said the whole item
        // failed and saved nothing. Less grave here — compression loses
        // nothing — but the figure was still wrong, and it is the figure
        // somebody checks their disk against.
        (Some(why), freed) => Ok(Applied::Partial { freed, why }),
    }
}

/// Why an item was not acted on.
#[derive(Debug)]
pub enum SkipReason {
    /// It is no longer there. Someone else got to it first, which is fine.
    Vanished,
    /// Its size, timestamp or type differs from what the scan saw.
    Changed,
    /// The path now resolves somewhere else.
    Moved,
    /// A guard refused it.
    Refused(Refusal),
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::Vanished => f.write_str("already gone"),
            SkipReason::Changed => f.write_str("changed since the scan"),
            SkipReason::Moved => f.write_str("moved since the scan"),
            SkipReason::Refused(r) => write!(f, "{r}"),
        }
    }
}

#[derive(Debug)]
pub enum Outcome {
    Done(Action),
    /// Part of it happened and part of it did not.
    ///
    /// **Only an action on a directory's contents can land here**, because it
    /// is the only one made of more than one removal — and it is the one people
    /// use on a browser cache while the browser is running. It was reported as
    /// a plain failure, which is a false statement about files that are gone.
    Partial {
        action: Action,
        why: String,
    },
    WouldDo(Action),
    Skipped(SkipReason),
    Failed(String),
}

/// What actually happened.
///
/// Execution is per-item and never transactional — the filesystem offers no way
/// to make it so, and pretending otherwise would be worse. Failures are
/// reported, not rounded off to "Done".
#[derive(Debug, Default)]
pub struct ExecutionReport {
    pub dry_run: bool,
    pub succeeded: usize,
    /// Items that were carried out in part: some of a directory's contents
    /// removed and some not.
    ///
    /// Neither a success nor a failure, and reported as neither. Rounding it
    /// into `failed` is what produced "did nothing across 0 items, 1 failed"
    /// over two files that had been permanently deleted.
    pub partial: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Bytes removed outright, and therefore actually available again.
    pub bytes_freed: u64,
    /// Bytes moved to the trash.
    ///
    /// Tracked apart from `bytes_freed` because they are *not* the same thing:
    /// the trash lives on the same volume, so a trashed file still occupies its
    /// space until the trash is emptied. Reporting the two together would tell
    /// a user they had reclaimed gigabytes that their disk still holds.
    pub bytes_trashed: u64,
    /// On-disk bytes the plan touched, however it disposed of them.
    ///
    /// Separate from `bytes_freed` because for compression the two are
    /// different questions: this is what the files occupied going in, and
    /// `bytes_freed` is what came back — which a dry run cannot know, because
    /// the ratio depends entirely on the bytes.
    pub bytes_examined: u64,
    /// Per-item detail, in plan order.
    pub details: Vec<(PathBuf, Outcome)>,
}

impl ExecutionReport {
    /// Bytes this plan affected, however they were disposed of.
    pub fn bytes_total(&self) -> u64 {
        self.bytes_freed.saturating_add(self.bytes_trashed)
    }

    fn tally(&mut self, item: &PlanItem, reclaimed: u64, outcome: Outcome) {
        match &outcome {
            Outcome::Done(_) | Outcome::WouldDo(_) => {
                self.succeeded += 1;
                self.bytes_examined = self.bytes_examined.saturating_add(item.bytes);

                // A dry run of a compression knows nothing about the saving —
                // it depends on the bytes, and nothing has read them. Leaving
                // `bytes_freed` at zero is what stops the dialog promising a
                // figure it invented; `bytes_examined` is what it shows instead.
                // Only trashing leaves the bytes on the volume. This asked
                // `is_reversible` before, which was the same question only
                // while `Trash` was the only reversible action — compression
                // is reversible too, and the space it saves really is back.
                if matches!(item.action, Action::Trash) {
                    self.bytes_trashed = self.bytes_trashed.saturating_add(reclaimed);
                } else if !matches!(outcome, Outcome::WouldDo(Action::Compress)) {
                    self.bytes_freed = self.bytes_freed.saturating_add(reclaimed);
                }
            }
            // Counted apart from both: calling it a success would overstate
            // what happened and calling it a failure would deny the bytes that
            // really are gone. The bytes it did free are counted, because they
            // are freed whatever the item is called.
            Outcome::Partial { .. } => {
                self.partial += 1;
                self.bytes_examined = self.bytes_examined.saturating_add(item.bytes);
                if matches!(item.action, Action::Trash) {
                    self.bytes_trashed = self.bytes_trashed.saturating_add(reclaimed);
                } else {
                    self.bytes_freed = self.bytes_freed.saturating_add(reclaimed);
                }
            }
            Outcome::Skipped(_) => self.skipped += 1,
            Outcome::Failed(_) => self.failed += 1,
        }
        self.details.push((item.path.clone(), outcome));
    }

    /// One line for the status bar.
    ///
    /// Says "moved to the trash" rather than "freed" where that is what
    /// happened, because on every supported platform the trash is on the same
    /// volume and the space does not come back until it is emptied.
    pub fn summary(&self) -> String {
        use kosha_core::format::bytes;

        let mut parts = Vec::new();
        if self.bytes_freed > 0 {
            parts.push(format!(
                "{} {}",
                if self.dry_run { "would free" } else { "freed" },
                bytes(self.bytes_freed),
            ));
        }
        if self.bytes_trashed > 0 {
            parts.push(format!(
                "{} {} to the trash",
                if self.dry_run { "would move" } else { "moved" },
                bytes(self.bytes_trashed),
            ));
        }
        if parts.is_empty() {
            parts.push(format!(
                "{} nothing",
                if self.dry_run { "would do" } else { "did" }
            ));
        }

        // "across 0 items" is a phrase for a count that is really zero, and a
        // partial item is not that: something was removed and no item came out
        // of it whole.
        let mut s = if self.succeeded == 0 && self.partial > 0 {
            parts.join(" and ")
        } else {
            format!(
                "{} across {}",
                parts.join(" and "),
                kosha_core::format::plural(self.succeeded, "item")
            )
        };
        if self.bytes_trashed > 0 {
            s.push_str(" — empty the trash to reclaim that space");
        }
        if self.partial > 0 {
            s.push_str(&format!(
                ", {} only partly — something in there could not be removed",
                self.partial
            ));
        }
        if self.skipped > 0 {
            s.push_str(&format!(", {} skipped", self.skipped));
        }
        if self.failed > 0 {
            s.push_str(&format!(", {} failed", self.failed));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The timestamp is the filesystem's, not a rounded version of it.
    ///
    /// Deterministic, and it is the assertion that bites if anybody truncates
    /// this again: a fingerprint built from whole seconds would hold
    /// `UNIX_EPOCH + secs`, which is not what the filesystem said.
    #[test]
    fn the_timestamp_is_kept_exactly_as_the_filesystem_gave_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, vec![b'a'; 64]).unwrap();

        let raw = std::fs::symlink_metadata(&path)
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(Fingerprint::of(&path).unwrap().modified, Some(raw));
    }

    /// A rewrite is the same file, and the part of the fingerprint that names a
    /// file must go on naming it.
    ///
    /// What a rewrite changes is the *timestamp*, and how soon it changes is the
    /// filesystem's business rather than this crate's: two writes a microsecond
    /// apart came back with the identical `FILETIME` on the machine this was
    /// written on, because NTFS takes its last-write time from the system clock
    /// and that ticks about every 15 ms. So the resolution is asserted where it
    /// can be — in `tests/execute.rs`, with a wait between the two writes that is
    /// far longer than a tick and far shorter than the second the fingerprint
    /// used to round to.
    #[test]
    fn a_rewrite_is_not_a_different_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, vec![b'a'; 64]).unwrap();
        let before = Fingerprint::of(&path).unwrap();

        std::fs::write(&path, vec![b'b'; 64]).unwrap();
        let after = Fingerprint::of(&path).unwrap();

        assert_eq!(before.len, after.len);
        assert_eq!(
            before.identity, after.identity,
            "a rewrite is not a different file"
        );
    }

    /// And two files are two files.
    ///
    /// Unix only, because [`Fingerprint::identity`] is `None` on Windows — which
    /// is a gap this asserts the shape of rather than papers over: on Windows the
    /// timestamp above is the whole of the answer.
    #[cfg(unix)]
    #[test]
    fn two_files_are_never_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        std::fs::write(&one, b"same length..").unwrap();
        std::fs::write(&two, b"same length..").unwrap();

        let one = Fingerprint::of(&one).unwrap();
        let two = Fingerprint::of(&two).unwrap();
        assert!(
            one.identity.is_some(),
            "a Unix file has a device and an inode"
        );
        assert_ne!(
            one.identity, two.identity,
            "two files on one filesystem shared an identity"
        );
    }

    /// A hardlink is the same file under another name, and deleting one name is
    /// not deleting the file — so identity alone must never be what a plan
    /// compares. This is here to say that the other fields are still in it.
    #[cfg(unix)]
    #[test]
    fn a_hardlink_is_the_same_file_and_the_fingerprint_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        std::fs::write(&one, b"contents").unwrap();
        std::fs::hard_link(&one, &two).unwrap();

        assert_eq!(
            Fingerprint::of(&one).unwrap(),
            Fingerprint::of(&two).unwrap(),
            "two names for one file described differently"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_gap_is_where_it_is_documented_to_be() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"x").unwrap();
        // If this ever starts returning `Some`, the doc comment on
        // `Fingerprint::identity` is out of date and the check got stronger.
        assert_eq!(Fingerprint::of(&path).unwrap().identity, None);
    }
}
