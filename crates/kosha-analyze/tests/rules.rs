// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Rule behaviour against synthetic trees.
//!
//! Each test builds a fixture directory, scans it, and asserts the exact
//! finding. Rules are pure, so "now" comes from the `Context` — which is how
//! these tests can assert on file ages without waiting or touching timestamps.

use std::fs;
use std::path::Path;

use kosha_analyze::{Confidence, Context, Insight, RuleId, Suggested, analyze};
use kosha_core::{ScanOptions, Scanner, SizeTree};

const MB: usize = 1024 * 1024;

/// Opt-in CPU comparison over a recorded tree, with no rescan or private paths
/// in successful output. This is a lookup microbenchmark, not scanner throughput.
#[test]
#[ignore = "set KOSHA_HOME_BENCH_SNAPSHOT to an existing .kosha metadata record"]
fn benchmark_indexed_home_paths_against_the_previous_arena_walk() {
    use kosha_analyze::{
        Rule,
        pack::{Matcher, PackRule},
    };
    use kosha_core::{Flags, NodeId};
    use std::hint::black_box;
    use std::time::Instant;
    let source = std::env::var_os("KOSHA_HOME_BENCH_SNAPSHOT").expect("snapshot path is required");
    let (_, tree) = kosha_core::snapshot::read(std::fs::File::open(source).unwrap()).unwrap();
    let mut context = Context::detect();
    context.min_reclaimable = 0;
    if let Some(home) = std::env::var_os("KOSHA_HOME_BENCH_HOME") {
        context.home = Some(home.into());
    }
    let home = context
        .home
        .as_ref()
        .expect("recorded home must be specified if not detectable");
    let paths = [
        ".claude/projects",
        ".codex/sessions",
        ".cache/uv",
        ".npm/_cacache",
        "AppData/Local/uv/cache",
        ".cargo/registry/cache",
        "Library/Caches",
        "missing-benchmark-path",
    ];
    let rules: Vec<_> = (0..64)
        .map(|index| PackRule {
            id: format!("benchmark-{index}"),
            title: "Review".into(),
            detail: "Recorded metadata".into(),
            subject: "directory".into(),
            matcher: Matcher::Location(kosha_analyze::location::Location {
                base: kosha_analyze::location::LocationBase::Home,
                path: paths[index % paths.len()].into(),
            }),
            action: Suggested::ReviewOnly,
        })
        .collect();
    let started = Instant::now();
    let indexed: Vec<_> = rules
        .iter()
        .map(|rule| {
            rule.evaluate(black_box(&tree), black_box(&context))
                .first()
                .and_then(|found| found.nodes.first().copied())
        })
        .collect();
    let indexed_us = started.elapsed().as_micros();
    let started = Instant::now();
    let mut visits = 0u64;
    let reference: Vec<_> = rules
        .iter()
        .map(|rule| {
            let Matcher::Location(location) = &rule.matcher else {
                unreachable!()
            };
            let target = home.join(&location.path);
            let leaf = target.file_name().unwrap();
            (0..tree.len()).map(NodeId::from_index).find(|&id| {
                visits += 1;
                let node = tree.node(id);
                node.is_dir()
                    && tree.name(id) == leaf
                    && !node.flags.contains(Flags::SYMLINK)
                    && !node.flags.contains(Flags::HARDLINK_DUP)
                    && !node.flags.contains(Flags::OTHER_DEVICE)
                    && !node.flags.contains(Flags::UNREADABLE)
                    && tree.path_of(id) == target
            })
        })
        .collect();
    let reference_us = started.elapsed().as_micros();
    assert_eq!(indexed, reference, "indexed matching changed the result");
    println!(
        "{}",
        serde_json::json!({"entries":tree.len(),"rules":rules.len(),"matched_rules":indexed.iter().filter(|id|id.is_some()).count(),
        "reference_node_visits":visits,"indexed_us":indexed_us,"reference_us":reference_us})
    );
}

/// Ten years past the fixture's mtimes, so everything in it reads as old, and
/// a threshold low enough that a test fixture can trip the rules at all.
fn ctx() -> Context {
    let now = kosha_core::format::now_unix() + 10 * 365 * 86_400;
    Context {
        min_reclaimable: 1,
        ..Context::at(now)
    }
}

/// A context anchored to now, so nothing reads as stale.
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

// ------------------------------------------------- rules nothing had tested

/// The last rule in the catalogue with no test.
///
/// It had none because its size threshold was a private 1 GiB constant, so a
/// fixture that tripped it had to really occupy a gigabyte — and a sparse file
/// will not do, since occupancy is the quantity under test. The threshold is a
/// `Context` field now, beside the day thresholds that were already there for
/// exactly this reason.
#[test]
fn a_large_old_file_is_reported_and_a_recent_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("archive.bin"), 8);
    write(&root.join("small.bin"), 1);

    let cold = Context {
        cold_file_bytes: 4 * MB as u64,
        ..ctx()
    };
    let insights = analyze(&scan(root), &cold);
    let found = find(&insights, "large-and-cold").expect("an 8 MB file aged ten years");

    assert_eq!(
        found.action,
        Suggested::ReviewOnly,
        "large and old describes a finished video project as well as junk"
    );
    assert_eq!(
        found.nodes.len(),
        1,
        "the 1 MB file is under the threshold and not a finding"
    );

    // Anchored to now, the same file is not old, and nothing is reported.
    let fresh = Context {
        cold_file_bytes: 4 * MB as u64,
        ..ctx_fresh()
    };
    assert!(
        find(&analyze(&scan(root), &fresh), "large-and-cold").is_none(),
        "a file modified moments ago was called cold"
    );
}

/// The threshold has to be the thing that decides it, or it is a setting in
/// name only — which is what the private constant amounted to.
#[test]
fn raising_the_cold_threshold_past_a_file_drops_it() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("archive.bin"), 8);
    let tree = scan(dir.path());

    let under = Context {
        cold_file_bytes: 4 * MB as u64,
        ..ctx()
    };
    assert!(find(&analyze(&tree, &under), "large-and-cold").is_some());

    let over = Context {
        cold_file_bytes: 64 * MB as u64,
        ..ctx()
    };
    assert!(find(&analyze(&tree, &over), "large-and-cold").is_none());
}

/// Twenty-five levels, with short names so the whole path stays under the 260
/// characters Windows stops at without being asked not to — the fixture must
/// not be the thing that fails.
#[test]
fn a_tree_that_recursed_into_itself_is_signposted() {
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    for _ in 0..25 {
        deep.push("d");
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("f.bin"), b"x").unwrap();

    let insights = analyze(&scan(dir.path()), &ctx());
    let found = find(&insights, "deep-nesting").expect("25 levels of nesting");
    assert_eq!(
        found.action,
        Suggested::ReviewOnly,
        "nesting is not deleted"
    );
    assert!(found.title.contains("25"), "{}", found.title);
    assert!(
        !found.title.contains("at least"),
        "a tree the walk saw all of is not a floor: {}",
        found.title
    );

    // And a shallow tree says nothing, or the panel would carry a permanent row.
    let shallow = tempfile::tempdir().unwrap();
    fs::create_dir_all(shallow.path().join("a/b/c")).unwrap();
    fs::write(shallow.path().join("a/b/c/f.bin"), b"x").unwrap();
    assert!(find(&analyze(&scan(shallow.path()), &ctx()), "deep-nesting").is_none());
}

/// And where the walk stopped, the depth it reports is a floor.
///
/// Found on this machine: `%LOCALAPPDATA%` holds a tree over 300 levels deep, the
/// walk stops at its 255-level ceiling, and the insight said "nested 255 levels
/// deep" as a flat fact. It is the ceiling, not a measurement — the same mistake
/// as calling a truncated scan's total the size of the folder.
#[test]
fn a_depth_the_walk_stopped_at_is_reported_as_a_floor() {
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    for _ in 0..30 {
        deep.push("d");
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("f.bin"), b"x").unwrap();

    // The ceiling is `u8::MAX` and reaching it needs a tree 255 deep, which is
    // expensive to build and — on Windows, past `MAX_PATH` — slow to delete: one
    // such tree is sitting in this machine's `%TEMP%` because something could not.
    // So what is asserted here is the other side of the condition, which is the
    // one a fixture can reach: a tree the walk saw all of says nothing about being
    // a floor, in the title or in the prose.
    let tree = scan(dir.path());
    assert!(!tree.stats().depth_limited);
    let insights = analyze(&tree, &ctx());
    let found = find(&insights, "deep-nesting").expect("30 levels");
    assert!(!found.title.contains("at least"), "{}", found.title);
    assert!(
        !found.detail.contains("where the walk stopped"),
        "{}",
        found.detail
    );
}

/// Cluster slack: a thousand one-byte files occupy a thousand allocation
/// units. True on NTFS and on ext4 alike, which is why this is not gated.
#[test]
fn many_tiny_files_are_reported_as_cluster_slack() {
    let dir = tempfile::tempdir().unwrap();
    let crumbs = dir.path().join("crumbs");
    fs::create_dir_all(&crumbs).unwrap();
    for i in 0..400 {
        fs::write(crumbs.join(format!("{i}.txt")), b"x").unwrap();
    }

    let tree = scan(dir.path());
    let insights = analyze(&tree, &ctx());
    let Some(found) = find(&insights, "cluster-waste") else {
        // A filesystem that packs tails, or one whose block size is 1, has no
        // slack to report and is not wrong to say so.
        assert_eq!(
            tree.total_allocated(),
            tree.total_logical(),
            "there was slack, and the rule missed it"
        );
        return;
    };

    assert_eq!(
        found.action,
        Suggested::ReviewOnly,
        "slack cannot be deleted"
    );
    assert!(
        found.reclaimable >= 400,
        "400 files of one byte wasted only {} bytes",
        found.reclaimable
    );
}

/// Sparse files, on both platforms, which set the flag by completely different
/// means: Unix from `st_blocks` falling short of the length, Windows from the
/// attribute bit.
///
/// This is also the test for asking the filesystem about sparse files even when
/// `--exact` was not requested. Without that, Windows reports a sparse file's
/// allocation as its full length, the saving computes to zero, and the insight
/// can never appear — while Linux reports it for free.
#[test]
fn sparse_files_are_credited_with_what_they_save() {
    const SPAN: u64 = 64 * 1024 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.img");
    fs::write(&path, b"").unwrap();

    // NTFS allocates on `set_len` unless the file is marked sparse first.
    // `fsutil` needs no elevation for a file you own.
    #[cfg(windows)]
    {
        let marked = std::process::Command::new("fsutil")
            .args(["sparse", "setflag"])
            .arg(&path)
            .stdout(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !marked {
            return; // No `fsutil`; nothing to assert.
        }
    }

    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(SPAN).unwrap();
    drop(file);

    let tree = scan(dir.path());
    let root = tree.root();
    let node = (0..tree.len())
        .map(kosha_core::NodeId::from_index)
        .find(|&id| tree.name_lossy(id) == "disk.img")
        .expect("the fixture file is in the tree");
    assert_ne!(node, root);

    if !tree.node(node).flags.contains(kosha_core::Flags::SPARSE) {
        // Some filesystems really do allocate the whole span. tmpfs and exFAT
        // both will, and neither is wrong.
        return;
    }

    assert!(
        tree.node(node).allocated < SPAN,
        "a sparse file was reported as occupying its full length, which is the \
         bug that makes the saving compute to zero"
    );

    let insights = analyze(&tree, &ctx());
    let found = find(&insights, "sparse-savings").expect("a 64 MiB sparse file holding nothing");
    assert_eq!(found.action, Suggested::ReviewOnly);
    assert!(
        found.title.contains("saved"),
        "the insight should say what was saved, not what can be freed: {}",
        found.title
    );
}

/// The package-manager cache rule was the only one in the catalogue with no
/// test at all, because it is the only one that needs `ctx.home` and nothing
/// set it. Eleven patterns, none of them exercised.
///
/// It matters most for the one written in Windows' own spelling. The patterns
/// are `&'static str` with forward slashes — `"AppData/Local/Temp"` — joined
/// onto the home directory and compared against a scanned path, which on
/// Windows really is `AppData\Local\Temp`. That comparison happens to work,
/// because `Path` compares by components and the Windows parser accepts both
/// separators, but "happens to work" is the state a test exists to end.
#[test]
fn a_cache_pattern_written_with_slashes_matches_the_real_directory() {
    let dir = tempfile::tempdir().unwrap();
    // The scanner resolves aliases such as macOS /var and Windows short names.
    // Context paths must use that same spelling, as Context::detect does.
    let home = kosha_core::path::canonical(dir.path());

    // One pattern from each platform's family, all rooted at the same fake
    // home so a single scan covers them.
    write(&home.join("AppData/Local/Temp/leftover.bin"), 4);
    write(&home.join(".cache/thing/blob.bin"), 4);
    write(&home.join(".cargo/registry/cache/crate.crate"), 4);
    // Not a cache: a directory that merely lives near them.
    write(&home.join("AppData/Local/Programs/app.exe"), 4);

    let tree = scan(&home);
    let found: Vec<Insight> = analyze(
        &tree,
        &Context {
            home: Some(home),
            ..ctx()
        },
    )
    .into_iter()
    .filter(|insight| insight.rule == RuleId::new("package-manager-cache"))
    .collect();

    let titles: Vec<&str> = found.iter().map(|i| i.title.as_str()).collect();
    assert!(
        titles.contains(&"AppData/Local/Temp"),
        "the Windows temp pattern did not match: {titles:?}"
    );
    assert!(
        titles.contains(&".cache"),
        "the XDG cache pattern did not match: {titles:?}"
    );
    assert!(
        titles.contains(&".cargo/registry/cache"),
        "a nested pattern did not match: {titles:?}"
    );
    assert!(
        !titles.iter().any(|t| t.contains("Programs")),
        "matched a directory that is not a cache: {titles:?}"
    );

    for insight in &found {
        assert_eq!(
            insight.action,
            Suggested::EmptyContents,
            "a cache is emptied, never deleted: {}",
            insight.title
        );
        assert!(insight.reclaimable >= 4 * MB as u64, "{}", insight.title);
    }
}

/// Without a home there is nothing to root the patterns at, and guessing would
/// mean matching any `.cache` anywhere.
#[test]
fn cache_patterns_do_not_fire_without_a_home() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join(".cache/thing/blob.bin"), 4);

    let insights = analyze(&scan(dir.path()), &ctx());
    assert!(find(&insights, "package-manager-cache").is_none());
}

#[test]
fn build_output_is_found_by_its_marker_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("project/package.json"), b"{}").ok();
    fs::create_dir_all(root.join("project")).unwrap();
    fs::write(root.join("project/package.json"), b"{}").unwrap();
    write(&root.join("project/node_modules/big.bin"), 4);

    let tree = scan(root);
    let insights = analyze(&tree, &ctx());
    let found = find(&insights, "build-artifacts").expect("node_modules beside a package.json");

    assert_eq!(found.confidence, Confidence::Certain);
    assert_eq!(found.action, Suggested::Trash);
    assert!(found.reclaimable >= 4 * MB as u64);
    assert_eq!(found.nodes.len(), 1);
    assert_eq!(tree.name_lossy(found.nodes[0]), "node_modules");
}

#[test]
fn a_directory_named_like_build_output_is_not_enough() {
    // This is the whole reason marker files exist. Reporting somebody's source
    // directory because it happens to be called `build` would train users to
    // ignore the panel.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("mysite/node_modules/big.bin"), 4);
    write(&root.join("other/build/big.bin"), 4);

    let tree = scan(root);
    let insights = analyze(&tree, &ctx());

    assert!(
        find(&insights, "build-artifacts").is_none(),
        "no package.json and no CMakeLists.txt, so neither is evidence of anything"
    );
}

#[test]
fn nested_build_output_is_counted_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(root.join("app/package.json"), b"{}").unwrap();
    // A transitive dependency with its own manifest and its own node_modules.
    fs::create_dir_all(root.join("app/node_modules/dep")).unwrap();
    fs::write(root.join("app/node_modules/dep/package.json"), b"{}").unwrap();
    write(&root.join("app/node_modules/dep/node_modules/inner.bin"), 4);

    let tree = scan(root);
    let insights = analyze(&tree, &ctx());
    let found = find(&insights, "build-artifacts").unwrap();

    assert_eq!(
        found.nodes.len(),
        1,
        "the inner node_modules is already inside the outer one's bytes"
    );
    assert_eq!(tree.name_lossy(found.nodes[0]), "node_modules");
    assert_eq!(
        found.reclaimable,
        tree.node(found.nodes[0]).allocated,
        "reclaimable must not double-count the nested copy"
    );
}

#[test]
fn old_installers_in_downloads_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("Downloads/some-app-1.2.3.exe"), 150);

    let tree = scan(root);
    let found = find(&analyze(&tree, &ctx()), "stale-installers")
        .expect("a 150 MB .exe in Downloads, aged ten years")
        .clone();

    assert_eq!(found.confidence, Confidence::Likely);
    assert_eq!(found.action, Suggested::Trash);
    assert_eq!(found.nodes.len(), 1);
}

#[test]
fn recent_installers_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("Downloads/some-app-1.2.3.exe"), 150);

    let tree = scan(root);
    assert!(
        find(&analyze(&tree, &ctx_fresh()), "stale-installers").is_none(),
        "a file downloaded moments ago is not stale"
    );
}

#[test]
fn installers_outside_downloads_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A 150 MB .exe here is a build product, not litter.
    write(&root.join("workspace/release/installer.exe"), 150);

    let tree = scan(root);
    assert!(find(&analyze(&tree, &ctx()), "stale-installers").is_none());
}

#[test]
fn logs_are_aggregated_by_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("service/logs/app.log"), 30);
    write(&root.join("service/logs/app.log.1"), 30);
    write(&root.join("service/logs/app.log.2.gz"), 30);
    write(&root.join("service/logs/notes.txt"), 30);

    let tree = scan(root);
    let found = find(&analyze(&tree, &ctx()), "log-files")
        .expect("90 MB of logs in one directory")
        .clone();

    assert_eq!(found.nodes.len(), 3, "notes.txt is not a log");
    assert!(found.reclaimable >= 90 * MB as u64);
    assert!(found.reclaimable < 120 * MB as u64, "notes.txt was counted");
}

#[test]
fn a_dominant_directory_is_signposted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("huge/deeper/payload.bin"), 100);
    write(&root.join("tiny.bin"), 1);

    let tree = scan(root);
    let insights = analyze(&tree, &ctx());
    let found = find(&insights, "single-dominant-child").unwrap();

    assert_eq!(found.action, Suggested::ReviewOnly);
    assert_eq!(found.reclaimable, 0, "a signpost is not a saving");
    // Followed `huge` and then `deeper`, since each dominates its parent.
    assert_eq!(tree.name_lossy(*found.nodes.last().unwrap()), "deeper");
}

#[test]
fn an_evenly_spread_tree_gets_no_signpost() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("a/x.bin"), 10);
    write(&root.join("b/x.bin"), 10);
    write(&root.join("c/x.bin"), 10);

    let tree = scan(root);
    assert!(find(&analyze(&tree, &ctx()), "single-dominant-child").is_none());
}

#[test]
fn analysis_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("p")).unwrap();
    fs::write(root.join("p/package.json"), b"{}").unwrap();
    write(&root.join("p/node_modules/a.bin"), 4);
    write(&root.join("Downloads/installer.exe"), 150);
    write(&root.join("logs/app.log"), 60);

    let tree = scan(root);
    let context = ctx();
    let first = analyze(&tree, &context);
    let second = analyze(&tree, &context);

    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(a.rule, b.rule);
        assert_eq!(a.title, b.title);
        assert_eq!(a.reclaimable, b.reclaimable);
        assert_eq!(a.nodes, b.nodes);
    }
}

#[test]
fn insights_are_ranked_by_confident_value() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A certain 4 MB should outrank a merely possible 200 MB.
    fs::create_dir_all(root.join("p")).unwrap();
    fs::write(root.join("p/package.json"), b"{}").unwrap();
    write(&root.join("p/node_modules/a.bin"), 40);
    write(&root.join("media/movie.mkv"), 200);

    let tree = scan(root);
    let insights = analyze(&tree, &ctx());
    let scores: Vec<f64> = insights.iter().map(Insight::score).collect();

    assert!(
        scores.windows(2).all(|w| w[0] >= w[1]),
        "not ranked: {scores:?}"
    );
}

#[test]
fn an_empty_tree_yields_no_insights() {
    let dir = tempfile::tempdir().unwrap();
    let tree = scan(dir.path());
    assert!(analyze(&tree, &ctx()).is_empty());
}

mod duplicates {
    use super::*;
    use kosha_analyze::duplicates::{self, DuplicateOptions};

    fn opts() -> DuplicateOptions {
        DuplicateOptions {
            min_size: 1,
            ..Default::default()
        }
    }

    /// Empty files are identical to each other and worth nothing.
    ///
    /// The default 1 MiB floor hides this, which is not the same as handling
    /// it — and now that `--dupe-min` lets the floor be lowered, "2 × 0 B —
    /// 0 B recoverable" is a set the user can actually be shown, offering a
    /// deletion that frees no bytes.
    #[test]
    fn a_set_that_would_free_nothing_is_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("empty-one"), b"").unwrap();
        fs::write(root.join("empty-two"), b"").unwrap();
        fs::write(root.join("empty-three"), b"").unwrap();
        // Something real, so an empty report cannot pass by accident.
        let payload = vec![3u8; 2 * MB];
        fs::write(root.join("a.bin"), &payload).unwrap();
        fs::write(root.join("b.bin"), &payload).unwrap();

        let tree = scan(root);
        let report = duplicates::find(
            &tree,
            &DuplicateOptions {
                min_size: 0,
                ..Default::default()
            },
        );

        assert_eq!(report.groups.len(), 1, "{:?}", report.groups);
        assert!(report.groups[0].reclaimable > 0);
        assert!(
            report.groups.iter().all(|group| group.size > 0),
            "an empty-file set was reported: {:?}",
            report.groups
        );
    }

    /// A hunt reads file contents, so it can take minutes — and a job that
    /// long has to be able to say how far along it is.
    #[test]
    fn a_hunt_reports_its_way_to_the_total() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Four pairs, so there is more than one group to report between.
        for pair in 0..4 {
            let payload = vec![pair as u8; 2 * MB];
            fs::write(root.join(format!("a{pair}.bin")), &payload).unwrap();
            fs::write(root.join(format!("b{pair}.bin")), &payload).unwrap();
        }

        let (tx, rx) = crossbeam_channel::unbounded();
        let report = duplicates::Hunt::new(opts())
            .with_progress(tx)
            .run(&scan(root));
        let updates: Vec<duplicates::Progress> = rx.try_iter().collect();

        assert_eq!(report.groups.len(), 4);
        assert!(!report.stopped, "nothing asked it to stop");

        let first = updates.first().expect("a hunt publishes before it reads");
        assert_eq!(first.files_done, 0, "the first update is the starting line");
        assert_eq!(first.files_total, 8, "the total is known before any I/O");

        let last = updates.last().expect("and again as it goes");
        assert_eq!(last.files_done, last.files_total, "it reaches its total");
        assert_eq!(last.fraction(), Some(1.0));
        assert!(last.bytes_read > 0, "it read something to decide");

        // Monotonic: a bar that goes backwards is worse than no bar.
        for pair in updates.windows(2) {
            assert!(
                pair[1].files_done >= pair[0].files_done,
                "{:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    /// Nothing to do is not 0%, and a bar drawn from it would sit at empty
    /// looking stuck.
    #[test]
    fn a_hunt_with_no_candidates_has_no_fraction() {
        let nothing = duplicates::Progress::default();
        assert_eq!(nothing.fraction(), None);
    }

    /// Stopping has to mean the reads still ahead do not happen — and the
    /// report has to say it was stopped, because after that a *missing* group
    /// means nothing.
    #[test]
    fn a_stopped_hunt_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for pair in 0..4 {
            let payload = vec![pair as u8; 2 * MB];
            fs::write(root.join(format!("a{pair}.bin")), &payload).unwrap();
            fs::write(root.join(format!("b{pair}.bin")), &payload).unwrap();
        }
        let tree = scan(root);

        // Already set, so every group sees it and none of them reads anything.
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let report = duplicates::Hunt::new(opts())
            .with_cancel(std::sync::Arc::clone(&flag))
            .run(&tree);

        assert!(report.stopped, "the report does not admit it was stopped");
        assert!(report.groups.is_empty(), "it resolved a group anyway");
        assert_eq!(report.bytes_hashed, 0, "it read files after being stopped");
        // The counting passes still ran, so the report can still say what it
        // *would* have looked at. That is the number the window shows.
        assert_eq!(report.candidates_examined, 8);

        // And the same tree without the flag finds everything, so the test
        // above is measuring the stop rather than an empty fixture.
        let complete = duplicates::find(&tree, &opts());
        assert_eq!(complete.groups.len(), 4);
        assert!(!complete.stopped);
    }

    /// The floor is the commonest reason a hunt finds nothing, so it is
    /// counted and reported rather than left for the user to deduce.
    #[test]
    fn files_under_the_floor_are_counted_and_the_floor_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("small-one.txt"), vec![1u8; 200]).unwrap();
        fs::write(root.join("small-two.txt"), vec![1u8; 200]).unwrap();

        let report = duplicates::find(&scan(root), &DuplicateOptions::default());

        assert!(report.groups.is_empty(), "the floor did not apply");
        assert_eq!(report.below_min, 2, "the skipped files were not counted");
        assert_eq!(report.min_size, DuplicateOptions::default().min_size);

        // And lowering it finds them, so the count is pointing at something
        // the user can actually act on.
        let found = duplicates::find(&scan(root), &opts());
        assert_eq!(found.groups.len(), 1);
        assert_eq!(found.below_min, 0);
    }

    #[test]
    fn identical_files_are_grouped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let payload = vec![7u8; 3 * MB];
        fs::write(root.join("one.bin"), &payload).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/two.bin"), &payload).unwrap();
        fs::write(root.join("different.bin"), vec![9u8; 3 * MB]).unwrap();

        let tree = scan(root);
        let report = duplicates::find(&tree, &opts());

        assert_eq!(report.groups.len(), 1, "exactly one pair is identical");
        assert_eq!(report.groups[0].nodes.len(), 2);
        assert_eq!(report.groups[0].size, 3 * MB as u64);
        assert!(report.total_reclaimable() > 0);
    }

    #[test]
    fn same_size_different_content_is_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Identical length, differing only in the last byte, so the edge hash
        // has to actually look at the tail.
        let mut a = vec![0u8; 2 * MB];
        let mut b = vec![0u8; 2 * MB];
        a[2 * MB - 1] = 1;
        b[2 * MB - 1] = 2;
        fs::write(root.join("a.bin"), &a).unwrap();
        fs::write(root.join("b.bin"), &b).unwrap();

        let tree = scan(root);
        let report = duplicates::find(&tree, &opts());

        assert!(report.groups.is_empty(), "{:?}", report.groups);
        assert_eq!(report.candidates_examined, 2, "both were still examined");
    }

    #[test]
    fn files_below_the_threshold_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.bin"), b"tiny").unwrap();
        fs::write(root.join("b.bin"), b"tiny").unwrap();

        let tree = scan(root);
        let report = duplicates::find(&tree, &DuplicateOptions::default());

        assert!(report.groups.is_empty());
        assert_eq!(report.candidates_examined, 0, "no I/O at all");
    }

    #[test]
    fn three_copies_reclaim_two() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let payload = vec![3u8; 2 * MB];
        for name in ["a.bin", "b.bin", "c.bin"] {
            fs::write(root.join(name), &payload).unwrap();
        }

        let tree = scan(root);
        let report = duplicates::find(&tree, &opts());

        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].nodes.len(), 3);
        let one = tree.node(report.groups[0].nodes[0]).allocated;
        assert_eq!(
            report.groups[0].reclaimable,
            2 * one,
            "keep one copy, recover the other two"
        );
    }

    /// A cloud placeholder must never be opened.
    ///
    /// Reading one downloads it. Hashing a folder of them would fill the disk
    /// this tool was asked to help empty, over the user's connection, because
    /// they clicked "find duplicates" — the worst thing this feature could do.
    ///
    /// `attrib +O` sets the same `FILE_ATTRIBUTE_OFFLINE` bit a OneDrive
    /// placeholder carries, so the refusal can be exercised without OneDrive.
    #[cfg(windows)]
    #[test]
    fn a_file_stored_in_the_cloud_is_never_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let payload = vec![5u8; 2 * MB];
        // Three identical files. Two are local and are a genuine duplicate
        // pair; the third would join them if its bytes were here.
        for name in ["here-a.bin", "here-b.bin", "in-the-cloud.bin"] {
            fs::write(root.join(name), &payload).unwrap();
        }

        let marked = std::process::Command::new("attrib")
            .arg("+O")
            .arg(root.join("in-the-cloud.bin"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !marked {
            return; // `attrib` unavailable; nothing to assert.
        }

        let tree = scan(root);
        let report = duplicates::find(&tree, &opts());

        assert_eq!(report.not_local, 1, "the cloud file was not skipped");

        // The two local copies are still found: declining one candidate must
        // not disable the feature for the rest of the folder.
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].nodes.len(), 2);

        let named: Vec<String> = report.groups[0]
            .nodes
            .iter()
            .map(|&id| tree.name_lossy(id).to_string())
            .collect();
        assert!(
            !named.iter().any(|n| n == "in-the-cloud.bin"),
            "a file whose data is not local was reported as a duplicate: {named:?}"
        );

        // The assertion that would catch a download. Two local files at 2 MiB
        // account for 4 MiB of full hashing plus 32 KiB of head-and-tail edge
        // reads each; a third file joining them would push this past 6 MiB.
        // The bound is "fewer bytes than three copies need", which is the
        // claim — not an exact figure, which would only pin the edge window.
        assert!(
            report.bytes_hashed < 3 * 2 * MB as u64,
            "enough bytes were read to have fetched the cloud copy: {}",
            report.bytes_hashed
        );
    }

    /// The same tree with the named files marked as cloud placeholders, and the
    /// files themselves *deleted from disk*.
    ///
    /// This is the platform-neutral way to be a placeholder: the scanner sets
    /// `OFFLINE` from a Windows attribute or a macOS `SF_DATALESS` bit, and both
    /// arrive in the tree as the same flag. Deleting the file afterwards is what
    /// makes a read observable without a cloud account: if the hunt opened a
    /// placeholder it would fail and count one as `unreadable`.
    fn with_placeholders(tree: &SizeTree, root: &Path, names: &[&str]) -> SizeTree {
        let mut nodes = tree.nodes_slice().to_vec();
        for (index, node) in nodes.iter_mut().enumerate() {
            let id = kosha_core::NodeId::from_index(index);
            let name = tree.name_lossy(id);
            if names.iter().any(|wanted| *wanted == name) {
                node.flags.insert(kosha_core::Flags::OFFLINE);
            }
        }
        for name in names {
            fs::remove_file(root.join(name)).unwrap();
        }
        SizeTree::from_parts(
            nodes,
            kosha_core::NameArena::from_bytes(tree.names_bytes().to_vec()),
            kosha_core::Owners::default(),
            tree.root_path().to_path_buf(),
            tree.stats().clone(),
        )
    }

    /// A placeholder is skipped and counted, and never opened, on any platform.
    #[test]
    fn a_flagged_placeholder_is_skipped_counted_and_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let payload = vec![5u8; 2 * MB];
        for name in ["here-a.bin", "here-b.bin", "in-the-cloud.bin"] {
            fs::write(root.join(name), &payload).unwrap();
        }
        let tree = with_placeholders(&scan(root), root, &["in-the-cloud.bin"]);

        let report = duplicates::find(&tree, &opts());

        assert_eq!(report.not_local, 1);
        assert_eq!(report.unreadable, 0, "a placeholder was opened");
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].nodes.len(), 2, "the local pair survives");
    }

    /// Same-length placeholders are not even candidates, so nothing is read.
    #[test]
    fn same_length_placeholders_are_not_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["a.bin", "b.bin"] {
            fs::write(root.join(name), vec![9u8; 2 * MB]).unwrap();
        }
        let tree = with_placeholders(&scan(root), root, &["a.bin", "b.bin"]);

        let report = duplicates::find(&tree, &opts());

        assert_eq!(report.not_local, 2);
        assert_eq!(report.candidates_examined, 0);
        assert_eq!(report.bytes_hashed, 0);
        assert_eq!(report.unreadable, 0);
        assert!(report.groups.is_empty());
    }
}

/// The hunt opens files, and not everything with a size can be read.
///
/// **Measured on Linux: `kosha <dir> --dupe-min 0` ran forever.** Two FIFOs in
/// a directory are two files of the same size — zero — which is one same-size
/// group, and the hunt opened the first of them. `open` on a FIFO waits for a
/// writer, so there was no output, no progress and no error, only a process to
/// kill. The 1 MiB floor is all that hides it from a default run, and the floor
/// is a setting.
///
/// Under a clock, because what is being tested for is *no answer at all*: a
/// test that simply called it would hang the suite rather than fail it.
#[test]
#[cfg(unix)]
fn a_pipe_in_the_way_does_not_stop_the_duplicate_hunt() {
    use kosha_analyze::duplicates::{self, DuplicateOptions};

    let dir = tempfile::tempdir().unwrap();
    for name in ["pipe-a", "pipe-b"] {
        let made = std::process::Command::new("mkfifo")
            .arg(dir.path().join(name))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo said {made}");
    }
    let twin = vec![b'k'; 4096];
    fs::write(dir.path().join("one.bin"), &twin).unwrap();
    fs::write(dir.path().join("two.bin"), &twin).unwrap();

    let tree = scan(dir.path());
    let (tell, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let report = duplicates::find(
            &tree,
            &DuplicateOptions {
                // Zero, which is what somebody chasing small duplicates types,
                // and what puts two empty FIFOs in a group together.
                min_size: 0,
                ..Default::default()
            },
        );
        let _ = tell.send((report.groups.len(), report.unreadable));
    });

    let (groups, unreadable) = heard
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the duplicate hunt never came back from a FIFO");

    assert_eq!(groups, 1, "the two identical files are one group");
    assert_eq!(unreadable, 2, "the two FIFOs are excluded, and counted");
}

/// Regression tests for the three-pass duplicate detector's read coverage.
///
/// These are separated from the happy-path tests above because they probe the
/// exact boundaries where the edge hash stops covering the whole file. Getting
/// these wrong means reporting two different files as identical, in a tool that
/// then offers to delete one of them.
mod duplicate_coverage {
    use super::*;
    use kosha_analyze::duplicates::{self, DuplicateOptions};

    const EDGE: usize = 16 * 1024;

    fn opts() -> DuplicateOptions {
        DuplicateOptions {
            min_size: 1,
            ..Default::default()
        }
    }

    /// Two files of exactly 2x the edge-hash window, identical in their first
    /// window and different in their second.
    #[test]
    fn differing_only_in_the_unread_span_is_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut a = vec![1u8; 2 * EDGE];
        let mut b = vec![1u8; 2 * EDGE];
        // Identical heads; the difference sits past the first window.
        a[EDGE + 100] = 0xAA;
        b[EDGE + 100] = 0xBB;
        fs::write(root.join("a.bin"), &a).unwrap();
        fs::write(root.join("b.bin"), &b).unwrap();

        let tree = scan(root);
        let report = duplicates::find(&tree, &opts());

        assert!(
            report.groups.is_empty(),
            "reported two different files as identical: {:?}",
            report.groups
        );
    }

    /// The same shape, one byte over the two-window boundary, where the code
    /// takes the full-hash path.
    #[test]
    fn differing_past_two_windows_is_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut a = vec![1u8; 2 * EDGE + 1];
        let mut b = vec![1u8; 2 * EDGE + 1];
        a[EDGE + 100] = 0xAA;
        b[EDGE + 100] = 0xBB;
        fs::write(root.join("a.bin"), &a).unwrap();
        fs::write(root.join("b.bin"), &b).unwrap();

        let tree = scan(root);
        assert!(duplicates::find(&tree, &opts()).groups.is_empty());
    }

    #[test]
    fn files_smaller_than_one_window_still_compare_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("same-a.bin"), vec![7u8; 4096]).unwrap();
        fs::write(root.join("same-b.bin"), vec![7u8; 4096]).unwrap();

        let mut different = vec![7u8; 4096];
        different[4095] = 9;
        fs::write(root.join("other.bin"), &different).unwrap();

        let tree = scan(root);
        let report = duplicates::find(&tree, &opts());

        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].nodes.len(), 2, "only the identical pair");
    }

    /// I/O totals must survive a group that turns out to hold no duplicates —
    /// the bytes were still read, and the report is what tells a user why the
    /// hunt took as long as it did.
    #[test]
    fn io_is_accounted_even_when_nothing_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut a = vec![0u8; 64 * 1024];
        let mut b = vec![0u8; 64 * 1024];
        a[0] = 1;
        b[0] = 2;
        fs::write(root.join("a.bin"), &a).unwrap();
        fs::write(root.join("b.bin"), &b).unwrap();

        let tree = scan(root);
        let report = duplicates::find(&tree, &opts());

        assert!(report.groups.is_empty());
        assert_eq!(report.candidates_examined, 2);
        assert!(
            report.bytes_hashed > 0,
            "two files were read but the report claims no I/O"
        );
    }
}

/// The engine's central guarantee: no node is offered by two findings, so the
/// panel's `reclaimable` figures can be added together without overstating.
mod disjointness {
    use super::*;
    use kosha_core::NodeId;
    use rustc_hash::FxHashSet;

    /// Every node claimed by a destructive finding, across the whole panel.
    fn claimed(insights: &[Insight]) -> Vec<NodeId> {
        insights
            .iter()
            .filter(|i| i.action.is_destructive())
            .flat_map(|i| i.nodes.iter().copied())
            .collect()
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A project whose build output also sits under a cache-shaped path, so
        // more than one rule has a reason to look at it.
        fs::create_dir_all(root.join("proj")).unwrap();
        fs::write(root.join("proj/package.json"), b"{}").unwrap();
        write(&root.join("proj/node_modules/big.bin"), 8);
        write(&root.join("proj/logs/app.log"), 60);
        write(&root.join("Downloads/installer.exe"), 150);
        write(&root.join("var/service.log"), 80);
        dir
    }

    #[test]
    fn no_node_is_claimed_twice() {
        let dir = fixture();
        let tree = scan(dir.path());
        let insights = analyze(&tree, &ctx());

        let all = claimed(&insights);
        let unique: FxHashSet<NodeId> = all.iter().copied().collect();
        assert_eq!(
            all.len(),
            unique.len(),
            "a node appears in more than one destructive finding"
        );
    }

    #[test]
    fn no_claimed_node_lies_under_another_claimed_node() {
        let dir = fixture();
        let tree = scan(dir.path());
        let insights = analyze(&tree, &ctx());
        let claimed: FxHashSet<NodeId> = claimed(&insights).into_iter().collect();

        for &node in &claimed {
            let mut cursor = node;
            while cursor != tree.root() {
                cursor = NodeId::from_index(tree.node(cursor).parent as usize);
                assert!(
                    !claimed.contains(&cursor),
                    "{} is claimed, and so is its ancestor {} — their bytes overlap",
                    tree.path_of(node).display(),
                    tree.path_of(cursor).display(),
                );
            }
        }
    }

    #[test]
    fn reclaimable_equals_the_bytes_of_the_nodes_claimed() {
        let dir = fixture();
        let tree = scan(dir.path());
        let insights = analyze(&tree, &ctx());

        for insight in &insights {
            if !insight.action.is_destructive() {
                continue;
            }
            let summed: u64 = insight
                .nodes
                .iter()
                .map(|&id| tree.node(id).allocated)
                .sum();
            assert_eq!(
                insight.reclaimable, summed,
                "`{}` claims {} but its nodes hold {}",
                insight.rule, insight.reclaimable, summed
            );
        }
    }

    /// The same overlap, where it is actually reachable: a user's rule inside a
    /// built-in one.
    ///
    /// A pack rule with a marker file earns `Certain`, and the engine ranks by
    /// confidence — so a `Certain` finding about a directory *inside* a `Likely`
    /// one is claimed first. The trimming drops a node whose ancestor is already
    /// claimed, which is this same overlap seen from the other end, and there is
    /// nothing that drops or adjusts a node whose *descendant* is claimed. The
    /// directory keeps its whole `allocated` — which includes the bytes the other
    /// finding has already offered.
    ///
    /// Not a contrived shape: `~/.cache/mytool`, `go/pkg/mod/download`,
    /// `node_modules/.cache` are all a recognised directory inside a recognised
    /// directory, and packs exist so that people can name the ones kosha does not
    /// know.
    #[test]
    fn a_users_rule_inside_a_built_in_one_does_not_claim_the_same_bytes_twice() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // `Cache` is an app-cache directory: Likely, and one insight.
        write(&root.join("Cache/mytool/blob.bin"), 8);
        // The marker is what earns the pack rule `Certain`, and it sits *beside*
        // the directory it proves, which is what `beside` means.
        write(&root.join("Cache/mytool.toml"), 0);
        write(&root.join("Cache/other/spill.bin"), 4);

        let pack = kosha_analyze::pack::parse(
            r#"{
                "version": 1,
                "rules": [
                    {
                        "id": "mytool-cache",
                        "title": "My tool's cache",
                        "detail": "Rebuilt on the next run.",
                        "subject": "cache directory",
                        "directory": "mytool",
                        "beside": ["mytool.toml"],
                        "action": "trash"
                    }
                ]
            }"#,
            std::path::Path::new("rules.json"),
            &[],
        )
        .expect("the pack loads");
        assert!(pack.rejected.is_empty(), "{:?}", pack.rejected);

        let tree = scan(root);
        let insights = kosha_analyze::findings_with(&tree, &ctx(), &pack.rules);

        let claimed: Vec<&Insight> = insights
            .iter()
            .filter(|i| i.action.is_destructive())
            .collect();
        assert!(
            claimed
                .iter()
                .any(|i| i.rule == RuleId::new("mytool-cache")),
            "the pack rule did not fire, so this proves nothing: {:?}",
            insights
                .iter()
                .map(|i| i.rule.to_string())
                .collect::<Vec<_>>()
        );
        assert!(
            claimed.iter().any(|i| i.rule == RuleId::new("app-cache")),
            "the built-in rule did not fire, so this proves nothing: {:?}",
            insights
                .iter()
                .map(|i| i.rule.to_string())
                .collect::<Vec<_>>()
        );

        // The bytes under `Cache` are the most anything can offer to free, and
        // two findings between them must not offer more than that.
        let cache = tree
            .children(tree.root())
            .find(|&id| tree.name_lossy(id) == "Cache")
            .expect("the fixture has a Cache directory");
        let ceiling = tree.node(cache).allocated;
        let offered = kosha_analyze::total_reclaimable(&insights);
        assert!(
            offered <= ceiling,
            "two findings offer {offered} bytes out of a directory holding {ceiling}"
        );
        // And exactly, not merely within bounds: between them the two findings
        // cover the whole of `Cache` — one empties it, the other trashes a
        // directory inside it — so the panel's figure is that directory's bytes
        // and not a byte more or less.
        assert_eq!(
            offered, ceiling,
            "the panel offers {offered} of the {ceiling} bytes the two findings \
             cover between them"
        );
    }

    /// No byte may be claimed twice, which is stronger than no *node* twice.
    ///
    /// `total_reclaimable` sums each finding's own figure, and a directory's
    /// `allocated` is its whole subtree — so two findings, one holding a
    /// directory and one holding something inside it, add that something's bytes
    /// up twice however disjoint their node lists look. The de-overlapping drops
    /// a node whose *ancestor* is already claimed; a node whose *descendant* is
    /// already claimed is the same overlap seen from the other end.
    ///
    /// The fixture is built to produce exactly that pair: a stale installer, big
    /// and old, inside a directory the cache rules recognise.
    #[test]
    fn no_finding_claims_a_directory_another_has_claimed_inside_of() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A cache the rules know by name, with an installer inside it that the
        // installer rule knows by name.
        write(&root.join("AppData/Local/Thing/Cache/setup-1.2.exe"), 8);
        write(&root.join("AppData/Local/Thing/Cache/blob.dat"), 4);
        // And an ordinary large cold file elsewhere, so the panel has more than
        // one finding to rank.
        write(&root.join("archive/old-video.mkv"), 12);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());

        let ancestor_of = |id: kosha_core::NodeId, of: kosha_core::NodeId| {
            let mut cur = of;
            while cur != kosha_core::NodeId::ROOT {
                cur = kosha_core::NodeId::from_index(tree.node(cur).parent as usize);
                if cur == id {
                    return true;
                }
            }
            false
        };

        let claimed: Vec<(String, kosha_core::NodeId)> = insights
            .iter()
            .filter(|i| i.action.is_destructive())
            .flat_map(|i| i.nodes.iter().map(move |&n| (i.rule.to_string(), n)))
            .collect();

        for (rule_a, a) in &claimed {
            for (rule_b, b) in &claimed {
                if a == b && rule_a == rule_b {
                    continue;
                }
                assert!(
                    !ancestor_of(*a, *b),
                    "`{rule_a}` claims a directory that `{rule_b}` claims inside of, \
                     so its bytes are counted twice"
                );
            }
        }
    }

    #[test]
    fn the_headline_total_never_exceeds_the_tree() {
        let dir = fixture();
        let tree = scan(dir.path());
        let insights = analyze(&tree, &ctx());

        assert!(
            kosha_analyze::total_reclaimable(&insights) <= tree.total_allocated(),
            "the panel offers to free more than the tree contains"
        );
    }

    #[test]
    fn findings_carry_every_node_they_cover() {
        // Node lists are data, not a display list. A view truncates; the engine
        // must not, or trimming overlaps would corrupt the totals.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..40 {
            write(&root.join(format!("logs/app.log.{i}")), 3);
        }

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let logs = find(&insights, "log-files").expect("40 log files");

        assert_eq!(logs.nodes.len(), 40, "node list was truncated");
        assert_eq!(logs.superseded, 0, "nothing else claimed these");
    }

    #[test]
    fn nodes_are_ordered_largest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("logs/small.log"), 20);
        write(&root.join("logs/huge.log"), 90);
        write(&root.join("logs/medium.log"), 50);

        let tree = scan(root);
        let insights = analyze(&tree, &ctx());
        let logs = find(&insights, "log-files").unwrap();

        let sizes: Vec<u64> = logs
            .nodes
            .iter()
            .map(|&id| tree.node(id).allocated)
            .collect();
        assert!(
            sizes.windows(2).all(|w| w[0] >= w[1]),
            "not largest-first: {sizes:?}"
        );
    }
}

// ------------------------------------ findings that are not produced by rules

/// The property that lets `fragmented` and `duplicates` run outside the engine
/// and be appended to its output.
///
/// A finding that needs I/O cannot be a rule, because rules are pure. It is
/// therefore produced afterwards and pushed onto the vector `analyze` returned
/// — which is only sound because the engine's de-overlapping deliberately
/// leaves non-destructive findings alone ("signposts describe rather than
/// propose"), and because the headline total counts only destructive ones.
///
/// If either of those ever changes, an appended finding starts either stealing
/// nodes from a real one or inflating the number a user acts on, and this fails.
#[test]
fn a_review_only_finding_can_be_appended_after_the_engine_has_run() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("project/target/debug/big.bin"), 8);
    write(&dir.path().join("project/Cargo.toml"), 0);
    let tree = scan(dir.path());

    let mut insights = analyze(&tree, &ctx());
    let before = kosha_analyze::total_reclaimable(&insights);
    assert!(before > 0, "the fixture has to produce a real finding");

    // Deliberately claiming the same nodes a real finding already claimed, and
    // a great many bytes, to prove neither is disturbed.
    let every_node: Vec<kosha_core::NodeId> = (0..tree.len())
        .map(kosha_core::NodeId::from_index)
        .collect();
    insights.push(kosha_analyze::fragmented::report(
        &every_node
            .iter()
            .map(|&node| kosha_analyze::fragmented::Fragmented {
                node,
                pieces: 99,
                allocated: 1 << 40,
            })
            .collect::<Vec<_>>(),
    ));
    kosha_analyze::rank(&mut insights);

    assert_eq!(
        kosha_analyze::total_reclaimable(&insights),
        before,
        "a signpost that frees nothing changed the figure the user acts on"
    );

    let appended = find(&insights, "fragmented-files").expect("it survived ranking");
    assert_eq!(appended.reclaimable, 0);
    assert!(!appended.action.is_destructive());
    assert_eq!(
        appended.nodes.len(),
        tree.len(),
        "a signpost is not trimmed against what other findings claimed"
    );

    // And it ranks below everything that frees something, because that is what
    // ranking is by. Not necessarily *last*: the other signposts also free
    // nothing, and among those the order falls back to the rule id.
    let at = insights
        .iter()
        .position(|i| i.rule == RuleId::new("fragmented-files"))
        .unwrap();
    let last_with_bytes = insights.iter().rposition(|i| i.reclaimable > 0);
    assert!(
        last_with_bytes.is_some_and(|real| at > real),
        "a finding that frees nothing outranked one that frees something"
    );
}

// ------------------------------------------------------------- rule packs

/// A pack rule is evaluated by kosha, not by the pack: it reads names and
/// paths out of the arena and returns nodes, exactly as a built-in does. The
/// validation decides what a rule may *claim*; this is what it may *do*.
#[test]
fn a_pack_rule_finds_what_it_describes_and_nothing_else() {
    use kosha_analyze::Rule;

    let dir = tempfile::tempdir().unwrap();
    // A directory that matches and has its proof beside it.
    write(&dir.path().join("scene/renders/frame.exr"), 4);
    fs::write(dir.path().join("scene/project.blend"), b"x").unwrap();
    // The same name with no proof beside it, which must not be claimed.
    write(&dir.path().join("elsewhere/renders/frame.exr"), 4);
    let tree = scan(dir.path());

    let loaded = kosha_analyze::pack::parse(
        r#"{
            "version": 1,
            "rules": [{
                "id": "render-output",
                "title": "Render output",
                "detail": "Regenerated by re-rendering the scene.",
                "subject": "project",
                "directory": "renders",
                "beside": ["project.blend"],
                "action": "trash"
            }]
        }"#,
        std::path::Path::new("rules.json"),
        &[],
    )
    .unwrap();
    assert!(loaded.rejected.is_empty());

    let found = loaded.rules[0].evaluate(&tree, &ctx());
    assert_eq!(found.len(), 1);
    let insight = &found[0];

    assert_eq!(insight.rule, RuleId::owned("render-output"));
    assert_eq!(insight.title, "Render output");
    assert_eq!(insight.confidence, Confidence::Certain);
    assert_eq!(insight.action, Suggested::Trash);
    assert_eq!(
        insight.nodes.len(),
        1,
        "the `renders` with no marker beside it was claimed anyway"
    );
    assert_eq!(
        tree.path_of(insight.nodes[0]),
        kosha_core::path::canonical(&dir.path().join("scene/renders"))
    );
}

/// A pack cannot opt out of the user's own floor, any more than a built-in can.
#[test]
fn a_pack_rule_respects_the_reclaimable_floor() {
    use kosha_analyze::Rule;

    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("scene/renders/frame.exr"), 1);
    fs::write(dir.path().join("scene/project.blend"), b"x").unwrap();
    let tree = scan(dir.path());

    let loaded = kosha_analyze::pack::parse(
        r#"{
            "version": 1,
            "rules": [{
                "id": "render-output", "title": "Render output",
                "detail": "Regenerated.", "subject": "project",
                "directory": "renders", "beside": ["project.blend"],
                "action": "trash"
            }]
        }"#,
        std::path::Path::new("rules.json"),
        &[],
    )
    .unwrap();

    let high = Context {
        min_reclaimable: 100 * MB as u64,
        ..ctx()
    };
    assert!(
        loaded.rules[0].evaluate(&tree, &high).is_empty(),
        "a pack rule reported a finding below the user's own threshold"
    );
}

/// The invariant the pack validator enforces, asserted against the catalogue it
/// was derived from.
///
/// If a built-in rule ever pairs `Possible` with a destructive action, the
/// discipline packs are held to stops matching the one kosha holds itself to —
/// and the argument for rejecting a user's rule evaporates.
#[test]
fn no_built_in_rule_offers_to_delete_on_a_possible() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("project/target/debug/big.bin"), 8);
    fs::write(dir.path().join("project/Cargo.toml"), b"").unwrap();
    write(&dir.path().join("Downloads/installer.dmg"), 200);
    write(&dir.path().join("logs/app.log"), 60);
    let tree = scan(dir.path());

    for insight in analyze(&tree, &ctx()) {
        if insight.confidence == Confidence::Possible {
            assert!(
                !insight.action.is_destructive(),
                "`{}` is only Possible and still offers `{:?}` — packs are \
                 rejected for exactly this",
                insight.rule,
                insight.action,
            );
        }
    }
}

/// User rules go through the engine's de-overlapping like everything else.
///
/// This is why they are evaluated before it rather than appended after: a pack
/// rule claiming a directory a built-in already claimed would otherwise have
/// its bytes counted twice in the headline, which is the exact number somebody
/// acts on.
#[test]
fn a_pack_rule_cannot_claim_bytes_a_built_in_already_claimed() {
    let dir = tempfile::tempdir().unwrap();
    // `target` beside a `Cargo.toml` is claimed by `build-artifacts`, Certain.
    write(&dir.path().join("project/target/debug/big.bin"), 8);
    fs::write(dir.path().join("project/Cargo.toml"), b"").unwrap();
    let tree = scan(dir.path());

    // A user rule that claims exactly the same directory, by a marker of its
    // own, and would be Certain too.
    let loaded = kosha_analyze::pack::parse(
        r#"{
            "version": 1,
            "rules": [{
                "id": "my-target", "title": "My build output",
                "detail": "Claimed by a pack as well.", "subject": "project",
                "directory": "target", "beside": ["Cargo.toml"],
                "action": "trash"
            }]
        }"#,
        std::path::Path::new("rules.json"),
        &[],
    )
    .unwrap();
    assert!(loaded.rejected.is_empty());

    let insights = kosha_analyze::findings_with(&tree, &ctx(), &loaded.rules);
    let total = kosha_analyze::total_reclaimable(&insights);
    let alone = kosha_analyze::total_reclaimable(&analyze(&tree, &ctx()));

    assert_eq!(
        total, alone,
        "the same bytes were counted twice once a pack rule claimed them"
    );

    // And the engine's central guarantee still holds with a user rule in the
    // mix: no node is *offered* by two findings. Scoped to the destructive ones
    // for the reason the engine scopes it — a signpost describes rather than
    // proposes, claims no bytes, and is deliberately left alone.
    let mut seen = std::collections::HashSet::new();
    for insight in insights.iter().filter(|i| i.action.is_destructive()) {
        for &node in &insight.nodes {
            assert!(
                seen.insert(node),
                "`{}` offered a node another finding already had",
                insight.rule
            );
        }
    }
}

/// A user rule that finds something nobody else claimed shows up beside the
/// built-ins, ranked by the same arithmetic.
#[test]
fn a_pack_rule_stands_beside_the_built_in_findings() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("scene/renders/frame.exr"), 40);
    fs::write(dir.path().join("scene/project.blend"), b"x").unwrap();
    let tree = scan(dir.path());

    let loaded = kosha_analyze::pack::parse(
        r#"{
            "version": 1,
            "rules": [{
                "id": "render-output", "title": "Render output",
                "detail": "Regenerated by re-rendering the scene.",
                "subject": "project", "directory": "renders",
                "beside": ["project.blend"], "action": "trash"
            }]
        }"#,
        std::path::Path::new("rules.json"),
        &[],
    )
    .unwrap();

    let insights = kosha_analyze::findings_with(&tree, &ctx(), &loaded.rules);
    let mine = find(&insights, "render-output").expect("the user's rule fired");
    assert_eq!(mine.confidence, Confidence::Certain);
    assert!(mine.reclaimable > 0);
    assert!(
        kosha_analyze::total_reclaimable(&insights) >= mine.reclaimable,
        "a user rule's bytes are missing from the headline"
    );
}

// ------------------------------------- what a finding is allowed to say aloud

/// A finding's own prose carries no control characters.
///
/// **Measured, and the worst thing found in the panel so far.** A rule pack is a
/// JSON file people swap around; this one's `detail` held a newline and two
/// spaces, and the panel printed a second finding that does not exist —
/// `[  99.9 GiB]  Delete everything (safe)` over
/// `confidence certain · trash 1 directory` — in kosha's own layout, directly
/// under a real one. The `title` carried a bare escape, which went to the
/// terminal as an escape.
///
/// Every name the panel prints beside these strings has gone through
/// `format::printable` since the scanner first met a Linux directory. The two
/// strings the rule writes itself were the gap, and the engine closes it on the
/// way out for every finding at once.
#[test]
fn a_pack_rule_cannot_forge_a_row_out_of_its_own_text() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("junk/big.bin"), 8);
    let tree = scan(dir.path());

    let loaded = kosha_analyze::pack::parse(
        r#"{
            "version": 1,
            "rules": [{
                "id": "hostile",
                "title": "Junk\u001b[31m",
                "detail": "harmless looking\n  [  99.9 GiB]  Delete everything (safe)\n               confidence certain · trash 1 directory",
                "subject": "directory\u001b[0m",
                "directory": "junk", "action": "review-only"
            }]
        }"#,
        std::path::Path::new("rules.json"),
        &[],
    )
    .unwrap();
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);

    let insights = kosha_analyze::findings_with(&tree, &ctx(), &loaded.rules);
    let mine = find(&insights, "hostile").expect("the pack rule fired");

    for (what, text) in [
        ("title", &mine.title),
        ("detail", &mine.detail),
        ("subject", &mine.subject),
    ] {
        assert!(
            !text.chars().any(char::is_control),
            "a pack's {what} reached a front end with a control character in it: {text:?}"
        );
    }
    // The prose is still there — the characters are replaced, not the sentence,
    // so a pack author who typed a tab still gets their rule.
    assert!(mine.title.starts_with("Junk"), "{:?}", mine.title);
    assert!(
        mine.detail.starts_with("harmless looking"),
        "{:?}",
        mine.detail
    );
}

/// The same guarantee for the built-in rules, which put names into their prose.
///
/// Five of them do, and a name is not ours either: every byte except `/` and NUL
/// is a legal filename on Linux, so the directory below is a directory somebody
/// can make on purpose. Windows refuses these names outright — measured, with
/// `CreateFileW` and a `\\?\` prefix, which still answers `ERROR_INVALID_NAME` —
/// but a name also arrives from a disk image and from a snapshot taken
/// elsewhere, and neither of those cares what Windows allows.
#[test]
#[cfg(unix)]
fn a_name_with_an_escape_in_it_does_not_reach_the_panel_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let hostile = format!("logs{}[31m{}forged", char::from(27), char::from(10));
    write(&dir.path().join(&hostile).join("big.bin"), 8);
    let tree = scan(dir.path());

    let insights = analyze(&tree, &ctx());
    let found = find(&insights, "single-dominant-child").expect("one directory holds everything");
    assert!(
        !found.detail.chars().any(char::is_control),
        "a filename put a control character in a finding: {:?}",
        found.detail
    );
    // And it is still the directory it names, with the characters stood in for.
    assert!(
        found.detail.contains("logs?[31m?forged"),
        "{:?}",
        found.detail
    );
}
