// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Files stored in the cloud, inside a plan that would delete them.
//!
//! Sync apps usually copy a local deletion to the online account. For a file
//! whose data is only in the cloud, the local entry is the handle for what may
//! be the only copy, so moving it to the Trash is not the reversible act the
//! word suggests.
//!
//! Three things live here, and each is a rule rather than advice:
//!
//! - [`CloudRisk`] is computed by [`ActionPlan::build`](crate::ActionPlan::build)
//!   from the scanned tree, with no filesystem call: the evidence is the
//!   operating system's own placeholder marker, which the scanner already
//!   recorded as [`Flags::OFFLINE`]. Never a name or a path convention.
//! - [`CloudWarning`] is the *only* source of the words a confirmation shows, so
//!   a front end can draw them but cannot write a softer version.
//! - [`CloudConfirmed`] is the token a plan with cloud-only files needs before
//!   it will execute. Like [`UserConfirmed`](crate::UserConfirmed), it is made
//!   in one named place, and it can only be made from a warning that a plan
//!   produced: it is bound to that plan and to the count the user was shown.

use std::sync::atomic::{AtomicU64, Ordering};

use kosha_core::{Flags, NodeId, SizeTree, format};

use crate::plan::Action;

/// What says that a planned item involves files stored in the cloud.
///
/// Only markers the operating system reports. A folder called "OneDrive" is
/// not evidence, and neither is a path that looks like a sync root: renamed
/// roots and unrelated folders make name matches wrong in both directions.
///
/// `non_exhaustive` so that evidence from an operating-system sync-root API can
/// be added later without changing a caller. Each kind says for itself whether
/// it needs the separate confirmation, through
/// [`requires_confirmation`](CloudEvidence::requires_confirmation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum CloudEvidence {
    /// The scanner saw the operating system's placeholder marker
    /// ([`Flags::OFFLINE`]): Windows' `OFFLINE` and `RECALL_ON_*` attributes,
    /// or macOS's `SF_DATALESS`. The file's data is not on this computer.
    OfflinePlaceholder,
}

impl CloudEvidence {
    /// Whether this evidence is strong enough that the plan must not execute
    /// without a [`CloudConfirmed`].
    pub fn requires_confirmation(self) -> bool {
        match self {
            CloudEvidence::OfflinePlaceholder => true,
        }
    }

    /// The word the audit log records.
    pub fn audit_name(self) -> &'static str {
        match self {
            CloudEvidence::OfflinePlaceholder => "offline",
        }
    }
}

/// Cloud risk of one item in a plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ItemCloudRisk {
    /// Cloud-only files at or below the item. Directories are never counted.
    pub offline_files: u64,
    pub evidence: CloudEvidence,
    /// The item is itself a cloud-only file, rather than a folder holding some.
    pub is_cloud_only_file: bool,
}

/// Cloud risk of a whole plan, present only when some item has any.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CloudRisk {
    pub(crate) plan_id: u64,
    pub(crate) action: Action,
    pub(crate) offline_files: u64,
    pub(crate) items_at_risk: usize,
    pub(crate) plan_items: usize,
    pub(crate) single_cloud_file: bool,
    pub(crate) skippable: bool,
    pub(crate) evidence: CloudEvidence,
}

impl CloudRisk {
    /// Cloud-only files the plan would act on, across every item.
    pub fn offline_files(&self) -> u64 {
        self.offline_files
    }

    /// How many of the plan's items hold at least one cloud-only file.
    pub fn items_at_risk(&self) -> usize {
        self.items_at_risk
    }

    pub fn evidence(&self) -> CloudEvidence {
        self.evidence
    }

    /// Whether the plan refuses to execute without a [`CloudConfirmed`].
    pub fn requires_confirmation(&self) -> bool {
        self.evidence.requires_confirmation()
    }

    /// Whether "Skip cloud-only items" is offered: a Trash or permanent
    /// delete with something left to act on once the cloud-only files are
    /// taken out. See [`ActionPlan::without_cloud_only`](crate::ActionPlan::without_cloud_only).
    pub fn can_skip_cloud_only(&self) -> bool {
        self.skippable
    }

    /// The words to show. The one place they are written.
    pub fn warning(&self) -> CloudWarning {
        CloudWarning::for_risk(self)
    }
}

/// The confirmation a plan with cloud-only files needs, as words.
///
/// Every string is produced by [`CloudWarning::for_risk`] and nowhere else, so
/// the front end cannot leave the warning out or soften it: it can only draw
/// these. Plain and hedged, because Kosha cannot see what a sync app will do —
/// "usually" and "may" are the accurate words, not caution for its own sake.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CloudWarning {
    plan_id: u64,
    offline_files: u64,
    title: String,
    body: String,
    confirm_label: String,
    skip_label: Option<&'static str>,
}

/// The skip button's label, when skipping is offered.
pub const SKIP_CLOUD_ONLY_LABEL: &str = "Skip cloud-only items";

impl CloudWarning {
    /// Every word of the warning, from one function.
    fn for_risk(risk: &CloudRisk) -> CloudWarning {
        let n = risk.offline_files;
        let one = n == 1;
        let files = if one {
            "1 file".to_string()
        } else {
            format!("{} files", format::count(n))
        };
        let cloud_files = if one {
            "1 cloud-only file".to_string()
        } else {
            format!("{} cloud-only files", format::count(n))
        };
        let is_are = if one { "is" } else { "are" };

        let title = if risk.single_cloud_file {
            "This file is stored in the cloud".to_string()
        } else if risk.plan_items == 1 {
            format!("{files} in this folder {is_are} stored in the cloud")
        } else {
            format!("{files} in this selection {is_are} stored in the cloud")
        };

        let (subject, pronoun, copy, only) = if one {
            ("It is", "it", "the online copy", "it may be the only copy")
        } else {
            (
                "They are",
                "them",
                "the online copies",
                "they may be the only copies",
            )
        };
        let trash = matches!(risk.action, Action::Trash);
        let doing = if trash {
            format!("Moving {pronoun} to the Trash")
        } else {
            format!("Deleting {pronoun}")
        };
        let body = format!(
            "{subject} not kept on this computer. {doing} usually deletes {copy} too, \
             and {only}."
        );

        let confirm_label = match (trash, risk.single_cloud_file) {
            (true, true) => format!("Move {cloud_files} to the Trash"),
            (true, false) => format!("Move everything, including {cloud_files}"),
            (false, true) => format!("Delete {cloud_files} permanently"),
            (false, false) => format!("Delete everything, including {cloud_files}"),
        };

        CloudWarning {
            plan_id: risk.plan_id,
            offline_files: n,
            title,
            body,
            confirm_label,
            skip_label: risk.skippable.then_some(SKIP_CLOUD_ONLY_LABEL),
        }
    }

    /// A heading, naming the count.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// What happens, in one or two sentences.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The label of the one button that may produce a [`CloudConfirmed`]. It
    /// names the number of cloud-only files, so the click is a statement about
    /// them rather than a reflex.
    pub fn confirm_label(&self) -> &str {
        &self.confirm_label
    }

    /// The label of the button that rebuilds the plan without the cloud-only
    /// files, or `None` where that is not offered.
    pub fn skip_label(&self) -> Option<&'static str> {
        self.skip_label
    }

    /// Cloud-only files the warning names.
    pub fn offline_files(&self) -> u64 {
        self.offline_files
    }
}

/// Evidence that the user was shown a [`CloudWarning`] and chose to go ahead.
///
/// No `Clone`, no `Default`, and the only constructor takes the warning, which
/// only a plan can produce. The token is bound to that plan and to the count
/// the warning named: a token from a different plan, or from a plan that
/// counted differently, does not satisfy this one.
#[derive(Debug)]
pub struct CloudConfirmed {
    plan_id: u64,
    offline_files: u64,
}

impl CloudConfirmed {
    /// Call this only from the branch where the user pressed the button
    /// labelled [`CloudWarning::confirm_label`] — never to satisfy the type
    /// checker.
    pub fn from_dialog_acceptance(warning: &CloudWarning) -> Self {
        CloudConfirmed {
            plan_id: warning.plan_id,
            offline_files: warning.offline_files,
        }
    }

    pub(crate) fn matches(&self, risk: &CloudRisk) -> bool {
        self.plan_id == risk.plan_id && self.offline_files == risk.offline_files
    }
}

/// A process-unique identity for each built plan, so a token cannot be reused.
pub(crate) fn next_plan_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Is this node a cloud-only file?
#[inline]
fn is_cloud_only_file(tree: &SizeTree, id: NodeId) -> bool {
    let node = tree.node(id);
    !node.is_dir() && node.flags.contains(Flags::OFFLINE)
}

/// Count cloud-only files at or below `id`, and remember which they are.
///
/// Iterative, over the immutable arena, with no filesystem call. `stack` is
/// reused across a plan's items, so the walk allocates nothing per node;
/// `found` grows only with cloud-only files. Directories are never counted —
/// the scanner never marks one, and counting one would be counting a folder as
/// a file.
pub(crate) fn walk_offline(
    tree: &SizeTree,
    id: NodeId,
    stack: &mut Vec<NodeId>,
    found: &mut Vec<NodeId>,
) -> u64 {
    stack.clear();
    stack.push(id);
    let mut count = 0u64;
    while let Some(next) = stack.pop() {
        let node = tree.node(next);
        if node.is_dir() {
            stack.extend(tree.children(next));
        } else if node.flags.contains(Flags::OFFLINE) {
            count += 1;
            found.push(next);
        }
    }
    count
}

/// The risk of one item, or `None` for an action that removes nothing, or an
/// item with no cloud-only file below it.
pub(crate) fn item_risk(
    tree: &SizeTree,
    id: NodeId,
    action: Action,
    stack: &mut Vec<NodeId>,
    found: &mut Vec<NodeId>,
) -> Option<ItemCloudRisk> {
    if !action.is_destructive() {
        return None;
    }
    let offline_files = walk_offline(tree, id, stack, found);
    (offline_files > 0).then(|| ItemCloudRisk {
        offline_files,
        evidence: CloudEvidence::OfflinePlaceholder,
        is_cloud_only_file: is_cloud_only_file(tree, id),
    })
}

/// The largest pieces of `targets` that hold no cloud-only file.
///
/// A target with none is kept whole. A cloud-only file is dropped. A folder
/// holding some is kept on disk and replaced by its children, each treated the
/// same way — so the result is exactly what is left to act on once the
/// cloud-only files are taken out, and nothing that holds one.
///
/// `offline` lists the cloud-only files found under the targets (from
/// [`walk_offline`]). Their ancestors are marked once, in a sorted list bounded
/// by files × depth, so deciding whether to descend is a binary search instead
/// of another walk. Iterative throughout.
pub(crate) fn cloud_free_pieces(
    tree: &SizeTree,
    targets: &[NodeId],
    offline: &[NodeId],
) -> Vec<NodeId> {
    let mut holding: Vec<NodeId> = Vec::new();
    for &file in offline {
        let mut cursor = file;
        while cursor != NodeId::ROOT {
            let parent = NodeId::from_index(tree.node(cursor).parent as usize);
            if parent == cursor {
                break;
            }
            // Marking ancestors above a target is harmless: the descent
            // below only ever visits nodes inside a target.
            holding.push(parent);
            cursor = parent;
        }
    }
    holding.sort_unstable();
    holding.dedup();

    let mut kept = Vec::new();
    let mut stack = Vec::new();
    for &target in targets {
        stack.push(target);
        while let Some(next) = stack.pop() {
            if is_cloud_only_file(tree, next) {
                continue;
            }
            if tree.node(next).is_dir() && holding.binary_search(&next).is_ok() {
                // Reversed so the pieces come out in the scan's order.
                let children: core::ops::Range<usize> = tree.node(next).child_range();
                stack.extend(children.rev().map(NodeId::from_index));
                continue;
            }
            kept.push(next);
        }
    }
    kept
}
