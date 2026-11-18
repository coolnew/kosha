// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The rules added to complete the documented catalogue.
//!
//! Each rule gets a case that should fire and, where the rule leans on a
//! heuristic rather than a marker, a case that should not. The negatives matter
//! more than the positives here: a rule that produces false positives teaches
//! users to ignore the panel, which costs more than the rule was worth.

use std::fs;
use std::path::Path;

use kosha_analyze::{Confidence, Context, Insight, RuleId, Suggested, analyze};
use kosha_core::{ScanOptions, Scanner, SizeTree};

const MB: usize = 1024 * 1024;

fn ctx() -> Context {
    let now = kosha_core::format::now_unix() + 10 * 365 * 86_400;
    Context {
        min_reclaimable: 1,
        ..Context::at(now)
    }
}

fn ctx_fresh() -> Context {
    Context {
        min_reclaimable: 1,
        ..Context::at(kosha_core::format::now_unix())
    }
}

fn scan(root: &Path) -> SizeTree {
    Scanner::new(ScanOptions::default()).scan(root).unwrap()
}

fn write(path: &Path, megabytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![0u8; megabytes * MB]).unwrap();
}

fn find<'a>(insights: &'a [Insight], rule: &'static str) -> Option<&'a Insight> {
    insights.iter().find(|i| i.rule == RuleId::new(rule))
}

mod browser_cache {
    use super::*;

    #[test]
    fn cache_under_a_browser_profile_is_certain() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("Google/Chrome/User Data/Default/Cache/data_1"),
            40,
        );

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let found = find(&insights, "browser-cache").expect("cache under a Chrome tree");

        assert_eq!(found.confidence, Confidence::Certain);
        assert_eq!(found.action, Suggested::EmptyContents);
        assert_eq!(found.nodes.len(), 1);
        assert_eq!(tree.name_lossy(found.nodes[0]), "Cache");
    }

    #[test]
    fn nested_browser_caches_are_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("Mozilla/Firefox/Profiles/p1/Cache/entries/a"),
            20,
        );
        write(
            &root.join("Mozilla/Firefox/Profiles/p1/Cache/Code Cache/b"),
            20,
        );

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let found = find(&insights, "browser-cache").unwrap();

        assert_eq!(
            found.nodes.len(),
            1,
            "the inner Code Cache is already inside the outer Cache's bytes"
        );
    }

    #[test]
    fn offline_application_data_is_not_touched() {
        // These sit beside the caches and hold logins and unsaved work. Naming
        // them would be the single most damaging false positive available.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let profile = root.join("Google/Chrome/User Data/Default");
        write(&profile.join("Local Storage/leveldb/000003.log"), 40);
        write(&profile.join("IndexedDB/store/data"), 40);
        write(&profile.join("Service Worker/CacheStorage/x"), 40);

        let tree = scan(root);
        assert!(find(&analyze(&tree, &ctx()), "browser-cache").is_none());
    }

    #[test]
    fn a_cache_directory_with_no_browser_above_it_is_not_a_browser_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("SomeApp/Cache/blob"), 40);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());

        assert!(find(&insights, "browser-cache").is_none());
        // It is still an application cache, just a less confident one.
        let app = find(&insights, "app-cache").expect("still a cache by name");
        assert_eq!(app.confidence, Confidence::Likely);
    }
}

mod app_cache {
    use super::*;

    #[test]
    fn browser_caches_are_left_to_their_own_rule() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("Google/Chrome/User Data/Default/Cache/data_1"),
            40,
        );
        write(&root.join("SomeApp/Caches/blob"), 30);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let app = find(&insights, "app-cache").unwrap();

        assert_eq!(app.nodes.len(), 1, "only the non-browser cache");
        assert_eq!(tree.name_lossy(app.nodes[0]), "Caches");
    }

    #[test]
    fn is_never_certain() {
        // "cache" is a naming convention, not a guarantee, and this rule has no
        // marker file to lean on.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("SomeApp/Cache/blob"), 40);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        assert_eq!(
            find(&insights, "app-cache").unwrap().confidence,
            Confidence::Likely
        );
    }
}

mod trash {
    use super::*;

    #[test]
    fn the_recycle_bin_is_reported_but_not_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("$Recycle.Bin/S-1-5-21/deleted.bin"), 40);

        let tree = scan(root);
        let found = {
            let insights = analyze(&tree, &ctx());
            find(&insights, "trash-not-emptied").cloned()
        }
        .expect("a recycle bin full of data");

        assert_eq!(found.confidence, Confidence::Certain);
        assert_eq!(
            found.action,
            Suggested::ReviewOnly,
            "emptying the trash belongs to the OS, which knows its bookkeeping"
        );
    }

    #[test]
    fn the_xdg_trash_is_found_by_its_full_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".local/share/Trash/files/old.bin"), 40);
        // A directory merely called Trash somewhere else is not the trash.
        write(&root.join("project/Trash/notes.bin"), 40);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let found = find(&insights, "trash-not-emptied").unwrap();

        assert_eq!(found.nodes.len(), 1);
        assert!(
            found.nodes.iter().all(|&id| {
                let path = tree.path_of(id);
                path.to_string_lossy().contains("share")
            }),
            "matched a directory that merely happens to be named Trash"
        );
    }

    #[test]
    fn does_not_contribute_to_the_reclaimable_headline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".Trash/big.bin"), 200);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        assert!(find(&insights, "trash-not-emptied").is_some());
        assert_eq!(
            kosha_analyze::total_reclaimable(&insights),
            0,
            "a ReviewOnly finding must not inflate the actionable total"
        );
    }
}

mod vm_images {
    use super::*;

    #[test]
    fn a_cold_disk_image_is_reported_for_review() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("VMs/ubuntu.qcow2"), 200);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let found = find(&insights, "orphaned-vm-images").unwrap();

        assert_eq!(found.confidence, Confidence::Likely);
        assert_eq!(
            found.action,
            Suggested::ReviewOnly,
            "a stale VM image is as often a working environment as it is junk"
        );
        assert_eq!(found.nodes.len(), 1);
    }

    #[test]
    fn a_recently_used_image_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("VMs/ubuntu.qcow2"), 200);

        let tree = scan(root);
        assert!(find(&analyze(&tree, &ctx_fresh()), "orphaned-vm-images").is_none());
    }

    #[test]
    fn small_images_are_not_worth_reporting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Snapshots and stubs, not somebody's machine.
        write(&root.join("VMs/snapshot.vmdk"), 4);

        let tree = scan(root);
        assert!(find(&analyze(&tree, &ctx()), "orphaned-vm-images").is_none());
    }
}

mod compressible {
    use super::*;

    fn write_text(path: &Path, megabytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'a'; megabytes * MB]).unwrap();
    }

    #[test]
    fn a_text_heavy_directory_is_suggested() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_text(&root.join("archive/one.json"), 200);
        write_text(&root.join("archive/two.csv"), 200);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let found = find(&insights, "compressible").expect("400 MB of text");

        assert_eq!(found.action, Suggested::Compress);
        assert_eq!(found.confidence, Confidence::Possible);
        assert!(
            !found.action.is_destructive(),
            "compression must never be offered as a deletion"
        );
    }

    #[test]
    fn a_directory_of_media_is_not_suggested() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("videos/a.mp4"), 300);
        write(&root.join("videos/b.mkv"), 300);
        write_text(&root.join("videos/notes.txt"), 1);

        let tree = scan(root);
        assert!(find(&analyze(&tree, &ctx()), "compressible").is_none());
    }

    #[test]
    fn the_estimate_never_exceeds_the_bytes_it_describes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_text(&root.join("logs/big.log"), 400);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let Some(found) = find(&insights, "compressible") else {
            return; // log-files may claim it first; that is fine.
        };
        let actual: u64 = found.nodes.iter().map(|&id| tree.node(id).allocated).sum();
        assert!(
            found.reclaimable <= actual,
            "estimated {} from {} of data",
            found.reclaimable,
            actual
        );
    }
}

mod catalogue_shape {
    use super::*;

    /// Every rule the documentation lists must exist and be registered. A
    /// catalogue that promises rules it does not run is a catalogue nobody can
    /// trust to be complete.
    #[test]
    fn every_documented_rule_is_registered() {
        let documented = [
            "build-artifacts",
            "package-manager-cache",
            "browser-cache",
            "app-cache",
            "trash-not-emptied",
            "stale-installers",
            "large-and-cold",
            "log-files",
            "orphaned-vm-images",
            "cluster-waste",
            "sparse-savings",
            "compressible",
            "deep-nesting",
            "single-dominant-child",
        ];

        let registered: Vec<RuleId> = kosha_analyze::registered_rules();
        for name in documented {
            assert!(
                registered.contains(&RuleId::new(name)),
                "`{name}` is in the expected built-in catalogue but not registered"
            );
        }
        assert_eq!(
            registered.len(),
            documented.len(),
            "a rule is registered that the catalogue does not document: {:?}",
            registered
                .iter()
                .filter(|r| !documented.contains(&r.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rule_ids_are_unique() {
        let registered = kosha_analyze::registered_rules();
        let mut ids: Vec<&str> = registered.iter().map(|r| r.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two rules share an id");
    }
}

mod wording {
    use super::*;

    /// A destructive finding's prose must not restate a figure the engine can
    /// change underneath it.
    ///
    /// The engine trims overlapping findings and recomputes `reclaimable`
    /// *after* a rule has written its title and detail. Anything baked into
    /// that prose is therefore a value from before the trim — which is how a
    /// card ended up reading "9.32 GiB in application caches / 105 directories"
    /// beside a corner figure of 9.31 GiB and a note that 77 of them were
    /// counted elsewhere. Counts and byte figures belong in the structured
    /// fields, which the view renders live.
    ///
    /// Review-only findings are exempt: nothing trims them, so their wording
    /// cannot go stale.
    ///
    /// **A figure, not a unit.** This looked for the unit alone and read the
    /// whole of the prose, which includes the folder the finding is about —
    /// so it failed once during a loaded run because the temporary directory
    /// underneath it was called `.tmp70iTBE`, and `TB` is in that. The test
    /// was right about nothing and the message pointed at a rule that had
    /// done nothing wrong. What it means by a baked figure is a *number* with
    /// a unit after it, so that is what it looks for now, in what the rule
    /// wrote rather than in the name of a directory somebody else chose.
    #[test]
    fn destructive_findings_do_not_bake_figures_into_prose() {
        const UNITS: &[&str] = &["KiB", "MiB", "GiB", "TiB", "KB", "MB", "GB", "TB"];

        /// Is there a `12 GiB` or a `12GiB` in here?
        fn baked(text: &str, unit: &str) -> bool {
            text.match_indices(unit).any(|(at, _)| {
                text[..at]
                    .trim_end_matches(' ')
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit())
            })
        }

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("proj")).unwrap();
        fs::write(root.join("proj/package.json"), b"{}").unwrap();
        write(&root.join("proj/node_modules/big.bin"), 8);
        write(&root.join("Downloads/installer.exe"), 150);
        write(&root.join("logs/app.log"), 60);
        write(
            &root.join("Google/Chrome/User Data/Default/Cache/data_1"),
            40,
        );
        write(&root.join("SomeApp/Caches/blob"), 30);

        // The predicate itself, on the two strings that made this test worth
        // rewriting: the card that prompted it, and the directory name that
        // failed it for nothing.
        assert!(baked("9.32 GiB in application caches", "GiB"));
        assert!(baked("8GB of it", "GB"));
        assert!(!baked("C:\\Temp\\.tmp70iTBE\\logs", "TB"));
        assert!(!baked("sizes are shown in MiB", "MiB"));

        let tree = scan(root);
        for insight in analyze(&tree, &ctx()) {
            if !insight.action.is_destructive() {
                continue;
            }
            // The folder this is about is named in the prose and is not the
            // rule's wording, so it is not what is under test.
            let here = root.to_string_lossy();
            let title = insight.title.replace(here.as_ref(), "<root>");
            let detail = insight.detail.replace(here.as_ref(), "<root>");
            for unit in UNITS {
                assert!(
                    !baked(&title, unit),
                    "`{}` puts a byte figure in its title: {title:?}",
                    insight.rule
                );
                assert!(
                    !baked(&detail, unit),
                    "`{}` puts a byte figure in its detail: {detail:?}",
                    insight.rule
                );
            }
        }
    }

    #[test]
    fn every_finding_names_what_its_nodes_are() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("proj")).unwrap();
        fs::write(root.join("proj/package.json"), b"{}").unwrap();
        write(&root.join("proj/node_modules/big.bin"), 8);
        write(&root.join("huge/deep/payload.bin"), 100);

        let tree = scan(root);
        for insight in analyze(&tree, &ctx()) {
            assert!(
                !insight.subject.is_empty(),
                "`{}` has no subject noun, so a view cannot render its count",
                insight.rule
            );
            assert!(
                !insight.subject.ends_with('s'),
                "`{}` has a plural subject {:?}; the view pluralises it",
                insight.rule,
                insight.subject
            );
        }
    }

    #[test]
    fn titles_are_short_enough_for_a_card() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("Downloads/installer.exe"), 150);

        let tree = scan(root);
        for insight in analyze(&tree, &ctx()) {
            assert!(
                insight.title.chars().count() <= 60,
                "`{}` has a {}-character title: {:?}",
                insight.rule,
                insight.title.chars().count(),
                insight.title
            );
            assert!(
                !insight.title.ends_with('.'),
                "`{}` title should not end with a period",
                insight.rule
            );
        }
    }
}

mod units {
    use super::*;
    use kosha_core::format::Units;

    fn ctx_with(units: Units) -> Context {
        Context {
            min_reclaimable: 1,
            units,
            ..Context::at(kosha_core::format::now_unix() + 10 * 365 * 86_400)
        }
    }

    /// A finding that spells a byte figure in its own prose must spell it the
    /// way the rest of the interface does. Otherwise one card reads "39.4 MB"
    /// in its corner and "6.24 MiB" in its text, and the user is left to work
    /// out whether those are the same number.
    #[test]
    fn rule_prose_follows_the_configured_units() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("huge/deeper/payload.bin"), 200);
        write(&root.join("tiny.bin"), 1);

        let binary = analyze(&scan(root), &ctx_with(Units::Binary));
        let decimal = analyze(&scan(root), &ctx_with(Units::Decimal));

        let prose = |insights: &[Insight]| -> String {
            insights
                .iter()
                .map(|i| format!("{} {}", i.title, i.detail))
                .collect()
        };

        let binary_text = prose(&binary);
        let decimal_text = prose(&decimal);

        assert!(
            binary_text.contains("iB"),
            "binary units should produce iB somewhere: {binary_text}"
        );
        assert!(
            !decimal_text.contains("iB"),
            "decimal units leaked a binary figure: {decimal_text}"
        );
    }
}

/// A real disk image offers nothing to act on, end to end.
///
/// The half that no unit test can reach: that `analyze` asks the tree where
/// it came from, and that a scan with `use_image` says `DiskImage`. The pass
/// itself is pinned by `an_image_turns_every_offer_into_something_to_read` in
/// the engine, which runs everywhere.
///
/// It needs an image with something worth reporting inside it. Build one —
///
/// ```text
/// mkdir -p t/proj/node_modules t/logs && echo '{}' > t/proj/package.json
/// dd if=/dev/zero of=t/proj/node_modules/big.bin bs=1M count=80
/// dd if=/dev/zero of=t/logs/app.log bs=1M count=55
/// xorrisofs -rational-rock -o cache.iso t
/// ```
///
/// — then `KOSHA_IMAGE_PROBE=/path/to/cache.iso cargo test -p kosha-analyze
/// -- --ignored an_image_offers_nothing`.
///
/// **It stands down rather than failing when that is unset**, the way the
/// share probe in `kosha-fsops` does and for the same reason: the sandbox
/// runs these files with `--include-ignored` on a machine that has no image
/// to hand, and a test that panicked there would fail a scenario that is
/// otherwise green.
#[test]
#[ignore = "needs a disk image; set KOSHA_IMAGE_PROBE"]
fn an_image_offers_nothing_to_act_on() {
    let Ok(image) = std::env::var("KOSHA_IMAGE_PROBE") else {
        println!("no KOSHA_IMAGE_PROBE: no image to point at, so nothing was checked");
        return;
    };
    let tree = Scanner::new(ScanOptions {
        use_image: true,
        ..ScanOptions::default()
    })
    .scan(Path::new(&image))
    .expect("the image reads");
    assert_eq!(
        tree.stats().strategy,
        kosha_core::Strategy::DiskImage,
        "that file was not read as an image, so this checked nothing"
    );

    let insights = analyze(&tree, &ctx());
    assert!(
        !insights.is_empty(),
        "the image holds nothing worth reporting, so this checked nothing"
    );
    for insight in &insights {
        assert!(
            !insight.action.is_actionable(),
            "`{}` offers {} inside an image",
            insight.rule,
            insight.action.label()
        );
    }
    assert_eq!(
        kosha_analyze::total_reclaimable(&insights),
        0,
        "bytes nobody can get back were counted as reclaimable"
    );
}
