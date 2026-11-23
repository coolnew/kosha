// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Things that were useful once.

use kosha_core::{NodeId, SizeTree, format};
use rustc_hash::FxHashMap;

use crate::context::Context;
use crate::insight::{Confidence, Insight, RuleId, Severity, Suggested};
use crate::rules::Rule;
use crate::rules::util;

const INSTALLER_EXTS: &[&str] = &[
    "dmg", "iso", "exe", "msi", "msix", "pkg", "deb", "rpm", "appimage",
];
const INSTALLER_MIN_BYTES: u64 = 100 * 1024 * 1024;

/// Downloaded installers nobody opened again.
///
/// Restricted to the download and desktop directories on purpose: a 300 MB
/// `.exe` in a build output directory is a product, not litter.
pub struct StaleInstallers;

impl Rule for StaleInstallers {
    fn id(&self) -> RuleId {
        RuleId::new("stale-installers")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        const PLACES: &[&str] = &["Downloads", "Desktop", "downloads", "desktop"];

        let mut found = Vec::new();
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if node.is_dir()
                || node.allocated < INSTALLER_MIN_BYTES
                || !util::is_countable(tree, id)
                || !ctx.older_than_days(node.mtime, ctx.stale_installer_days)
            {
                continue;
            }
            if !util::has_extension(tree, id, INSTALLER_EXTS) {
                continue;
            }
            if !util::under_directory_named(tree, id, PLACES) {
                continue;
            }
            found.push(id);
        }

        let reclaimable = util::total_allocated(tree, &found);
        if found.is_empty() || reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.id(),
            title: "Old installers".into(),
            subject: "installer".into(),
            // States the threshold rather than the observed oldest file. This
            // finding is destructive, so the engine may trim nodes out of it
            // after this sentence is written, and the threshold stays true for
            // whatever survives where "the oldest is 3 months old" would not.
            detail: format!(
                "In Downloads or on the Desktop, none touched in over {} days. \
                 Installers are re-downloadable by definition.",
                ctx.stale_installer_days,
            ),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            confidence: Confidence::Likely,
            action: Suggested::Trash,
            severity: Severity::Notice,
        }]
    }
}

/// Big files that have not changed in a very long time.
///
/// Deliberately `ReviewOnly`: "large and old" describes a finished video
/// project as accurately as it describes junk, and only the user can tell them
/// apart.
pub struct LargeAndCold;

impl Rule for LargeAndCold {
    fn id(&self) -> RuleId {
        RuleId::new("large-and-cold")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let mut found = Vec::new();
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if !node.is_dir()
                && node.allocated >= ctx.cold_file_bytes
                && util::is_countable(tree, id)
                && ctx.older_than_days(node.mtime, ctx.cold_file_days)
            {
                found.push(id);
            }
        }

        let reclaimable = util::total_allocated(tree, &found);
        if found.is_empty() || reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.id(),
            title: "Large files untouched for over a year".into(),
            subject: "file".into(),
            detail: "Each is over 1 GiB and has not been modified in more than a year. \
                 Worth archiving somewhere cheaper — or keeping, if you know what \
                 they are."
                .into(),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            confidence: Confidence::Possible,
            action: Suggested::ReviewOnly,
            severity: Severity::Info,
        }]
    }
}

const LOG_DIR_MIN_BYTES: u64 = 50 * 1024 * 1024;

/// Log files, aggregated by the directory holding them.
///
/// Per-file reporting would produce hundreds of tiny findings; the actionable
/// unit is "this directory has 900 MB of logs in it".
pub struct LogFiles;

impl Rule for LogFiles {
    fn id(&self) -> RuleId {
        RuleId::new("log-files")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let mut by_dir: FxHashMap<u32, (u64, Vec<NodeId>)> = FxHashMap::default();

        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if node.is_dir() || !util::is_countable(tree, id) {
                continue;
            }
            if !is_log_name(tree, id) {
                continue;
            }
            let entry = by_dir.entry(node.parent).or_default();
            entry.0 = entry.0.saturating_add(node.allocated);
            entry.1.push(id);
        }

        let mut insights: Vec<Insight> = by_dir
            .into_iter()
            .filter(|(_, (bytes, _))| *bytes >= LOG_DIR_MIN_BYTES && *bytes >= ctx.min_reclaimable)
            .map(|(dir, (bytes, files))| {
                let dir_id = NodeId::from_index(dir as usize);
                Insight {
                    rule: self.id(),
                    title: format!("Logs in {}", tree.name_lossy(dir_id)),
                    subject: "log file".into(),
                    detail: format!(
                        "Under {}. Logs are append-only history; deleting them \
                         loses the record but breaks nothing.",
                        tree.path_of(dir_id).display(),
                    ),
                    nodes: util::sort_by_size(tree, files),
                    superseded: 0,
                    reclaimable: bytes,
                    confidence: Confidence::Likely,
                    action: Suggested::Trash,
                    severity: Severity::Notice,
                }
            })
            .collect();

        // Stable order regardless of hash-map iteration.
        insights.sort_unstable_by(|a, b| {
            b.reclaimable
                .cmp(&a.reclaimable)
                .then_with(|| a.title.cmp(&b.title))
        });
        insights
    }
}

/// `app.log`, `app.log.3`, `app.log.2024-01-01.gz` — rotation puts the
/// interesting part in the middle of the name, so an extension test alone is
/// not enough.
fn is_log_name(tree: &SizeTree, id: NodeId) -> bool {
    tree.name(id)
        .as_encoded_bytes()
        .split(|&byte| byte == b'.')
        .skip(1)
        .any(|part| part.eq_ignore_ascii_case(b"log"))
}

/// Virtual machine and container disk images.
const VM_IMAGE_EXTS: &[&str] = &[
    "vdi",
    "vmdk",
    "qcow",
    "qcow2",
    "vhd",
    "vhdx",
    "hdd",
    "ova",
    "vbox-prev",
];

/// Images below this are stubs and snapshots, not somebody's machine.
const VM_IMAGE_MIN_BYTES: u64 = 128 * 1024 * 1024;

/// Virtual machine images nobody has booted in a long time.
///
/// `ReviewOnly` despite high confidence in the *observation*: a stale VM image
/// is frequently the largest reclaimable thing on a developer's disk, and just
/// as frequently somebody's entire working environment. Those two are
/// indistinguishable from the outside, so this rule points and explains rather
/// than offering a button.
pub struct OrphanedVmImages;

impl Rule for OrphanedVmImages {
    fn id(&self) -> RuleId {
        RuleId::new("orphaned-vm-images")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let mut found = Vec::new();
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if node.is_dir()
                || node.allocated < VM_IMAGE_MIN_BYTES
                || !util::is_countable(tree, id)
                || !ctx.older_than_days(node.mtime, ctx.vm_image_days)
                || !util::has_extension(tree, id, VM_IMAGE_EXTS)
            {
                continue;
            }
            found.push(id);
        }

        let reclaimable = util::total_allocated(tree, &found);
        if found.is_empty() || reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        let oldest = found
            .iter()
            .map(|&id| tree.node(id).mtime)
            .min()
            .unwrap_or(ctx.now);

        vec![Insight {
            rule: self.id(),
            title: "Virtual machine images".into(),
            subject: "disk image".into(),
            detail: format!(
                "None written to in over {} days, the oldest {}. These are often \
                 the largest reclaimable thing on a disk, and sometimes an entire \
                 working environment. Check what each one is before removing it.",
                ctx.vm_image_days,
                format::age(oldest, ctx.now),
            ),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            confidence: Confidence::Likely,
            action: Suggested::ReviewOnly,
            severity: Severity::Notice,
        }]
    }
}
