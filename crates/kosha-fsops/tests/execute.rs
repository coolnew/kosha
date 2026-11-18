// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! End-to-end tests for the plan, dry run and execute path.
//!
//! This is the crate that can destroy a user's data, so these tests are written
//! from the position that every guard is load-bearing until proven otherwise.
//! Most of them assert that something did *not* happen.
//!
//! Execution uses `DeletePermanently` and `EmptyContents` rather than `Trash`.
//! Not to avoid the code path — `Trash` is covered by dry runs and by an
//! explicitly-run test at the bottom — but because a normal `cargo test` should
//! not deposit files in the developer's Recycle Bin.

use std::fs;
use std::path::Path;

use kosha_core::{NodeId, ScanOptions, Scanner, SizeTree};
use kosha_fsops::{Action, ActionPlan, AuditLog, Outcome, Session, SkipReason, UserConfirmed};

/// ```text
/// root/
///   cache/          (a directory whose contents are disposable)
///     a.bin  4 KiB
///     b.bin  4 KiB
///     nested/c.bin  4 KiB
///   keep.txt        (must survive everything)
///   junk.bin  8 KiB
/// ```
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("cache/nested")).unwrap();
    fs::write(root.join("cache/a.bin"), vec![0u8; 4096]).unwrap();
    fs::write(root.join("cache/b.bin"), vec![0u8; 4096]).unwrap();
    fs::write(root.join("cache/nested/c.bin"), vec![0u8; 4096]).unwrap();
    fs::write(root.join("keep.txt"), b"important").unwrap();
    fs::write(root.join("junk.bin"), vec![0u8; 8192]).unwrap();
    dir
}

fn scan(root: &Path) -> SizeTree {
    Scanner::new(ScanOptions::default()).scan(root).unwrap()
}

fn node(tree: &SizeTree, relative: &str) -> NodeId {
    let mut cursor = tree.root();
    for part in relative.split('/') {
        cursor = tree
            .child_named(cursor, std::ffi::OsStr::new(part))
            .unwrap_or_else(|| panic!("no node at {relative}"));
    }
    cursor
}

/// A session already switched into read-write mode.
fn writable(root: &Path) -> Session {
    let mut session = Session::new(root);
    session.enable_read_write(UserConfirmed::from_dialog_acceptance());
    session
}

// ------------------------------------------------------------------ dry runs

#[test]
fn a_dry_run_changes_nothing() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin")],
        Action::DeletePermanently,
        &session,
    );
    let report = plan.dry_run();

    assert!(report.dry_run);
    assert_eq!(report.succeeded, 1);
    assert_eq!(report.failed, 0);
    assert!(
        dir.path().join("junk.bin").exists(),
        "a dry run must not touch the filesystem"
    );
    assert!(matches!(report.details[0].1, Outcome::WouldDo(_)));
}

#[test]
fn a_dry_run_needs_no_grant() {
    // Read-only sessions must still be able to preview, which is what lets the
    // UI show what read-write mode would offer without enabling it.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = Session::new(dir.path());
    assert!(session.grant().is_err(), "session is read-only");

    let plan = ActionPlan::build(&tree, &[node(&tree, "junk.bin")], Action::Trash, &session);
    assert_eq!(plan.dry_run().succeeded, 1);
    assert!(dir.path().join("junk.bin").exists());
}

#[test]
fn the_dry_run_and_the_execution_agree() {
    // The whole point of the dry run: the numbers a user approves come out of
    // the same validation path that will run.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin"), node(&tree, "cache/a.bin")],
        Action::DeletePermanently,
        &session,
    );

    let preview = plan.dry_run();
    let grant = session.grant().unwrap();
    let actual = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(preview.succeeded, actual.succeeded);
    assert_eq!(preview.skipped, actual.skipped);
    assert_eq!(preview.failed, actual.failed);
    assert_eq!(preview.bytes_freed, actual.bytes_freed);
}

// ----------------------------------------------------------------- execution

#[test]
fn permanent_deletion_removes_the_file() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin")],
        Action::DeletePermanently,
        &session,
    );
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.succeeded, 1);
    assert_eq!(report.failed, 0);
    assert!(!dir.path().join("junk.bin").exists());
    assert!(
        dir.path().join("keep.txt").exists(),
        "an unrelated file was removed"
    );
    assert_eq!(report.bytes_freed, 8192);
    assert_eq!(report.bytes_trashed, 0);
}

#[test]
fn emptying_a_directory_keeps_the_directory() {
    // Cache directories are emptied, not removed: the tool that owns one
    // expects to still find it there.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "cache")],
        Action::EmptyContents,
        &session,
    );
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.failed, 0, "{:?}", report.details);
    let cache = dir.path().join("cache");
    assert!(cache.is_dir(), "the directory itself must survive");
    assert_eq!(
        fs::read_dir(&cache).unwrap().count(),
        0,
        "contents should be gone"
    );
    assert!(dir.path().join("keep.txt").exists());
}

#[test]
fn emptying_removes_nested_directories_too() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "cache")],
        Action::EmptyContents,
        &session,
    );
    let grant = session.grant().unwrap();
    plan.execute(&grant, &mut AuditLog::disabled());

    assert!(!dir.path().join("cache/nested").exists());
}

#[test]
fn emptying_reports_bytes_as_freed_not_trashed() {
    // EmptyContents is typed irreversible and removes outright, because the
    // trash is on the same volume: trashing a cache would free nothing.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "cache")],
        Action::EmptyContents,
        &session,
    );
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert!(report.bytes_freed > 0);
    assert_eq!(report.bytes_trashed, 0);
    assert!(!report.summary().contains("trash"));
}

#[test]
fn a_trash_plan_reports_moved_rather_than_freed() {
    // Dry run only: the space does not come back until the trash is emptied,
    // and saying "freed" would be a lie the user acts on.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(&tree, &[node(&tree, "junk.bin")], Action::Trash, &session);
    let report = plan.dry_run();

    assert_eq!(report.bytes_freed, 0);
    assert_eq!(report.bytes_trashed, 8192);
    assert!(report.summary().contains("trash"), "{}", report.summary());
    assert!(!report.summary().contains("would free"));
}

// -------------------------------------------------------------- verification

#[test]
fn a_file_that_changed_since_the_scan_is_skipped() {
    // A scan is an observation. Acting on a stale one is how a tool deletes
    // something the user just recreated.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin")],
        Action::DeletePermanently,
        &session,
    );

    fs::write(dir.path().join("junk.bin"), vec![1u8; 999]).unwrap();

    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.succeeded, 0);
    assert_eq!(report.skipped, 1);
    assert!(
        dir.path().join("junk.bin").exists(),
        "the changed file must survive"
    );
    assert!(matches!(
        report.details[0].1,
        Outcome::Skipped(SkipReason::Changed)
    ));
}

/// The same file, the same length, rewritten between the plan and the run.
///
/// The test above changes the length, which any check would notice. This is the
/// one that says what the fingerprint is made of: a cache file rewritten with
/// the same amount of data, inside the same second, is a *different file* whose
/// bytes the user has not seen and has not agreed to lose. The fingerprint held
/// its timestamp as whole seconds and went ahead and deleted it.
///
/// **The wait is 25 ms, and both ends of that are deliberate.** Long enough that
/// the filesystem can tell the two writes apart — NTFS takes its last-write time
/// from the system clock, which ticks about every 15 ms, so two writes inside one
/// tick genuinely carry the same timestamp and no check made of timestamps can
/// see the difference. Short enough that it is still the same second, which is
/// what the old fingerprint could not see and this one can.
///
/// The loop is for the second boundary: land across one and the old code would
/// have caught it too, so the attempt proves nothing and is retried.
#[test]
fn a_file_rewritten_to_the_same_length_in_the_same_second_is_still_a_change() {
    let seconds = |path: &std::path::Path| -> u64 {
        fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };

    for _ in 0..8 {
        let dir = fixture();
        let path = dir.path().join("junk.bin");
        let length = fs::metadata(&path).unwrap().len() as usize;
        let tree = scan(dir.path());
        let session = writable(dir.path());
        let planned = seconds(&path);

        let plan = ActionPlan::build(
            &tree,
            &[node(&tree, "junk.bin")],
            Action::DeletePermanently,
            &session,
        );

        std::thread::sleep(std::time::Duration::from_millis(25));
        fs::write(&path, vec![0xAAu8; length]).unwrap();
        if seconds(&path) != planned {
            continue; // A second ticked over; this attempt proves nothing.
        }

        let grant = session.grant().unwrap();
        let report = plan.execute(&grant, &mut AuditLog::disabled());

        assert_eq!(
            report.skipped, 1,
            "the file was rewritten and the plan went ahead anyway"
        );
        assert!(path.exists(), "a rewritten file must survive the plan");
        assert!(matches!(
            report.details[0].1,
            Outcome::Skipped(SkipReason::Changed)
        ));
        return;
    }
    panic!("eight attempts and every pair of writes crossed a second boundary");
}

#[test]
fn a_file_that_vanished_is_skipped_without_complaint() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin")],
        Action::DeletePermanently,
        &session,
    );

    fs::remove_file(dir.path().join("junk.bin")).unwrap();

    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.skipped, 1);
    assert_eq!(report.failed, 0, "someone else getting there first is fine");
}

#[test]
fn one_bad_item_does_not_abort_the_rest() {
    // Execution is per-item and never transactional; the filesystem gives us no
    // way to make it so, and stopping halfway would be worse than continuing.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[
            node(&tree, "cache/a.bin"),
            node(&tree, "junk.bin"),
            node(&tree, "cache/b.bin"),
        ],
        Action::DeletePermanently,
        &session,
    );

    // Make the middle item stale.
    fs::write(dir.path().join("junk.bin"), vec![2u8; 3]).unwrap();

    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.succeeded, 2);
    assert_eq!(report.skipped, 1);
    assert!(!dir.path().join("cache/a.bin").exists());
    assert!(!dir.path().join("cache/b.bin").exists());
    assert!(dir.path().join("junk.bin").exists());
}

// -------------------------------------------------------------------- guards

#[test]
fn a_path_outside_the_root_never_reaches_the_plan() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("root");
    let elsewhere = outer.path().join("elsewhere");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(root.join("inside.bin"), vec![0u8; 16]).unwrap();
    let victim = elsewhere.join("precious.txt");
    fs::write(&victim, b"do not touch").unwrap();

    // Scan the sibling, then try to act on it through a session rooted at
    // `root`. The plan must refuse before anything is executed.
    let other_tree = scan(&elsewhere);
    let session = writable(&root);

    let plan = ActionPlan::build(
        &other_tree,
        &[node(&other_tree, "precious.txt")],
        Action::DeletePermanently,
        &session,
    );

    assert!(plan.is_empty(), "an out-of-root path was planned");
    assert_eq!(plan.rejected().len(), 1);

    let grant = session.grant().unwrap();
    plan.execute(&grant, &mut AuditLog::disabled());
    assert!(victim.exists(), "a file outside the root was deleted");
}

#[test]
fn rejected_paths_are_reported_not_silently_dropped() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("root");
    let elsewhere = outer.path().join("elsewhere");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("a.txt"), b"x").unwrap();

    let other_tree = scan(&elsewhere);
    let session = writable(&root);
    let plan = ActionPlan::build(
        &other_tree,
        &[node(&other_tree, "a.txt")],
        Action::DeletePermanently,
        &session,
    );

    let (path, reason) = &plan.rejected()[0];
    assert!(path.ends_with("a.txt"));
    assert!(
        reason.contains("outside"),
        "the reason should name the problem: {reason}"
    );
}

// ------------------------------------------------------------ plan mechanics

#[test]
fn a_plan_reports_its_total_before_running() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin"), node(&tree, "cache/a.bin")],
        Action::DeletePermanently,
        &session,
    );

    assert_eq!(plan.items().len(), 2);
    assert!(plan.total_bytes() >= 8192 + 4096);
    assert!(plan.has_irreversible());
}

#[test]
fn a_trash_plan_is_not_irreversible() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(&tree, &[node(&tree, "junk.bin")], Action::Trash, &session);
    assert!(!plan.has_irreversible());
}

#[test]
fn emptying_contents_counts_as_irreversible() {
    // It removes outright, so the confirmation dialog must show the warning.
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "cache")],
        Action::EmptyContents,
        &session,
    );
    assert!(plan.has_irreversible());
}

#[test]
fn an_empty_plan_executes_cleanly() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(&tree, &[], Action::DeletePermanently, &session);
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.succeeded, 0);
    assert_eq!(report.bytes_total(), 0);
}

// ----------------------------------------------------------------- audit log

#[test]
fn every_executed_action_is_recorded() {
    let dir = fixture();
    let log_dir = tempfile::tempdir().unwrap();
    let log_path = log_dir.path().join("audit.jsonl");

    let tree = scan(dir.path());
    let session = writable(dir.path());
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin"), node(&tree, "cache/a.bin")],
        Action::DeletePermanently,
        &session,
    );

    let mut audit = AuditLog::at(log_path.clone());
    let grant = session.grant().unwrap();
    plan.execute(&grant, &mut audit);
    drop(audit);

    let text = fs::read_to_string(&log_path).unwrap();
    assert_eq!(text.lines().count(), 2, "one line per executed action");
    assert!(text.contains("junk.bin"));
    assert!(text.contains(r#""outcome":"done""#));
    assert!(text.contains(r#""reversible":false"#));
}

#[test]
fn a_dry_run_writes_nothing_to_the_audit_log() {
    let dir = fixture();
    let log_dir = tempfile::tempdir().unwrap();
    let log_path = log_dir.path().join("audit.jsonl");

    let tree = scan(dir.path());
    let session = writable(dir.path());
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin")],
        Action::DeletePermanently,
        &session,
    );

    // Opening the log creates the file; a dry run must leave it empty.
    let audit = AuditLog::at(log_path.clone());
    drop(audit);
    plan.dry_run();

    assert_eq!(fs::read_to_string(&log_path).unwrap(), "");
}

#[test]
fn skipped_items_are_not_recorded_as_done() {
    let dir = fixture();
    let log_dir = tempfile::tempdir().unwrap();
    let log_path = log_dir.path().join("audit.jsonl");

    let tree = scan(dir.path());
    let session = writable(dir.path());
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin")],
        Action::DeletePermanently,
        &session,
    );
    fs::write(dir.path().join("junk.bin"), b"changed").unwrap();

    let mut audit = AuditLog::at(log_path.clone());
    let grant = session.grant().unwrap();
    plan.execute(&grant, &mut audit);
    drop(audit);

    assert_eq!(
        fs::read_to_string(&log_path).unwrap(),
        "",
        "nothing happened, so nothing should be logged as having happened"
    );
}

// ---------------------------------------------------------------------- mode

#[test]
fn returning_to_read_only_prevents_further_execution() {
    let dir = fixture();
    let tree = scan(dir.path());
    let mut session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "junk.bin")],
        Action::DeletePermanently,
        &session,
    );

    session.return_to_read_only();

    // The plan survives the mode change and can still be previewed — that is
    // what lets the UI keep showing what read-write mode would do. What it
    // cannot do is produce the grant that `execute` requires, so there is no
    // way to reach the filesystem from here.
    assert_eq!(plan.dry_run().succeeded, 1);
    assert!(session.grant().is_err());
    assert!(
        dir.path().join("junk.bin").exists(),
        "nothing ran without a grant"
    );
}

// --------------------------------------------------------- the real trash

/// Exercises the actual trash integration. Ignored by default because it puts
/// a real file in the platform trash; run it deliberately with
/// `cargo test -p kosha-fsops -- --ignored`.
///
/// It checks that the file is *recoverable*, not merely gone. Everything this
/// asserted before would have passed just as well against `remove_file`, which
/// is the one implementation Trash must not be — the whole difference between
/// this action and `DeletePermanently` is that this one can be undone, and the
/// UI labels it reversible on that basis.
///
/// It also puts the trash back as it found it, so running it does not leave
/// litter behind for whoever runs it next.
#[test]
#[ignore = "moves a real file to the platform trash"]
fn trashing_leaves_the_file_recoverable() {
    let dir = fixture();
    let tree = scan(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(&tree, &[node(&tree, "junk.bin")], Action::Trash, &session);
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.failed, 0, "{:?}", report.details);
    assert!(!dir.path().join("junk.bin").exists());
    // Trashed, not freed: the bytes are still on the volume until the trash is
    // emptied, and reporting them as reclaimed would be a lie the user acts on.
    assert_eq!(report.bytes_trashed, 8192);
    assert_eq!(report.bytes_freed, 0);

    // Listing the trash is supported on Windows and on Freedesktop-compliant
    // Unix, which is where this claim can actually be checked. macOS has no
    // equivalent API in the crate, so there the assertions above are all there
    // is.
    #[cfg(any(
        target_os = "windows",
        all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        )
    ))]
    {
        use std::ffi::OsStr;

        let ours: Vec<trash::TrashItem> = trash::os_limited::list()
            .expect("the platform trash could not be listed")
            .into_iter()
            .filter(|item| item.name == OsStr::new("junk.bin"))
            .filter(|item| item.original_parent == dir.path())
            .collect();

        assert!(
            !ours.is_empty(),
            "the file left its directory but is not in the trash — that is a \
             permanent delete wearing a reversible label"
        );

        // Leave the developer's trash as it was found.
        trash::os_limited::purge_all(ours).expect("could not purge the test item");
    }
}

/// Every file of exactly `bytes` sitting in this volume's recycle bin.
///
/// The bin is a directory — `C:\$Recycle.Bin\<SID>` — holding a `$R…` file per
/// deleted item and a `$I…` beside it with the item's original path. Reading it is
/// how a test can tell a reversible delete from a permanent one without asking the
/// shell, whose answer turned out to depend on the machine: `os_limited::list`
/// reported a deep item on a developer's box and nothing at all on a clean
/// Windows, for the same operation.
///
/// A directory that cannot be read is skipped rather than failing: another
/// account's recycle bin is not this test's business and is not readable anyway.
#[cfg(windows)]
fn recycled(bytes: u64) -> Vec<std::path::PathBuf> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let mut found = Vec::new();
    let Ok(accounts) = fs::read_dir(format!("{drive}\\$Recycle.Bin")) else {
        return found;
    };
    for account in accounts.flatten() {
        let Ok(items) = fs::read_dir(account.path()) else {
            continue;
        };
        for item in items.flatten() {
            if !item.file_name().to_string_lossy().starts_with("$R") {
                continue;
            }
            if item.metadata().is_ok_and(|meta| meta.len() == bytes) {
                found.push(item.path());
            }
        }
    }
    found
}

/// A path past `MAX_PATH`, which is where the Recycle Bin stops being reversible.
///
/// **The two machines this runs on answer differently, and the test says so.**
/// With `LongPathsEnabled` set — this developer's box — the shell recycles a file
/// 325 characters deep and it can be restored. On a clean Windows, which is the
/// default and what the sandbox boots, the same call reports success and the file
/// is gone: nothing of its size under `C:\$Recycle.Bin`, nothing in the shell's
/// listing. `trash::delete` returned `Ok` for it.
///
/// So the plan refuses it there, and this asserts whichever of those is true of the
/// machine it is running on — a delete that is reversible, or a refusal that says
/// why. What must never happen is the third thing, and that is the assertion with
/// the sentence borrowed from the test above: a file gone for good under a label
/// that says it can be undone.
///
/// Ignored by default with its neighbour, because it puts a real file in the real
/// trash, and run for real by the sandbox scenario.
#[cfg(windows)]
#[test]
#[ignore = "moves a real file to the platform trash"]
fn a_file_past_max_path_is_either_recoverable_or_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    while deep.as_os_str().len() < 300 {
        deep = deep.join("a-segment-of-some-thirty-chars");
    }
    fs::create_dir_all(&deep).unwrap();
    let victim = deep.join("junk.bin");
    // A size no other fixture uses: the recycle bin is shared, `recycled` finds
    // items by the bytes they hold, and the test beside this one trashes 8,192 of
    // them. Purging by size took that item out from under it, which is one test
    // reaching into another through the machine.
    fs::write(&victim, vec![7u8; 7_777]).unwrap();
    let length = victim.as_os_str().len();
    assert!(length > 260, "the fixture is only {length} characters deep");

    let tree = scan(dir.path());
    let session = writable(dir.path());
    // The arena holds every entry, so the file is findable without walking the
    // path back down through thirty-character segments.
    let id = (0..tree.len())
        .map(kosha_core::NodeId::from_index)
        .find(|&id| tree.name_lossy(id) == "junk.bin")
        .expect("the fixture's file is in the tree");

    let plan = ActionPlan::build(&tree, &[id], Action::Trash, &session);

    if !kosha_core::long_paths_enabled() {
        // The machine cannot recycle it, so the plan does not offer to.
        assert!(
            plan.items().is_empty(),
            "a path this machine cannot recycle was planned as a reversible delete"
        );
        let (path, why) = plan
            .rejected()
            .first()
            .expect("a refusal, with a reason in it");
        assert_eq!(path, &victim);
        assert!(
            why.contains("Recycle Bin") && why.contains(&length.to_string()),
            "the refusal does not say what is wrong: {why}"
        );
        assert!(victim.exists(), "the file went somewhere anyway");
        return;
    }

    // And where it can be recycled, it is — verifiably.
    assert!(
        plan.rejected().is_empty(),
        "the guard refused a path this machine can recycle: {:?}",
        plan.rejected()
    );
    let before = recycled(7_777);
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.failed, 0, "{:?}", report.details);
    assert!(!victim.exists(), "the file is still where it was");
    // On-disk bytes, not the length: 7,777 bytes occupies two clusters, and what
    // moving it to the trash does *not* free is what it occupies.
    assert_eq!(report.bytes_trashed, tree.node(id).allocated);

    let appeared: Vec<std::path::PathBuf> = recycled(7_777)
        .into_iter()
        .filter(|path| !before.contains(path))
        .collect();
    assert!(
        !appeared.is_empty(),
        "a file {length} characters deep left its directory and nothing of its \
         size appeared in the recycle bin — that is a permanent delete wearing a \
         reversible label"
    );

    // Leave the machine's recycle bin as it was found: only what appeared during
    // this test, and the `$I` twin that records where each came from. The shell's
    // own purge needs an item its listing produced, and the point of `recycled` is
    // that the listing does not always produce one.
    for path in appeared {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let twin = path.with_file_name(name.replacen("$R", "$I", 1));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&twin);
    }
}

// ---------------------------------------------------------------- compression

/// A directory of highly compressible files, plus one that is not.
fn compressible(dir: &Path) -> Vec<u8> {
    let text: Vec<u8> = b"the quick brown fox jumps over the lazy dog. "
        .iter()
        .copied()
        .cycle()
        .take(2 * 1024 * 1024)
        .collect();
    fs::create_dir_all(dir.join("logs/old")).unwrap();
    fs::write(dir.join("logs/a.log"), &text).unwrap();
    fs::write(dir.join("logs/old/b.log"), &text).unwrap();
    text
}

/// True where both the platform and the volume under `path` can compress.
///
/// Checked rather than assumed: a temp directory on exFAT or a network share
/// is a real configuration, and a test that fails there is testing the machine.
///
/// **Asked, not tried.** This used to compress the fixture to find out and then
/// uncompress it again, which is a capability probe that mutates — and it was
/// the only reason anything outside this crate needed a way to set compression.
/// The volume's own flags answer it for nothing.
fn can_compress(path: &Path) -> bool {
    kosha_core::compression_available_at(path)
}

#[test]
fn compressing_reclaims_space_without_losing_a_byte() {
    let dir = tempfile::tempdir().unwrap();
    let text = compressible(dir.path());
    let session = writable(dir.path());
    if !can_compress(dir.path()) {
        return;
    }

    let tree = scan(dir.path());
    let plan = ActionPlan::build(&tree, &[node(&tree, "logs")], Action::Compress, &session);

    let mut audit = AuditLog::disabled();
    let report = plan.execute(&session.grant().unwrap(), &mut audit);

    assert_eq!(report.failed, 0, "{:?}", report.details);
    assert_eq!(report.succeeded, 1);
    assert!(
        report.bytes_freed > 0,
        "compression reported no saving at all"
    );
    // Freed, not trashed: the space really is back, unlike a trashed file.
    assert_eq!(report.bytes_trashed, 0);

    // The whole point: every file still reads back exactly as written.
    for name in ["logs/a.log", "logs/old/b.log"] {
        assert_eq!(
            fs::read(dir.path().join(name)).unwrap(),
            text,
            "{name} came back different"
        );
    }
    // And the walk reached the nested one, not just the top level.
    let nested = kosha_core::allocated_size(&dir.path().join("logs/old/b.log")).unwrap();
    assert!(
        nested < text.len() as u64,
        "a file one level down was not compressed: {nested}"
    );
}

/// A compression that could not do every file still reports what it saved.
///
/// The same shape as an empty that could not finish, and it was wrong the same
/// way: the walk collected the last error and returned it *instead of* the
/// saving, so a run that had just recovered real space reported the item as
/// failed with nothing saved. Nothing is lost by compressing, so this is a
/// number rather than a file — but it is the number somebody checks their disk
/// against.
#[test]
fn a_compression_that_could_not_do_everything_reports_what_it_saved() {
    let dir = tempfile::tempdir().unwrap();
    let text = compressible(dir.path());
    let session = writable(dir.path());
    if !can_compress(dir.path()) {
        return;
    }
    // One file in the tree that cannot be opened for the control code.
    let held = hold_something_open(&dir.path().join("logs"));

    let tree = scan(dir.path());
    let plan = ActionPlan::build(&tree, &[node(&tree, "logs")], Action::Compress, &session);
    let report = plan.execute(&session.grant().unwrap(), &mut AuditLog::disabled());

    assert_eq!(report.partial, 1, "{report:?}");
    assert_eq!(report.failed, 0, "a saving is not a failure: {report:?}");
    assert!(
        report.bytes_freed > 0,
        "the files it did compress saved nothing: {report:?}"
    );
    assert!(
        !report.summary().contains("did nothing"),
        "{}",
        report.summary()
    );

    // And nothing was harmed on the way past.
    for name in ["logs/a.log", "logs/old/b.log"] {
        assert_eq!(fs::read(dir.path().join(name)).unwrap(), text, "{name}");
    }

    drop(held);
}

#[test]
fn compressing_is_reversible_and_not_destructive() {
    assert!(Action::Compress.is_reversible());
    assert!(!Action::Compress.is_destructive());
    // Everything else removes something.
    for action in [
        Action::Trash,
        Action::DeletePermanently,
        Action::EmptyContents,
    ] {
        assert!(action.is_destructive(), "{action:?}");
    }
}

#[test]
fn compressing_still_needs_read_write_mode() {
    // It is not destructive, but it is a mutation, and the grant is the only
    // thing standing between read-only mode and the filesystem.
    let dir = tempfile::tempdir().unwrap();
    compressible(dir.path());
    let tree = scan(dir.path());
    let session = Session::new(dir.path());

    assert!(
        session.grant().is_err(),
        "read-only mode handed out a grant"
    );
    let plan = ActionPlan::build(&tree, &[node(&tree, "logs")], Action::Compress, &session);
    let report = plan.dry_run();
    assert!(report.dry_run);
    assert_eq!(report.succeeded, 1);
}

#[test]
fn a_dry_run_promises_no_compression_ratio() {
    // The dialog is built from this report. Claiming a saving nothing has
    // measured is the same class of lie as reporting trashed bytes as freed,
    // which this crate already goes out of its way not to tell.
    let dir = tempfile::tempdir().unwrap();
    compressible(dir.path());
    let tree = scan(dir.path());
    let session = writable(dir.path());
    let plan = ActionPlan::build(&tree, &[node(&tree, "logs")], Action::Compress, &session);

    let report = plan.dry_run();
    assert_eq!(
        report.bytes_freed, 0,
        "a dry run claimed to know how much compression would save"
    );
    assert_eq!(report.bytes_trashed, 0);
    assert!(
        report.bytes_examined > 0,
        "the dialog has no figure at all to show"
    );

    // A deletion, by contrast, knows exactly what it will free.
    let deleting = ActionPlan::build(
        &tree,
        &[node(&tree, "logs")],
        Action::DeletePermanently,
        &session,
    );
    let report = deleting.dry_run();
    assert!(report.bytes_freed > 0);
    assert_eq!(report.bytes_freed, report.bytes_examined);
}

#[test]
fn a_dry_run_compresses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let text = compressible(dir.path());
    let session = writable(dir.path());
    if !can_compress(dir.path()) {
        return;
    }

    let before = kosha_core::allocated_size(&dir.path().join("logs/a.log")).unwrap();
    let tree = scan(dir.path());
    let plan = ActionPlan::build(&tree, &[node(&tree, "logs")], Action::Compress, &session);

    let report = plan.dry_run();
    assert!(report.dry_run);
    assert_eq!(
        kosha_core::allocated_size(&dir.path().join("logs/a.log")).unwrap(),
        before,
        "a dry run compressed a file"
    );
    assert_eq!(fs::read(dir.path().join("logs/a.log")).unwrap(), text);
}

#[test]
fn a_symlink_is_never_followed_into() {
    // Compressing a link's target would act outside the plan the user read.
    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        compressible(dir.path());
        let outside = dir.path().join("outside.bin");
        fs::write(&outside, vec![0u8; 1024]).unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("logs/link")).unwrap();

        let tree = scan(dir.path());
        let session = writable(dir.path());
        let plan = ActionPlan::build(&tree, &[node(&tree, "logs")], Action::Compress, &session);
        let mut audit = AuditLog::disabled();
        // Unsupported on Unix, so this reports a failure rather than acting —
        // what matters is that it did not chase the link first.
        let _ = plan.execute(&session.grant().unwrap(), &mut audit);
        assert_eq!(fs::read(&outside).unwrap().len(), 1024);
    }
}

/// A link inside a doomed directory is removed; what it points at is not.
///
/// The worst thing this crate could do is delete somebody's photographs
/// because a cache directory happened to contain a shortcut to them. Nothing
/// here had ever tested it, on either platform, and the answer is not obvious
/// from the code: the deletion is `std::fs::remove_dir_all`, whose treatment of
/// a Windows junction is a property of the standard library rather than of
/// anything in this repository — and junctions are what a Windows machine
/// actually has, since a symlink needs a privilege and a junction does not.
///
/// Measured rather than assumed, and the answer is the right one: the junction
/// goes, the target stays, and the bytes behind the link were never in the
/// figure the plan offered. This test is here so that stays true.
#[test]
fn a_link_in_a_doomed_directory_is_removed_without_what_it_points_at() {
    for action in [Action::DeletePermanently, Action::EmptyContents] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("victim")).unwrap();
        fs::write(root.join("victim/treasure.bin"), vec![7u8; 64 * 1024]).unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::write(root.join("cache/junk.bin"), vec![0u8; 4096]).unwrap();
        link_to_a_directory(&root.join("victim"), &root.join("cache/link"));

        let tree = scan(root);
        let session = writable(root);
        let plan = ActionPlan::build(&tree, &[node(&tree, "cache")], action, &session);

        // Sixteen times the doomed tree sits behind that link, and none of it
        // is in the promise: a plan that counted it would be offering bytes it
        // must not take.
        assert_eq!(
            plan.dry_run().bytes_freed,
            4096,
            "{action:?} counted what the link points at"
        );

        let grant = session.grant().unwrap();
        let report = plan.execute(&grant, &mut AuditLog::disabled());
        assert_eq!(report.failed, 0, "{action:?} failed: {report:?}");
        assert_eq!(report.bytes_freed, 4096, "{action:?}");

        assert!(
            !root.join("cache/link").exists(),
            "{action:?} left the link behind"
        );
        assert!(
            root.join("victim/treasure.bin").exists(),
            "{action:?} deleted through the link"
        );
        assert_eq!(
            fs::read(root.join("victim/treasure.bin")).unwrap().len(),
            64 * 1024,
            "{action:?} emptied what the link points at"
        );
    }
}

/// A directory link, of the kind the platform actually uses.
///
/// A junction on Windows rather than a symlink, because a symlink there needs
/// either developer mode or an elevated process while a junction needs neither
/// — so a junction is what is in a real user's directory, and it is also the
/// one the standard library's treatment of is least obvious.
fn link_to_a_directory(target: &Path, link: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let made = std::process::Command::new("cmd")
            .arg("/C")
            // `raw_arg`, because `mklink` is a built-in of `cmd` and the
            // quoting std applies for a real executable does not reach it.
            .raw_arg(format!(
                "mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
            .output()
            .expect("cmd runs");
        assert!(
            made.status.success(),
            "mklink /J: {}",
            String::from_utf8_lossy(&made.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("a symlink");

    assert!(
        fs::symlink_metadata(link)
            .expect("the link is there")
            .file_type()
            .is_symlink(),
        "the fixture did not make a link"
    );
}

/// And the link itself can be deleted, without taking its target with it.
///
/// It could not: a junction is a directory to Win32 and a not-directory to the
/// scanner, so the plan's `is_dir` said "file" and the deletion called
/// `remove_file`, which answers "Access is denied" on a junction. Measured —
/// `succeeded=0 failed=1`, the link still there — and an ordinary thing to ask
/// for, since a junction is exactly what somebody has when they have moved a
/// cache onto another drive and later wants the link gone.
#[test]
fn a_link_can_be_deleted_and_its_target_survives() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("victim")).unwrap();
    fs::write(root.join("victim/treasure.bin"), vec![7u8; 64 * 1024]).unwrap();
    link_to_a_directory(&root.join("victim"), &root.join("link"));

    let tree = scan(root);
    let session = writable(root);
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "link")],
        Action::DeletePermanently,
        &session,
    );
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(report.succeeded, 1, "{report:?}");
    assert!(
        fs::symlink_metadata(root.join("link")).is_err(),
        "the link is still there"
    );
    assert!(
        root.join("victim/treasure.bin").exists(),
        "deleting a link deleted what it pointed at"
    );
}

/// An empty that only partly worked says so, and says what it did remove.
///
/// **The likeliest way anybody uses this action is on a cache whose
/// application is running**, and then one file is held open. Measured, three
/// files with one of them held: two were permanently deleted and the report
/// read
///
/// ```text
/// succeeded=0 failed=1 freed=0
/// "did nothing across 0 items, 1 failed"
/// ```
///
/// — a false statement about two files that no longer existed — while the
/// audit log, which is written only for `Done`, recorded nothing at all. The
/// log is what the safety model leans on for "what happened to my files", and
/// this is exactly the case somebody would go to it with.
#[test]
fn an_empty_that_could_not_finish_reports_what_it_did() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("cache")).unwrap();
    for name in ["a.bin", "b.bin"] {
        fs::write(root.join("cache").join(name), vec![0u8; 4096]).unwrap();
    }
    let held = hold_something_open(&root.join("cache"));

    let tree = scan(root);
    let session = writable(root);
    let log = tempfile::tempdir().unwrap();
    let mut audit = AuditLog::at(log.path().join("actions.jsonl"));

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "cache")],
        Action::EmptyContents,
        &session,
    );
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut audit);

    assert_eq!(report.partial, 1, "{report:?}");
    assert_eq!(report.failed, 0, "a partial is not a failure: {report:?}");
    assert_eq!(report.succeeded, 0, "nor a success: {report:?}");
    assert_eq!(
        report.bytes_freed, 8192,
        "the two files that went are 8 KiB: {report:?}"
    );
    assert!(
        matches!(report.details[0].1, Outcome::Partial { .. }),
        "{:?}",
        report.details
    );

    // The sentence a person reads must not say "did nothing".
    let said = report.summary();
    assert!(said.contains("freed 8.00 KiB"), "{said}");
    assert!(!said.contains("did nothing"), "{said}");
    assert!(said.contains("partly"), "{said}");

    // And the log has a line for it, with what was actually removed.
    let written = fs::read_to_string(audit.path()).expect("the log was written");
    assert!(written.contains(r#""outcome":"partial""#), "{written}");
    assert!(written.contains(r#""bytes":8192"#), "{written}");

    drop(held);
}

/// Make one entry in `dir` impossible to remove, the way a running program
/// would.
///
/// On Windows a file held open with no sharing, which is what an application
/// does to its own cache file. On Unix an unwritable directory, since a file
/// is unlinked through its parent and an open file is no obstacle at all
/// there — the same situation stated in each platform's own terms.
fn hold_something_open(dir: &Path) -> Box<dyn std::any::Any> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        fs::write(dir.join("locked.bin"), vec![0u8; 4096]).unwrap();
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(dir.join("locked.bin"))
            .expect("hold it open");
        Box::new(handle)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        /// Puts the permissions back, so the temporary directory can be
        /// cleaned up after the test.
        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
            }
        }

        let held = dir.join("held");
        fs::create_dir_all(&held).unwrap();
        fs::write(held.join("inner.bin"), vec![0u8; 4096]).unwrap();
        fs::set_permissions(&held, fs::Permissions::from_mode(0o500)).unwrap();
        Box::new(Restore(held))
    }
}

/// And the same for a permanent delete, which is the one that cannot be undone.
///
/// **`remove_dir_all` is not all-or-nothing.** Measured on Windows, a directory
/// of three files with one of them held open: the other two and the
/// subdirectory holding them were gone, the call returned the error for the one
/// that stayed, and the report said `did nothing across 0 items, 1 failed`
/// about 12 KiB that no longer existed.
///
/// What is asserted is the *relationship*, not a number, because how much gets
/// removed before the error depends on the order a directory is read in and
/// that is the filesystem's business: whatever went, the report says exactly
/// that much, and it is called partial when something went and failed when
/// nothing did.
#[test]
fn a_permanent_delete_that_could_not_finish_reports_what_it_did() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("doomed/inner")).unwrap();
    for name in ["a.bin", "b.bin"] {
        fs::write(root.join("doomed/inner").join(name), vec![0u8; 4096]).unwrap();
    }
    fs::write(root.join("doomed/c.bin"), vec![0u8; 4096]).unwrap();
    let held = hold_something_open(&root.join("doomed"));

    let tree = scan(root);
    let before = scan(&root.join("doomed")).total_allocated();
    let session = writable(root);
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "doomed")],
        Action::DeletePermanently,
        &session,
    );
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert!(
        root.join("doomed").exists(),
        "the directory holding what could not go is still there"
    );
    let gone = before - scan(&root.join("doomed")).total_allocated();
    assert_eq!(
        report.bytes_freed, gone,
        "the report and the disk disagree about how much went: {report:?}"
    );
    if gone > 0 {
        assert_eq!(report.partial, 1, "{report:?}");
        assert_eq!(report.failed, 0, "{report:?}");
        assert!(
            !report.summary().contains("did nothing"),
            "{}",
            report.summary()
        );
    } else {
        assert_eq!(
            report.failed, 1,
            "nothing went, so this is a failure: {report:?}"
        );
        assert_eq!(report.partial, 0, "{report:?}");
    }

    drop(held);
}

/// A file that could not be deleted at all is a failure, not a partial.
///
/// The distinction is the whole point: "partly" says bytes are gone, and
/// saying that about a file still sitting on the disk would be the same lie in
/// the other direction.
#[test]
fn a_delete_that_did_nothing_is_still_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("cache")).unwrap();
    let held = hold_something_open(&root.join("cache"));
    let only = fs::read_dir(root.join("cache"))
        .unwrap()
        .flatten()
        .next()
        .expect("the held entry")
        .file_name();

    let tree = scan(root);
    let session = writable(root);
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, &format!("cache/{}", only.to_string_lossy()))],
        Action::DeletePermanently,
        &session,
    );
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());

    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(
        report.partial, 0,
        "nothing went, so nothing is partial: {report:?}"
    );
    assert_eq!(report.bytes_freed, 0, "{report:?}");

    drop(held);
}

/// Trashing on a share is refused with the reason, before anything is tried.
///
/// **The Recycle Bin is per-volume and local**, so a network location has
/// nowhere to put an item and no bookkeeping to record it in. Measured against
/// `\\wsl.localhost\archlinux\tmp`, which is a share this tool is expected to
/// be pointed at when scanning a WSL distribution:
/// `trash::delete` came back with the shell's own `0x80070002`, *"The system
/// cannot find the file specified"*, about a file that was sitting right
/// there. Nothing was lost; what was wrong was the account of it, and a
/// failure after the fact is worse than a refusal before it — the dialog can
/// offer a permanent delete instead.
///
/// A mapped drive letter needs no case of its own: `W:` mapped to that share
/// arrives here already resolved to `\\wsl.localhost\...`, which is `resolve`
/// doing its job and was confirmed by running this with both spellings.
///
/// The rule itself is pinned by `a_share_is_told_from_a_long_local_path` in
/// the guard, which runs everywhere. This is the end-to-end half, and it needs
/// a share to point at: set `KOSHA_TRASH_PROBE` to a directory on one and run
/// `cargo test -p kosha-fsops -- --ignored a_share_is_refused`.
///
/// **It stands down rather than failing when that is unset**, because the
/// sandbox's `read-write` scenario runs this file with `--include-ignored` on
/// a machine that has no share — which is how the first version of this test,
/// which panicked, failed a scenario that was otherwise green.
#[test]
#[ignore = "needs a network share; set KOSHA_TRASH_PROBE"]
fn a_share_is_refused_for_the_trash_and_not_for_a_delete() {
    let Ok(where_) = std::env::var("KOSHA_TRASH_PROBE") else {
        println!("no KOSHA_TRASH_PROBE: no share to point at, so nothing was checked");
        return;
    };
    let root = std::path::PathBuf::from(where_);
    fs::create_dir_all(root.join("doomed")).unwrap();
    fs::write(root.join("doomed/evidence.bin"), vec![9u8; 8192]).unwrap();

    let tree = scan(&root);
    let session = writable(&root);

    // Refused, by name, with the reason and the way forward.
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "doomed/evidence.bin")],
        Action::Trash,
        &session,
    );
    assert!(plan.is_empty(), "the trash was allowed on a share");
    let (path, why) = &plan.rejected()[0];
    assert!(
        why.contains("network location") && why.contains("permanent delete"),
        "{path:?}: {why}"
    );
    assert!(
        root.join("doomed/evidence.bin").exists(),
        "a refused item was removed anyway"
    );

    // And the way forward really does work, which is what makes the refusal
    // an answer rather than a dead end.
    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "doomed/evidence.bin")],
        Action::DeletePermanently,
        &session,
    );
    let report = plan.execute(&session.grant().unwrap(), &mut AuditLog::disabled());
    assert_eq!(report.succeeded, 1, "{report:?}");
    assert!(!root.join("doomed/evidence.bin").exists());

    let _ = fs::remove_dir_all(root.join("doomed"));
}

/// A name Windows will not spell back, and the neighbour it reaches instead.
///
/// **`victim.bin ` is deleted by asking for `victim.bin`.** Win32 drops a
/// trailing space or dot from every path handed to it, so `CreateFileW`,
/// `symlink_metadata`, `canonicalize` and the shell all answer about the file
/// next to the one that was asked for — and answer well, with no error to
/// notice. Nothing downstream could catch it, because everything downstream
/// agreed: the plan resolved a 64 KiB `victim.bin ` to a 1 KiB `victim.bin`,
/// took the fingerprint of *that* file, and so sailed through the check whose
/// whole job is to notice a file that is not what the scan saw. The dry run
/// then said it would trash `victim.bin` and quoted 64 KiB, which is one
/// sentence about two files.
///
/// kosha's own scanner reads both correctly — enumerating a directory returns
/// names instead of asking for them — so this is only ever about acting.
///
/// Both orders are checked, big-then-small and small-then-big, because the
/// first of them is the one where the wrong answer looks most reasonable.
#[cfg(windows)]
#[test]
fn a_name_windows_cannot_ask_for_is_refused_rather_than_swapped() {
    for (plain, spaced) in [(1024usize, 65536usize), (65536, 1024)] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("victim.bin"), vec![0u8; plain]).unwrap();
        // The only way to make one: a verbatim path, which Win32 leaves alone.
        fs::write(
            format!(r"\\?\{}\victim.bin ", dir.path().display()),
            vec![0u8; spaced],
        )
        .unwrap();

        let tree = scan(dir.path());
        let target = tree
            .find(&dir.path().join("victim.bin "))
            .expect("the scanner reads the name it cannot ask for");

        let session = Session::new(dir.path().to_path_buf());
        let plan = ActionPlan::build(&tree, &[target], Action::Trash, &session);

        assert!(
            plan.items().is_empty(),
            "planned an action on a path that names something else"
        );
        let (_, why) = plan.rejected().first().expect("a refusal, with a reason");
        assert!(
            why.contains("Windows cannot ask for") && why.contains("ends in a space"),
            "{why}"
        );

        // And nothing was touched by finding that out.
        let report = plan.dry_run();
        assert_eq!(report.succeeded, 0, "{}", report.summary());
        assert_eq!(
            fs::metadata(dir.path().join("victim.bin")).unwrap().len() as usize,
            plain
        );
    }
}

/// A file named after a device, and the four that are not the problem.
///
/// Windows keeps `CON`, `PRN`, `NUL` and the rest in every directory at once,
/// and the obvious fear is that acting on a file with one of those names acts
/// on the device. Measured, that fear is wrong about nearly all of them:
/// `CON`, `PRN`, `COM1` and `aux.txt` resolve to the real file, fingerprint
/// it, delete it, and leave the file beside it alone. Refusing every device
/// name would take four working things away to explain one broken one.
///
/// `NUL` is the broken one. It resolves to the null device, which answers a
/// request for its metadata with `ERROR_INVALID_FUNCTION`, so the plan
/// refused it — correctly, and by saying `Incorrect function. (os error 1)`
/// to somebody who had just asked kosha to delete a file it had listed for
/// them. Nothing was ever at risk here; the whole defect was the sentence.
///
/// A device name that is simply not there keeps the plain not-found message,
/// because a missing file is not a device story.
#[cfg(windows)]
#[test]
fn a_file_named_after_a_device_is_acted_on_or_told_why_not() {
    for name in ["CON", "PRN", "COM1", "aux.txt"] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bystander.bin"), vec![0u8; 1024]).unwrap();
        fs::write(
            format!(r"\\?\{}\{name}", dir.path().display()),
            vec![0u8; 65536],
        )
        .unwrap();

        let tree = scan(dir.path());
        let id = tree.find(&dir.path().join(name)).expect("in the tree");
        let mut session = Session::new(dir.path().to_path_buf());
        session.enable_read_write(UserConfirmed::from_dialog_acceptance());
        let plan = ActionPlan::build(&tree, &[id], Action::DeletePermanently, &session);
        assert!(plan.rejected().is_empty(), "{name}: {:?}", plan.rejected());

        let grant = session.grant().expect("read-write");
        let report = plan.execute(&grant, &mut AuditLog::disabled());
        assert_eq!(report.succeeded, 1, "{name}: {}", report.summary());
        assert!(!dir.path().join(name).exists(), "{name} is still there");
        assert!(
            dir.path().join("bystander.bin").exists(),
            "{name}: the file beside it went"
        );
    }

    // And the one that cannot be reached, which must say so in words.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        format!(r"\\?\{}\NUL", dir.path().display()),
        vec![0u8; 65536],
    )
    .unwrap();
    let tree = scan(dir.path());
    let id = tree.find(&dir.path().join("NUL")).expect("NUL is scanned");
    let session = Session::new(dir.path().to_path_buf());
    let plan = ActionPlan::build(&tree, &[id], Action::Trash, &session);
    assert!(plan.items().is_empty());
    let (_, why) = plan.rejected().first().expect("a reason");
    assert!(
        why.contains("named after a device") && !why.contains("os error"),
        "{why}"
    );

    // A device name that is not there is a missing file, and says that.
    let why = kosha_fsops::guard::check(&dir.path().join("CON"), dir.path())
        .expect_err("nothing is there")
        .to_string();
    assert!(why.contains("could not be resolved"), "{why}");
}
