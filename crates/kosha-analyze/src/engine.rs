// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Run every rule, then rank and de-overlap the results.

use kosha_core::SizeTree;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::context::Context;
use crate::insight::Insight;
use crate::rules::{self, Rule, util};

/// Analyse a scanned tree.
///
/// Rules run in parallel — each is pure and sees the whole tree — and the
/// results are merged into one ranked list. Deterministic: the same tree and
/// the same [`Context`] always produce the same vector, in the same order.
pub fn analyze(tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
    let mut catalogue = rules::all();
    let imported = tree.stats().strategy == kosha_core::Strategy::Imported;
    if imported {
        catalogue.retain(|rule| !MEASURES_THIS_MACHINE.contains(&rule.id().as_str()));
    }

    let mut insights: Vec<Insight> = catalogue
        .par_iter()
        .flat_map_iter(|rule| rule.evaluate(tree, ctx))
        .collect();

    deduplicate_claims(tree, ctx, &mut insights);
    rank(&mut insights);
    nothing_inside_an_image_can_be_acted_on(tree.stats().strategy, &mut insights);
    nothing_imported_can_be_acted_on(tree.stats().strategy, &mut insights);
    only_printable_text(&mut insights);
    insights
}

/// Rules whose evidence is how this machine's filesystem allocates space —
/// cluster slack, sparse files, compression. An imported listing was measured
/// by another tool on another machine, so none of them is asked.
const MEASURES_THIS_MACHINE: &[&str] = &["cluster-waste", "sparse-savings", "compressible"];

/// A finding about an imported listing is advice about the machine the
/// listing came from. Nothing in it exists here to act on, so every finding
/// is review-only and says so.
fn nothing_imported_can_be_acted_on(strategy: kosha_core::Strategy, insights: &mut [Insight]) {
    if strategy != kosha_core::Strategy::Imported {
        return;
    }
    for insight in insights.iter_mut() {
        insight.action = crate::Suggested::ReviewOnly;
        insight.detail.push_str(
            " This comes from a listing imported from another tool and is about \
             the machine it describes; kosha did not measure it and nothing here \
             can be acted on.",
        );
    }
}

/// A finding about a disk image describes it and never offers to act on it.
///
/// **kosha offered to put a directory inside an ISO in the Recycle Bin.**
/// Measured, on an image built to look like an ordinary project:
///
/// ```text
/// [  80.0 MiB]  Build output
///               Certain · Trash · 1 project
///               · ...\cache.iso\proj\node_modules
/// [  55.0 MiB]  Logs in logs
///               Likely · Trash · 2 log files
///
/// 135 MiB reclaimable across confident findings
/// ```
///
/// Every path there is inside a file. None of them exists to the system
/// holding the image, nothing can be trashed, and the 135 MiB is the image
/// itself — one file the person never asked about. Pressing the button builds
/// a plan whose every item fails to resolve, which is safe and is not an
/// answer: the offer should not have been made, and the headline should not
/// have counted bytes nobody can get back.
///
/// The findings themselves are worth keeping, for the reason a snapshot's are
/// — "this image is mostly build output" is exactly what somebody scanning an
/// image wants to know — so this changes what is *offered*, not what is said.
/// `ReviewOnly` takes the button away in the window, drops the word Trash
/// from the command line's line, and removes these bytes from
/// [`total_reclaimable`], which counts destructive findings only.
///
/// The image is the only strategy this applies to. An elevated Master File
/// Table scan reads a live volume, so its paths are real and its offers work.
///
/// Takes the strategy rather than the tree so that it can be tested for what
/// it is: a tree that came out of an image cannot be built in a test here —
/// `SizeTree::from_parts` needs a name arena, which `kosha-core` keeps to
/// itself — and building a real ISO needs a tool this suite does not have.
/// The pass is the unit, and the wiring above is one line. What no unit test
/// covers — that `analyze` asks the tree where it came from, and that a scan
/// with `use_image` answers `DiskImage` — is covered by
/// `an_image_offers_nothing_to_act_on`, which takes a real image from the
/// environment and stands down where there is none.
fn nothing_inside_an_image_can_be_acted_on(
    strategy: kosha_core::Strategy,
    insights: &mut [Insight],
) {
    if strategy != kosha_core::Strategy::DiskImage {
        return;
    }
    for insight in insights.iter_mut().filter(|i| i.action.is_actionable()) {
        insight.action = crate::Suggested::ReviewOnly;
        insight.detail.push_str(
            " This is inside a disk image, which is one file to the system \
             holding it, so there is nothing here to act on.",
        );
    }
}

/// Every finding a front end should show, including the ones that read the disk.
///
/// [`analyze`] is the pure engine and stays that way: it is what the rule tests
/// call, and its determinism is the property the whole catalogue is built on.
/// But not every finding can be a rule — [`fragmented`](crate::fragmented) has
/// to open files — so the complete set is *this*, and it exists in one place so
/// that the window and the command line cannot answer the same question
/// differently. They did, briefly, and nothing would have caught it.
///
/// Anything appended here must be non-destructive: that is the condition under
/// which the engine's guarantees survive a finding it did not produce.
///
/// Loads the user's packs and **discards what it could not use**. A front end
/// that can show somebody why their rule did not load should call
/// [`crate::pack::load_all`] itself and pass the result to [`findings_with`];
/// this exists for callers that have nowhere to put the message.
pub fn findings(tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
    findings_with(tree, ctx, &crate::pack::load_all().rules)
}

/// [`findings`], against a given set of user rules rather than whatever is on
/// disk.
///
/// Separated so a front end that has already loaded the packs — because it
/// wants to *report* what was rejected — does not read and validate them a
/// second time on every rescan, and so the whole path is testable without a
/// config directory.
pub fn findings_with(
    tree: &SizeTree,
    ctx: &Context,
    user: &[crate::pack::PackRule],
) -> Vec<Insight> {
    let mut insights = analyze(tree, ctx);

    // User rules are evaluated by the same code as the built-ins and produce
    // the same `Insight`, so they are subject to every guarantee the engine
    // makes — including the de-overlapping, which is why they run *before* it
    // rather than being appended after.
    //
    // They cannot be part of `analyze` itself: that takes no arguments beyond
    // the tree and the context, and giving it a third would mean the pure
    // engine's output depended on a file on disk.
    if !user.is_empty() {
        let extra: Vec<Insight> = user
            .par_iter()
            .flat_map_iter(|rule| rule.evaluate(tree, ctx))
            .collect();
        if !extra.is_empty() {
            insights.extend(extra);
            deduplicate_claims(tree, ctx, &mut insights);
            rank(&mut insights);
        }
    }

    let imported = tree.stats().strategy == kosha_core::Strategy::Imported;
    if !imported
        && let Some(found) = crate::fragmented::find(tree, &crate::fragmented::Options::default())
    {
        insights.push(found);
        rank(&mut insights);
    }
    only_printable_text(&mut insights);
    insights
}

/// Replace the control characters in every finding's own prose.
///
/// **A finding's text is not always written by us.** A rule pack is a JSON file
/// people swap around, and its `title`, `detail` and `subject` are printed
/// verbatim by both front ends — so one pack's `detail` was enough to forge a
/// whole second finding in kosha's own layout, complete with a byte figure and
/// "confidence certain · trash 1 directory", while a bare escape went to the
/// terminal beside it. The built-in rules are no safer by construction: five of
/// them put a name or a path into their prose, and a name comes off a disk, out
/// of a disk image, or out of a snapshot taken on a machine where a newline in a
/// filename is legal.
///
/// Here rather than at each of those places because this is the seam every
/// finding crosses on its way to a person — including a pack's, including one
/// appended after the rules ran — and because the list of places was never going
/// to stay complete. `printable` is what the tree listing beside the panel has
/// always used; it is the same substitution `ls` makes.
///
/// Both exits do it: [`analyze`] is what the rule tests call, so the guarantee
/// belongs to the pure engine too and not only to the assembled set.
fn only_printable_text(insights: &mut [Insight]) {
    for insight in insights {
        for text in [
            &mut insight.title,
            &mut insight.detail,
            &mut insight.subject,
        ] {
            if text.chars().any(char::is_control) {
                *text = kosha_core::format::printable(text).into_owned();
            }
        }
    }
}

/// Order the panel: biggest confident win first.
///
/// Ties break on rule id and then title so the order never depends on thread
/// scheduling.
///
/// Public because not every finding comes from a rule. Anything that needs I/O
/// — [`duplicates`](crate::duplicates), [`fragmented`](crate::fragmented) —
/// runs outside [`analyze`] and is appended afterwards, and appending without
/// re-ranking would leave it wherever it happened to be pushed rather than
/// where it belongs. Such a finding must be non-destructive, which is what lets
/// it skip `deduplicate_claims` without breaking the guarantee that the panel's
/// figures can be added up.
pub fn rank(insights: &mut [Insight]) {
    insights.sort_by(|a, b| {
        b.score()
            .partial_cmp(&a.score())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.confidence.cmp(&a.confidence))
            .then_with(|| a.rule.as_str().cmp(b.rule.as_str()))
            .then_with(|| a.title.cmp(&b.title))
    });
}

/// Ensure no node is offered twice, so the panel's numbers can be added up.
///
/// Findings are visited most-confident first. Each keeps only the nodes no
/// earlier finding has already claimed — directly or through an ancestor — and
/// has its `reclaimable` recomputed from exactly what remains. A finding left
/// with nothing is dropped.
///
/// The alternative, dropping only findings that are *entirely* covered, leaves
/// partial overlaps double-counting their shared bytes. That is invisible until
/// something sums the panel, at which point the headline "reclaimable" figure
/// overstates by however much two rules happened to agree on — which is exactly
/// the number a user would act on.
///
/// Trimming exactly is only sound because a finding's node list is complete.
/// See the note on [`Insight::nodes`].
fn deduplicate_claims(tree: &SizeTree, ctx: &Context, insights: &mut Vec<Insight>) {
    let mut order: Vec<usize> = (0..insights.len()).collect();
    order.sort_by(|&a, &b| {
        insights[b]
            .confidence
            .cmp(&insights[a].confidence)
            .then_with(|| {
                insights[b]
                    .score()
                    .partial_cmp(&insights[a].score())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| insights[a].rule.as_str().cmp(insights[b].rule.as_str()))
    });

    let mut claimed: FxHashSet<kosha_core::NodeId> = FxHashSet::default();
    // Bytes already claimed *inside* a node, by a finding ranked above this one
    // or by a node above this one in the same finding.
    //
    // This is the half that was missing. Dropping a node whose ancestor is
    // claimed handles the overlap seen from below; seen from above, the ancestor
    // is still a directory whose `allocated` is its whole subtree, and the bytes
    // a more confident finding already offered are inside it. Measured on a
    // fixture where a user's pack rule earns `Certain` on a directory inside an
    // app cache: two findings offered 20,971,520 bytes out of a directory
    // holding 12,582,912.
    let mut claimed_below: FxHashMap<kosha_core::NodeId, u64> = FxHashMap::default();
    let mut keep = vec![true; insights.len()];

    for &i in &order {
        // Signposts describe rather than propose. They claim no bytes, so they
        // neither shrink anything nor get shrunk.
        if !insights[i].action.is_destructive() {
            continue;
        }

        let before = insights[i].nodes.len();

        // Decided parents first — the arena is breadth-first, so index order is
        // — which is what lets one finding holding a directory *and* something
        // inside it come out right as well.
        let mut by_index = insights[i].nodes.clone();
        by_index.sort_unstable_by_key(|n| n.index());

        // Nothing is committed until the finding is known to survive, because a
        // finding that is dropped must not leave its nodes claimed behind it.
        let mut mine: FxHashSet<kosha_core::NodeId> = FxHashSet::default();
        let mut mine_below: FxHashMap<kosha_core::NodeId, u64> = FxHashMap::default();
        let mut reclaimable = 0u64;

        for n in by_index {
            if claimed.contains(&n) || mine.contains(&n) {
                continue;
            }
            if util::has_claimed_ancestor(tree, n, &claimed)
                || util::has_claimed_ancestor(tree, n, &mine)
            {
                continue;
            }
            let taken = claimed_below.get(&n).copied().unwrap_or(0)
                + mine_below.get(&n).copied().unwrap_or(0);
            let contribution = tree.node(n).allocated.saturating_sub(taken);
            mine.insert(n);
            if contribution > 0 {
                let mut cur = n;
                while cur != kosha_core::NodeId::ROOT {
                    cur = kosha_core::NodeId::from_index(tree.node(cur).parent as usize);
                    *mine_below.entry(cur).or_insert(0) += contribution;
                }
            }
            reclaimable = reclaimable.saturating_add(contribution);
        }

        if mine.is_empty() || reclaimable < ctx.min_reclaimable {
            keep[i] = false;
            continue;
        }

        claimed.extend(mine.iter().copied());
        for (node, bytes) in mine_below {
            *claimed_below.entry(node).or_insert(0) += bytes;
        }
        // The display order the rule chose, minus what it no longer holds.
        let insight = &mut insights[i];
        insight.nodes.retain(|n| mine.contains(n));
        insight.superseded = before - insight.nodes.len();
        insight.reclaimable = reclaimable;
    }

    let mut iter = keep.iter();
    insights.retain(|_| *iter.next().unwrap_or(&true));
}

/// Total on-disk bytes across every finding the user could safely act on.
///
/// Counts only `Certain` and `Likely` findings with a destructive action — the
/// number the UI puts in the insights panel headline.
///
/// Summing is sound because no *byte* is claimed twice — which is a stronger
/// statement than no node appearing twice, and the difference was a bug. A
/// directory's `allocated` is its whole subtree, so two findings with disjoint
/// node lists still counted the same bytes twice whenever one held a directory
/// and the other held something inside it. See [`deduplicate_claims`].
pub fn total_reclaimable(insights: &[Insight]) -> u64 {
    insights
        .iter()
        .filter(|i| i.action.is_destructive() && i.confidence >= crate::Confidence::Likely)
        .map(|i| i.reclaimable)
        .fold(0u64, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Confidence, RuleId, Severity, Suggested};
    use kosha_core::Strategy;

    fn finding(action: Suggested) -> Insight {
        Insight {
            rule: RuleId::new("build-output"),
            title: "Build output".into(),
            detail: "Rebuilding recreates it.".into(),
            subject: "project".into(),
            nodes: vec![kosha_core::NodeId::from_index(1)],
            superseded: 0,
            reclaimable: 80 * 1024 * 1024,
            confidence: Confidence::Certain,
            action,
            severity: Severity::Notice,
        }
    }

    /// Inside an image there is nothing to act on, so nothing offers to.
    ///
    /// **kosha offered to put a directory inside an ISO in the Recycle Bin.**
    /// Measured on an image built to look like an ordinary project: "Build
    /// output — Certain · Trash" over `cache.iso\proj\node_modules`, "Logs in
    /// logs — Likely · Trash" under it, and "135 MiB reclaimable across
    /// confident findings" beneath both. Every one of those paths is inside a
    /// file; none exists to the system holding the image; and the 135 MiB is
    /// the image itself, one file nobody asked about. The button builds a
    /// plan whose every item fails to resolve, which is safe and is not an
    /// answer.
    #[test]
    fn an_image_turns_every_offer_into_something_to_read() {
        let mut insights = vec![
            finding(Suggested::Trash),
            finding(Suggested::EmptyContents),
            finding(Suggested::Compress),
            finding(Suggested::ReviewOnly),
        ];
        let before = insights.len();

        nothing_inside_an_image_can_be_acted_on(Strategy::DiskImage, &mut insights);

        assert_eq!(
            insights.len(),
            before,
            "a finding was dropped, not softened"
        );
        for insight in &insights {
            assert_eq!(insight.action, Suggested::ReviewOnly);
        }
        // The three that changed say why; the one that was already for
        // reading is left exactly as its rule wrote it.
        assert_eq!(
            insights
                .iter()
                .filter(|i| i.detail.contains("inside a disk image"))
                .count(),
            3
        );
        assert_eq!(total_reclaimable(&insights), 0);
    }

    /// And every other strategy is left alone.
    ///
    /// A Master File Table scan reads a live volume: its paths are real, its
    /// offers work, and softening them would take the whole feature away from
    /// the fastest scan kosha has.
    #[test]
    fn a_scan_of_a_real_disk_still_offers_to_act() {
        for strategy in [Strategy::DirectoryWalk, Strategy::MasterFileTable] {
            let mut insights = vec![finding(Suggested::Trash)];
            nothing_inside_an_image_can_be_acted_on(strategy, &mut insights);
            assert_eq!(insights[0].action, Suggested::Trash, "{strategy:?}");
            assert_eq!(insights[0].detail, "Rebuilding recreates it.");
            assert!(total_reclaimable(&insights) > 0);
        }
    }

    /// A listing imported from another tool is advice about that machine:
    /// no rule measuring this machine's allocation runs, and nothing offers
    /// to act.
    #[test]
    fn an_imported_listing_offers_nothing_and_skips_allocation_rules() {
        use kosha_core::{Entry, Flags, ScanStats, TreeBuilder};
        let dir = Entry {
            flags: Flags::DIR,
            ..Entry::default()
        };
        let mut b = TreeBuilder::new("home", dir).unwrap();
        let project = b.push(0, "project", dir).unwrap();
        b.push(
            project,
            "Cargo.toml",
            Entry {
                logical: 100,
                allocated: 4096,
                ..Entry::default()
            },
        )
        .unwrap();
        let target = b.push(project, "target", dir).unwrap();
        for index in 0..400 {
            b.push(
                target,
                &format!("obj{index}.o"),
                Entry {
                    logical: 3 << 20,
                    allocated: (3 << 20) + 600_000,
                    ..Entry::default()
                },
            )
            .unwrap();
        }
        let stats = ScanStats {
            strategy: Strategy::Imported,
            ..ScanStats::default()
        };
        let (tree, _) = b.finish("/home".into(), stats).unwrap();
        let insights = analyze(&tree, &Context::default());
        for insight in &insights {
            assert_eq!(insight.action, Suggested::ReviewOnly, "{}", insight.title);
            assert!(insight.detail.contains("imported from another tool"));
            assert!(
                !MEASURES_THIS_MACHINE.contains(&insight.rule.as_str()),
                "{} measured this machine",
                insight.rule.as_str()
            );
        }
        assert_eq!(total_reclaimable(&insights), 0);
        assert!(crate::fragmented::find(&tree, &Default::default()).is_none());
    }
}
