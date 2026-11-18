// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Space lost to how the filesystem stores things, and shape observations that
//! tell the user where to look next.

use kosha_core::{Flags, NodeId, SizeTree, format};
use rustc_hash::FxHashMap;

use crate::context::Context;
use crate::insight::{Confidence, Insight, RuleId, Severity, Suggested};
use crate::rules::Rule;
use crate::rules::util;

/// Bytes lost to cluster slack.
///
/// A directory of 200,000 tiny files can waste gigabytes without any single
/// file looking large: each one occupies a whole allocation unit regardless of
/// how little it contains. The fix is an archive or a different filesystem, not
/// a deletion — so this is always `ReviewOnly`.
pub struct ClusterWaste;

impl Rule for ClusterWaste {
    fn id(&self) -> RuleId {
        RuleId::new("cluster-waste")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let mut by_dir: FxHashMap<u32, (u64, u32)> = FxHashMap::default();
        let mut total: u64 = 0;

        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            // Sparse and compressed files have allocated < logical, so they
            // never contribute here; `wasted` saturates to zero for them.
            if node.is_dir() || node.flags.contains(Flags::SPARSE) {
                continue;
            }
            let wasted = node.wasted();
            if wasted == 0 {
                continue;
            }
            total = total.saturating_add(wasted);
            let entry = by_dir.entry(node.parent).or_default();
            entry.0 = entry.0.saturating_add(wasted);
            entry.1 += 1;
        }

        if total < ctx.min_reclaimable {
            return Vec::new();
        }

        let mut worst: Vec<(u32, u64, u32)> =
            by_dir.into_iter().map(|(d, (w, n))| (d, w, n)).collect();
        worst.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        worst.truncate(16);

        let nodes: Vec<NodeId> = worst
            .iter()
            .map(|&(d, _, _)| NodeId::from_index(d as usize))
            .collect();
        let worst_dir = nodes.first().copied();

        vec![Insight {
            rule: self.id(),
            title: "Space lost to cluster slack".into(),
            subject: "directory".into(),
            detail: match worst_dir {
                Some(d) => format!(
                    "Small files each occupy a whole allocation unit, so their \
                     on-disk size exceeds their contents. The worst offender is \
                     {} with {} of slack across {}. Archiving such a \
                     directory recovers most of it.",
                    tree.name_lossy(d),
                    format::bytes_in(worst[0].1, ctx.units),
                    format::plural(worst[0].2 as usize, "file"),
                ),
                None => "Small files each occupy a whole allocation unit.".into(),
            },
            nodes,
            superseded: 0,
            reclaimable: total,
            confidence: Confidence::Possible,
            action: Suggested::ReviewOnly,
            severity: Severity::Info,
        }]
    }
}

/// Follow the biggest child down for as long as one child dominates.
///
/// This is the "you do not need to browse, look here" hint, and it answers the
/// only question most users actually have.
pub struct DominantChild;

const DOMINANCE: f32 = 0.7;

impl Rule for DominantChild {
    fn id(&self) -> RuleId {
        RuleId::new("single-dominant-child")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let root_total = tree.total_allocated();
        if root_total < ctx.min_reclaimable {
            return Vec::new();
        }

        let mut chain = Vec::new();
        let mut cur = tree.root();
        // Long enough to be a useful hint, short enough for a person to read.
        while chain.len() < 12
            && let Some(biggest) = tree.children(cur).max_by_key(|&c| tree.node(c).allocated)
        {
            if !tree.node(biggest).is_dir() || tree.fraction_of_parent(biggest) < DOMINANCE {
                break;
            }
            chain.push(biggest);
            cur = biggest;
        }

        if chain.is_empty() {
            return Vec::new();
        }

        let deepest = *chain.last().unwrap();
        let bytes = tree.node(deepest).allocated;
        let share = format::percent(bytes, root_total);

        vec![Insight {
            rule: self.id(),
            title: format!("{share} of everything sits under one directory"),
            subject: "directory".into(),
            detail: format!(
                "{} holds {} — {} of the scanned total. Start here.",
                tree.path_of(deepest).display(),
                format::bytes_in(bytes, ctx.units),
                share,
            ),
            nodes: chain,
            superseded: 0,
            reclaimable: 0, // A signpost, not a saving.
            confidence: Confidence::Certain,
            action: Suggested::ReviewOnly,
            severity: Severity::Info,
        }]
    }
}

const DEEP: u8 = 24;

/// Pathological nesting, which is almost always a tool that ran away.
pub struct DeepNesting;

impl Rule for DeepNesting {
    fn id(&self) -> RuleId {
        RuleId::new("deep-nesting")
    }

    fn evaluate(&self, tree: &SizeTree, _ctx: &Context) -> Vec<Insight> {
        // Directories only. The deepest *node* in a runaway tree is the file at
        // the bottom of it, which made the insight call a file a directory and
        // report one level more nesting than there are directories. Caught by
        // the first test this rule ever had.
        let deepest = tree
            .nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_dir() && n.depth >= DEEP)
            .max_by_key(|(_, n)| n.depth);

        let Some((index, node)) = deepest else {
            return Vec::new();
        };
        let id = NodeId::from_index(index);

        // **The deepest thing found is not always the deepest thing there.** A
        // walk that stopped at its ceiling knows nothing about what is below it,
        // so a depth of 255 in such a tree is the ceiling and not a measurement.
        // Measured on this machine's `%LOCALAPPDATA%`: the scan stopped at 255
        // and the tree goes past 300. Saying "255 levels" flatly there is the
        // same mistake as calling a truncated scan's total the folder's size.
        let stopped = tree.stats().depth_limited && node.depth == u8::MAX;
        let at_least = if stopped { "at least " } else { "" };

        vec![Insight {
            rule: self.id(),
            title: format!("Directories nested {at_least}{} levels deep", node.depth),
            subject: "directory".into(),
            detail: format!(
                "{} sits {at_least}{} levels below the scan root{}. Nesting like \
                 this usually means a tool recursed into itself, and the deepest \
                 paths may be hard for other programs to open at all.",
                tree.path_of(id).display(),
                node.depth,
                if stopped {
                    ", which is where the walk stopped — there may be more below"
                } else {
                    ""
                },
            ),
            nodes: vec![id],
            superseded: 0,
            reclaimable: 0,
            confidence: Confidence::Certain,
            action: Suggested::ReviewOnly,
            severity: Severity::Info,
        }]
    }
}

/// Space that sparse files and filesystem compression are already saving.
///
/// Pure context, and it earns its place by preventing a misreading: a 4 GB VM
/// image occupying 200 MB looks like 3.8 GB of waste if you only compare the
/// two size columns. This says what is actually happening.
pub struct SparseSavings;

impl Rule for SparseSavings {
    fn id(&self) -> RuleId {
        RuleId::new("sparse-savings")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let mut saved: u64 = 0;
        let mut found = Vec::new();

        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if node.is_dir() || !node.flags.contains(Flags::SPARSE) {
                continue;
            }
            let savings = node.logical.saturating_sub(node.allocated);
            if savings == 0 {
                continue;
            }
            saved = saved.saturating_add(savings);
            found.push(id);
        }

        if found.is_empty() || saved < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.id(),
            title: format!(
                "{} already saved by sparse files",
                format::bytes_in(saved, ctx.units)
            ),
            subject: "file".into(),
            detail: "These report more bytes than they occupy, because the filesystem \
                 stores them sparsely or compressed. Their apparent size is not \
                 disk you can reclaim — it is disk you never spent."
                .into(),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            // Not reclaimable at all: this is space you already have.
            reclaimable: 0,
            confidence: Confidence::Certain,
            action: Suggested::ReviewOnly,
            severity: Severity::Info,
        }]
    }
}

/// Extensions whose contents are text, or otherwise highly compressible.
const COMPRESSIBLE_EXTS: &[&str] = &[
    "txt", "log", "json", "xml", "csv", "tsv", "sql", "md", "yaml", "yml", "html", "htm", "css",
    "js", "ts", "c", "h", "cpp", "hpp", "rs", "py", "go", "java", "rb", "svg", "ndjson", "jsonl",
];

/// A directory has to hold at least this much before compressing it is worth
/// suggesting.
const COMPRESSIBLE_MIN_BYTES: u64 = 256 * 1024 * 1024;

/// The share of a directory's bytes that must be compressible before it counts.
const COMPRESSIBLE_SHARE: f64 = 0.7;

/// Conservative estimate of what filesystem compression recovers on text. Real
/// ratios are usually better; overstating a saving would be worse than
/// understating one.
const COMPRESSION_ESTIMATE: f64 = 0.5;

/// Directories dominated by text, where filesystem compression would pay.
pub struct Compressible;

impl Rule for Compressible {
    fn id(&self) -> RuleId {
        RuleId::new("compressible")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        // Measured over each directory's own files rather than its whole
        // subtree, so the finding names the directory that actually holds the
        // text instead of some ancestor that merely contains it.
        let mut totals: FxHashMap<u32, (u64, u64)> = FxHashMap::default();

        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if node.is_dir() || !util::is_countable(tree, id) {
                continue;
            }
            // Already-compressed files would make the ratio lie.
            if node.flags.contains(Flags::SPARSE) {
                continue;
            }
            let entry = totals.entry(node.parent).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(node.allocated);
            if util::has_extension(tree, id, COMPRESSIBLE_EXTS) {
                entry.1 = entry.1.saturating_add(node.allocated);
            }
        }

        let mut found = Vec::new();
        let mut compressible_bytes: u64 = 0;
        for (dir, (total, compressible)) in totals {
            if total < COMPRESSIBLE_MIN_BYTES {
                continue;
            }
            if (compressible as f64) < total as f64 * COMPRESSIBLE_SHARE {
                continue;
            }
            found.push(NodeId::from_index(dir as usize));
            compressible_bytes = compressible_bytes.saturating_add(compressible);
        }

        let estimate = (compressible_bytes as f64 * COMPRESSION_ESTIMATE) as u64;
        if found.is_empty() || estimate < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.id(),
            title: "Text that would compress well".into(),
            subject: "directory".into(),
            detail: format!(
                "Holding {} of mostly text, which filesystem compression typically \
                 halves. The estimate is deliberately conservative; the files stay \
                 readable and nothing is deleted.",
                format::bytes_in(compressible_bytes, ctx.units),
            ),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable: estimate,
            confidence: Confidence::Possible,
            action: Suggested::Compress,
            severity: Severity::Info,
        }]
    }
}
