// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Files stored in the cloud, inside a plan that would delete them.
//!
//! Every tree here is synthetic: ordinary files in a temporary directory,
//! scanned, and then given the `OFFLINE` flag in the arena — which is exactly
//! how a placeholder reaches this crate, since the scanner turns Windows'
//! attributes and macOS's `SF_DATALESS` into that one flag. No sync folder is
//! touched and no real cloud file is ever deleted.
//!
//! Real execution uses `DeletePermanently` inside the temporary directory, as
//! `execute.rs` does, so a normal `cargo test` deposits nothing in the Trash.
//! `Trash` plans are only dry-run or blocked by the gate here.

use std::fs;
use std::path::{Path, PathBuf};

use kosha_core::{Flags, NameArena, NodeId, Owners, ScanOptions, Scanner, SizeTree};
use kosha_fsops::{
    Action, ActionPlan, AuditLog, CloudConfirmed, CloudEvidence, Outcome, Session, SkipReason,
    UserConfirmed,
};

/// ```text
/// root/
///   photos/
///     a.jpg          cloud-only
///     b.jpg
///     local-only/f.jpg
///     trip/
///       c.jpg        cloud-only
///       d.jpg
///       deep/e.jpg   cloud-only
///   plain/g.txt
///   empty-dirs/x/y/
///   cloud.docx       cloud-only
///   local.txt
/// ```
const CLOUD_ONLY: [&str; 4] = [
    "photos/a.jpg",
    "photos/trip/c.jpg",
    "photos/trip/deep/e.jpg",
    "cloud.docx",
];

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for folder in [
        "photos/local-only",
        "photos/trip/deep",
        "plain",
        "empty-dirs/x/y",
    ] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    for file in [
        "photos/a.jpg",
        "photos/b.jpg",
        "photos/local-only/f.jpg",
        "photos/trip/c.jpg",
        "photos/trip/d.jpg",
        "photos/trip/deep/e.jpg",
        "plain/g.txt",
        "cloud.docx",
        "local.txt",
    ] {
        fs::write(root.join(file), vec![b'x'; 2048]).unwrap();
    }
    dir
}

/// Scan, then flag the cloud-only files the way the scanner would have.
fn synthetic_tree(root: &Path) -> SizeTree {
    let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();
    let base = tree.root_path().to_path_buf();
    let mut nodes = tree.nodes_slice().to_vec();
    for (index, node) in nodes.iter_mut().enumerate() {
        let path = tree.path_of(NodeId::from_index(index));
        let Ok(relative) = path.strip_prefix(&base) else {
            continue;
        };
        if CLOUD_ONLY.iter().any(|c| relative == Path::new(c)) {
            node.flags.insert(Flags::OFFLINE);
        }
    }
    SizeTree::from_parts(
        nodes,
        NameArena::from_bytes(tree.names_bytes().to_vec()),
        Owners::default(),
        tree.root_path().to_path_buf(),
        tree.stats().clone(),
    )
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

fn writable(root: &Path) -> Session {
    let mut session = Session::new(root);
    session.enable_read_write(UserConfirmed::from_dialog_acceptance());
    session
}

/// Paths relative to the scanned root, with `/` separators, sorted.
fn relative(paths: impl IntoIterator<Item = PathBuf>, root: &Path) -> Vec<String> {
    // A plan holds resolved paths, and a temporary directory can sit behind a
    // link (`/var` on macOS) or behind a Windows 8.3 short name (`RUNNER~1` on
    // the hosted runners), so several spellings of the root may prefix them.
    // `canonicalize` on Windows returns a verbatim `\\?\C:\...` path while the
    // plan holds the plain `C:\...` spelling, so that one is tried as well.
    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut bases = vec![canonical.clone(), root.to_path_buf()];
    if let Some(plain) = canonical.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        bases.push(PathBuf::from(plain));
    }
    let mut out: Vec<String> = paths
        .into_iter()
        .map(|p| {
            bases
                .iter()
                .find_map(|base| p.strip_prefix(base).ok())
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    out.sort();
    out
}

fn would_do(report: &kosha_fsops::ExecutionReport) -> Vec<PathBuf> {
    report
        .details
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::WouldDo(_)))
        .map(|(p, _)| p.clone())
        .collect()
}

fn done(report: &kosha_fsops::ExecutionReport) -> Vec<PathBuf> {
    report
        .details
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Done(_)))
        .map(|(p, _)| p.clone())
        .collect()
}

// ------------------------------------------------------------ the warning

/// The test that must go red if a Trash plan for a cloud-only file ever loses
/// its warning, or can run without the token. Remove the risk assignment in
/// `ActionPlan::build` and every assertion below the first fails.
#[test]
fn a_trash_plan_for_a_cloud_only_file_warns_and_will_not_run_unconfirmed() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(&tree, &[node(&tree, "cloud.docx")], Action::Trash, &session);
    let risk = plan
        .cloud_risk()
        .expect("a Trash plan for a cloud-only file carried no cloud risk");
    assert_eq!(risk.offline_files(), 1);
    assert_eq!(risk.items_at_risk(), 1);
    assert_eq!(risk.evidence(), CloudEvidence::OfflinePlaceholder);
    assert!(risk.requires_confirmation());
    assert!(!risk.can_skip_cloud_only(), "skipping would leave nothing");

    let warning = plan.cloud_warning().expect("no warning words");
    assert_eq!(warning.title(), "This file is stored in the cloud");
    assert_eq!(
        warning.body(),
        "It is not kept on this computer. Moving it to the Trash usually deletes \
         the online copy too, and it may be the only copy."
    );
    assert_eq!(
        warning.confirm_label(),
        "Move 1 cloud-only file to the Trash"
    );
    assert_eq!(warning.skip_label(), None);
    assert_eq!(warning.offline_files(), 1);

    // Read-write mode alone is not enough.
    let grant = session.grant().unwrap();
    let report = plan.execute(&grant, &mut AuditLog::disabled());
    assert_eq!(report.succeeded, 0);
    assert_eq!(report.skipped, 1);
    assert!(matches!(
        report.details[0].1,
        Outcome::Skipped(SkipReason::CloudNotConfirmed)
    ));
    assert!(dir.path().join("cloud.docx").exists());
}

#[test]
fn a_folder_counts_every_cloud_only_file_at_any_depth_and_never_a_folder() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = Session::new(dir.path());

    let count = |relative: &str| {
        ActionPlan::build(&tree, &[node(&tree, relative)], Action::Trash, &session)
            .cloud_risk()
            .map(|r| r.offline_files())
    };
    assert_eq!(count("photos"), Some(3));
    assert_eq!(count("photos/trip"), Some(2));
    assert_eq!(count("photos/trip/deep"), Some(1));
    assert_eq!(count("photos/local-only"), None);
    assert_eq!(count("plain"), None);
    assert_eq!(count("empty-dirs"), None, "a folder was counted as a file");
}

#[test]
fn folder_wording_names_the_count_in_plural_and_singular() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = Session::new(dir.path());

    let many = ActionPlan::build(&tree, &[node(&tree, "photos")], Action::Trash, &session)
        .cloud_warning()
        .unwrap();
    assert_eq!(
        many.title(),
        "3 files in this folder are stored in the cloud"
    );
    assert_eq!(
        many.body(),
        "They are not kept on this computer. Moving them to the Trash usually \
         deletes the online copies too, and they may be the only copies."
    );
    assert_eq!(
        many.confirm_label(),
        "Move everything, including 3 cloud-only files"
    );
    assert_eq!(many.skip_label(), Some("Skip cloud-only items"));

    let one = ActionPlan::build(
        &tree,
        &[node(&tree, "photos/trip/deep")],
        Action::Trash,
        &session,
    );
    let warning = one.cloud_warning().unwrap();
    assert_eq!(
        warning.title(),
        "1 file in this folder is stored in the cloud"
    );
    assert_eq!(
        warning.body(),
        "It is not kept on this computer. Moving it to the Trash usually deletes \
         the online copy too, and it may be the only copy."
    );
    assert_eq!(
        warning.confirm_label(),
        "Move everything, including 1 cloud-only file"
    );
    // The folder holds nothing else, so there is nothing to skip to.
    assert_eq!(warning.skip_label(), None);
}

#[test]
fn a_mixed_selection_counts_across_its_items() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = Session::new(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[
            node(&tree, "photos"),
            node(&tree, "local.txt"),
            node(&tree, "cloud.docx"),
            node(&tree, "plain"),
        ],
        Action::Trash,
        &session,
    );
    let risk = plan.cloud_risk().unwrap();
    assert_eq!(risk.offline_files(), 4);
    assert_eq!(risk.items_at_risk(), 2);
    let warning = risk.warning();
    assert_eq!(
        warning.title(),
        "4 files in this selection are stored in the cloud"
    );
    assert_eq!(
        warning.confirm_label(),
        "Move everything, including 4 cloud-only files"
    );
    assert_eq!(warning.skip_label(), Some("Skip cloud-only items"));

    let per_item: Vec<Option<u64>> = plan
        .items()
        .iter()
        .map(|i| i.cloud.map(|c| c.offline_files))
        .collect();
    assert_eq!(per_item, vec![Some(3), None, Some(1), None]);
}

/// Permanent deletion is at least as strict: the same gate, and words that do
/// not mention the Trash.
#[test]
fn a_permanent_delete_is_gated_the_same_way() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "cloud.docx")],
        Action::DeletePermanently,
        &session,
    );
    let warning = plan.cloud_warning().unwrap();
    assert_eq!(
        warning.body(),
        "It is not kept on this computer. Deleting it usually deletes the online \
         copy too, and it may be the only copy."
    );
    assert_eq!(
        warning.confirm_label(),
        "Delete 1 cloud-only file permanently"
    );
    assert!(plan.cloud_risk().unwrap().requires_confirmation());

    let report = plan.execute(&session.grant().unwrap(), &mut AuditLog::disabled());
    assert_eq!(report.succeeded, 0);
    assert!(dir.path().join("cloud.docx").exists());

    let folder = ActionPlan::build(
        &tree,
        &[node(&tree, "photos")],
        Action::DeletePermanently,
        &session,
    );
    assert_eq!(
        folder.cloud_warning().unwrap().confirm_label(),
        "Delete everything, including 3 cloud-only files"
    );
}

#[test]
fn emptying_a_folder_with_cloud_only_files_is_gated_and_not_skippable() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "photos")],
        Action::EmptyContents,
        &session,
    );
    let risk = plan.cloud_risk().unwrap();
    assert_eq!(risk.offline_files(), 3);
    assert!(!risk.can_skip_cloud_only());

    let report = plan.execute(&session.grant().unwrap(), &mut AuditLog::disabled());
    assert_eq!(report.succeeded, 0);
    assert!(dir.path().join("photos/b.jpg").exists());
}

/// Compression removes nothing, so it carries no cloud risk at all.
#[test]
fn compression_carries_no_cloud_risk() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = Session::new(dir.path());
    let plan = ActionPlan::build(&tree, &[node(&tree, "photos")], Action::Compress, &session);
    assert!(plan.cloud_risk().is_none());
    assert!(plan.cloud_warning().is_none());
}

// ---------------------------------------------------------------- the gate

/// With the plan's own token, it runs, and the audit log records the risk and
/// the confirmation on the one line.
#[test]
fn the_plans_own_token_lets_it_run_and_the_log_says_so() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());
    let log_dir = tempfile::tempdir().unwrap();
    let log_path = log_dir.path().join("audit.jsonl");

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "photos/trip"), node(&tree, "local.txt")],
        Action::DeletePermanently,
        &session,
    );
    let preview = plan.dry_run();
    assert_eq!(preview.succeeded, 2, "the preview shows the confirmed path");

    let confirmed = CloudConfirmed::from_dialog_acceptance(&plan.cloud_warning().unwrap());
    let mut audit = AuditLog::at(log_path.clone());
    let report = plan.execute_including_cloud(&session.grant().unwrap(), &confirmed, &mut audit);
    drop(audit);

    assert_eq!(report.succeeded, 2);
    assert_eq!(
        relative(done(&report), dir.path()),
        relative(would_do(&preview), dir.path()),
        "what ran is not what the preview showed"
    );
    assert!(!dir.path().join("photos/trip").exists());
    assert!(!dir.path().join("local.txt").exists());

    let text = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    let trip = lines.iter().find(|l| l.contains("trip")).unwrap();
    assert!(
        trip.ends_with(r#","cloud":{"offline_files":2,"evidence":"offline","confirmed":true}}"#),
        "{trip}"
    );
    let local = lines.iter().find(|l| l.contains("local.txt")).unwrap();
    assert!(
        !local.contains("cloud"),
        "an ordinary item gained a field: {local}"
    );
}

/// Without the token nothing runs — the ordinary items in the plan included,
/// because the user did not accept the plan they were shown.
#[test]
fn without_the_token_nothing_in_the_plan_runs() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "local.txt"), node(&tree, "photos")],
        Action::DeletePermanently,
        &session,
    );
    let report = plan.execute(&session.grant().unwrap(), &mut AuditLog::disabled());
    assert_eq!(report.succeeded, 0);
    assert_eq!(report.skipped, 2);
    assert!(
        report
            .details
            .iter()
            .all(|(_, o)| matches!(o, Outcome::Skipped(SkipReason::CloudNotConfirmed)))
    );
    assert!(dir.path().join("local.txt").exists());
    assert!(dir.path().join("photos/a.jpg").exists());
}

/// A token is bound to the plan whose warning made it.
#[test]
fn a_token_from_another_plan_is_refused() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());

    let shown = ActionPlan::build(
        &tree,
        &[node(&tree, "photos")],
        Action::DeletePermanently,
        &session,
    );
    let other = ActionPlan::build(
        &tree,
        &[node(&tree, "photos")],
        Action::DeletePermanently,
        &session,
    );
    let token = CloudConfirmed::from_dialog_acceptance(&shown.cloud_warning().unwrap());
    let report =
        other.execute_including_cloud(&session.grant().unwrap(), &token, &mut AuditLog::disabled());
    assert_eq!(report.succeeded, 0);
    assert!(dir.path().join("photos/a.jpg").exists());
}

// --------------------------------------------------------------- skipping

/// Skipping rebuilds the plan, and what then runs is exactly what its preview
/// listed: the ordinary files, never a cloud-only one, and never a folder that
/// holds one.
#[test]
fn skipping_cloud_only_items_runs_exactly_what_the_preview_showed() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "photos"), node(&tree, "cloud.docx")],
        Action::DeletePermanently,
        &session,
    );
    assert!(plan.cloud_risk().unwrap().can_skip_cloud_only());

    let skipped = plan.without_cloud_only(&tree, &session);
    assert!(
        skipped.cloud_risk().is_none(),
        "the rebuilt plan still holds cloud-only files"
    );
    let preview = skipped.dry_run();
    assert_eq!(
        relative(would_do(&preview), dir.path()),
        vec!["photos/b.jpg", "photos/local-only", "photos/trip/d.jpg"]
    );

    // No cloud token: the rebuilt plan needs none.
    let report = skipped.execute(&session.grant().unwrap(), &mut AuditLog::disabled());
    assert_eq!(
        relative(done(&report), dir.path()),
        relative(would_do(&preview), dir.path())
    );
    assert_eq!(report.skipped, 0);

    for gone in ["photos/b.jpg", "photos/local-only", "photos/trip/d.jpg"] {
        assert!(!dir.path().join(gone).exists(), "{gone} survived");
    }
    for kept in CLOUD_ONLY {
        assert!(dir.path().join(kept).exists(), "{kept} was deleted");
    }
}

#[test]
fn a_trash_skip_preview_lists_only_ordinary_files() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = Session::new(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "photos/trip")],
        Action::Trash,
        &session,
    );
    let skipped = plan.without_cloud_only(&tree, &session);
    assert!(skipped.cloud_risk().is_none());
    assert_eq!(skipped.action(), Some(Action::Trash));
    assert_eq!(
        relative(would_do(&skipped.dry_run()), dir.path()),
        vec!["photos/trip/d.jpg"]
    );
}

#[test]
fn skipping_against_a_different_tree_plans_nothing_and_says_why() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = Session::new(dir.path());
    let plan = ActionPlan::build(&tree, &[node(&tree, "photos")], Action::Trash, &session);

    fs::write(dir.path().join("new.txt"), b"n").unwrap();
    let rescanned = synthetic_tree(dir.path());
    let skipped = plan.without_cloud_only(&rescanned, &session);
    assert!(skipped.is_empty());
    assert_eq!(skipped.rejected().len(), 1);
}

// ------------------------------------------------------ ordinary and read-only

/// An ordinary folder sees nothing new: no risk, no words, and the plain
/// `execute` works as it always did.
#[test]
fn an_ordinary_tree_gets_no_warning_and_no_friction() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let session = writable(dir.path());

    let plan = ActionPlan::build(
        &tree,
        &[node(&tree, "plain"), node(&tree, "photos/local-only")],
        Action::DeletePermanently,
        &session,
    );
    assert!(plan.cloud_risk().is_none());
    assert!(plan.cloud_warning().is_none());
    assert!(plan.items().iter().all(|i| i.cloud.is_none()));

    let report = plan.execute(&session.grant().unwrap(), &mut AuditLog::disabled());
    assert_eq!(report.succeeded, 2);
    assert!(!dir.path().join("plain").exists());
}

/// Read-only mode refuses everything; a cloud token is not a grant.
#[test]
fn read_only_mode_still_refuses_everything() {
    let dir = fixture();
    let tree = synthetic_tree(dir.path());
    let mut session = Session::new(dir.path());

    let plan = ActionPlan::build(&tree, &[node(&tree, "photos")], Action::Trash, &session);
    let _token = CloudConfirmed::from_dialog_acceptance(&plan.cloud_warning().unwrap());
    assert!(session.grant().is_err(), "a read-only session gave a grant");
    assert_eq!(plan.dry_run().succeeded, 1, "previewing needs no grant");

    session.enable_read_write(UserConfirmed::from_dialog_acceptance());
    session.return_to_read_only();
    assert!(session.grant().is_err());
    assert!(dir.path().join("photos/a.jpg").exists());
}
