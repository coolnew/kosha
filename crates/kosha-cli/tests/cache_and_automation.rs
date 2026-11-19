// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Agent-friendly streams, explicit historical reads, and recipe overrides.

mod common;
use kosha_core::{ScanOptions, Scanner, snapshot};
use std::{
    path::Path,
    process::{Command, Output},
    time::UNIX_EPOCH,
};

fn run(args: &[&str], home: &Path) -> Output {
    Command::new(common::kosha())
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("roaming"))
        .env("LOCALAPPDATA", home.join("local"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .output()
        .unwrap()
}
fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
fn data_dir(home: &Path) -> std::path::PathBuf {
    if cfg!(windows) {
        home.join("local/coolnew/kosha")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/app.coolnew.kosha")
    } else {
        home.join("data/kosha")
    }
}
fn config_dir(home: &Path) -> std::path::PathBuf {
    if cfg!(windows) {
        home.join("roaming/coolnew/kosha")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/app.coolnew.kosha")
    } else {
        home.join("config/kosha")
    }
}

#[test]
fn cache_inventory_lists_shared_and_custom_records_without_reading_the_source() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("offline-drive");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("remembered.md"), b"recorded").unwrap();
    let tree = Scanner::new(ScanOptions::default()).scan(&source).unwrap();
    let dir = data_dir(home.path()).join("drive-cache");
    let record = snapshot::store::save_automatic_in(
        &dir,
        &tree,
        UNIX_EPOCH + std::time::Duration::from_secs(1000),
    )
    .unwrap();
    let before = std::fs::read(&record).unwrap();
    std::fs::write(dir.join("drive-0000000000000000.kosha"), b"broken").unwrap();
    std::fs::write(dir.join("pending.writing"), b"staged").unwrap();
    snapshot::store::save_in(
        &data_dir(home.path()).join("snapshots"),
        &tree,
        "Explicit",
        UNIX_EPOCH,
    )
    .unwrap();
    std::fs::remove_dir_all(&source).unwrap();

    let output = run(&["--caches", "--export", "json"], home.path());
    assert!(output.status.success(), "{}", err(&output));
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["schema"], "kosha.cache.list.v1");
    assert_eq!(doc["header_only"], true);
    let rows = doc["records"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let good = rows.iter().find(|r| r["status"] == "recorded").unwrap();
    assert_eq!(good["root"], tree.root_path().to_string_lossy().as_ref());
    assert_eq!(good["taken_unix"], 1000);
    assert_eq!(good["incomplete"], false);
    assert!(rows.iter().any(|r| r["status"] == "unreadable"));
    assert!(err(&output).is_empty());
    let output = run(
        &[
            "--caches",
            "--cache-dir",
            dir.to_str().unwrap(),
            "--export",
            "jsonl",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", err(&output));
    assert_eq!(
        out(&output)
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .collect::<Vec<_>>()
            .len(),
        2
    );
    let output = run(&["--caches"], home.path());
    assert!(output.status.success());
    assert!(out(&output).contains("Headers only") && out(&output).contains("offline-drive"));
    assert_eq!(std::fs::read(&record).unwrap(), before);
    assert!(!source.exists());
}

#[test]
fn absent_cache_inventory_is_empty_and_does_not_create_directories() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("missing-cache");
    let output = run(
        &[
            "--caches",
            "--cache-dir",
            dir.to_str().unwrap(),
            "--export",
            "json",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", err(&output));
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["records"].as_array().unwrap().len(), 0);
    assert!(!dir.exists());
    let reordered = run(
        &[
            "--cache-dir",
            dir.to_str().unwrap(),
            "--export",
            "json",
            "--caches",
        ],
        home.path(),
    );
    assert!(reordered.status.success(), "{}", err(&reordered));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&reordered.stdout).unwrap()["records"],
        doc["records"]
    );
    for args in [
        vec!["--caches", "--cache"],
        vec!["--caches", "--export", "csv"],
        vec!["--caches", "--cache-dir"],
    ] {
        assert!(!run(&args, home.path()).status.success(), "{args:?}");
    }
}

#[test]
fn saved_snapshot_names_work_for_browsing_search_and_comparison_with_local_precedence() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("stored-only.md"), b"saved").unwrap();
    let tree = Scanner::new(ScanOptions::default()).scan(&source).unwrap();
    let stored = snapshot::store::save_in(
        &data_dir(home.path()).join("snapshots"),
        &tree,
        "Stored",
        UNIX_EPOCH,
    )
    .unwrap();
    let name = stored.file_name().unwrap().to_str().unwrap();
    let baseline = run(&[name, "--since", name, "--since-min", "0"], home.path());
    assert!(baseline.status.success(), "{}", err(&baseline));
    for args in [
        vec![name, "--export", "txt"],
        vec!["search", "--load", name, "files", "--export", "txt"],
    ] {
        let output = run(&args, home.path());
        assert!(output.status.success(), "{}", err(&output));
        assert!(out(&output).contains("stored-only.md"));
        assert!(err(&output).contains("source: snapshot; taken_unix=0"));
    }
    let explicit_local = format!("./{name}");
    assert!(
        !run(&[&explicit_local], home.path()).status.success(),
        "explicit missing local path must not fall back"
    );
    std::fs::remove_file(source.join("stored-only.md")).unwrap();
    std::fs::write(source.join("cwd-only.md"), b"local").unwrap();
    let local = Scanner::new(ScanOptions::default()).scan(&source).unwrap();
    let saved_local =
        snapshot::store::save_in(&home.path().join("stage"), &local, "Local", UNIX_EPOCH).unwrap();
    let local_path = home.path().join(name);
    std::fs::copy(saved_local, &local_path).unwrap();
    for args in [
        vec![name, "--export", "txt"],
        vec!["search", "--load", name, "files", "--export", "txt"],
    ] {
        let output = run(&args, home.path());
        assert!(output.status.success(), "{}", err(&output));
        assert!(out(&output).contains("cwd-only.md") && !out(&output).contains("stored-only.md"));
    }
    let absolute = run(&[stored.to_str().unwrap(), "--export", "txt"], home.path());
    assert!(absolute.status.success());
    assert!(out(&absolute).contains("stored-only.md"));
    std::fs::write(local_path, b"corrupt local file").unwrap();
    for args in [vec![name], vec!["search", "--load", name, "files"]] {
        let output = run(&args, home.path());
        assert!(!output.status.success());
        assert!(!out(&output).contains("stored-only.md"));
    }
}

#[test]
fn fresh_scan_ignores_shared_cache_and_explicit_cache_reads_the_old_record() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("old.md"), b"old").unwrap();
    let tree = Scanner::new(ScanOptions::default()).scan(&source).unwrap();
    let root = tree.root_path().to_string_lossy().into_owned();
    let cache = data_dir(home.path()).join("drive-cache");
    snapshot::store::save_automatic_in(
        &cache,
        &tree,
        UNIX_EPOCH + std::time::Duration::from_secs(1000),
    )
    .unwrap();
    std::fs::remove_file(source.join("old.md")).unwrap();
    std::fs::write(source.join("new.md"), b"new").unwrap();
    let fresh = run(&[&root, "--export", "txt"], home.path());
    assert!(fresh.status.success(), "{}", err(&fresh));
    assert!(out(&fresh).contains("new.md") && !out(&fresh).contains("old.md"));
    assert!(!err(&fresh).contains("source: cache"));
    for args in [
        vec![root.as_str(), "--cache", "--export", "txt"],
        vec![
            "search",
            "--path",
            root.as_str(),
            "--cache",
            "--export",
            "txt",
            "files",
        ],
    ] {
        let cached = run(&args, home.path());
        assert!(cached.status.success(), "{}", err(&cached));
        assert!(
            out(&cached).contains("old.md") && !out(&cached).contains("new.md"),
            "{}",
            out(&cached)
        );
        assert!(!out(&cached).contains("SNAPSHOT"));
        assert!(err(&cached).contains("source: cache; taken_unix=1000; age_seconds="));
    }
    let saved = snapshot::store::read_meta_at(&snapshot::store::automatic_path_in(
        &cache,
        tree.root_path(),
    ))
    .unwrap();
    assert_eq!(saved.taken, 1000, "fresh scan must not refresh cache");
}

#[test]
fn custom_cache_miss_and_invalid_cache_combinations_never_fall_back_to_scan() {
    let home = tempfile::tempdir().unwrap();
    let cache = home.path().join("custom");
    let cache_text = cache.to_string_lossy();
    let source = home.path().join("missing");
    let source_text = source.to_string_lossy();
    let missing = run(
        &[&source_text, "--cache", "--cache-dir", &cache_text],
        home.path(),
    );
    assert!(!missing.status.success());
    assert!(
        err(&missing).contains("could not use cached scan")
            && err(&missing).contains("omit --cache")
    );
    for args in [
        vec![".", "--cache-dir", &cache_text],
        vec![".", "--cache", "--depth", "2"],
        vec![".", "--cache", "--duplicates"],
        vec![".", "--cache", "--snapshot"],
        vec!["search", "--describe", "--cache", "files"],
        vec!["search", "--load", "missing.kosha", "--cache", "files"],
        vec!["search", "--path", ".", "--cache-dir", &cache_text, "files"],
    ] {
        let output = run(&args, home.path());
        assert!(!output.status.success(), "{args:?}");
        assert!(
            !err(&output).contains("could not use cached scan"),
            "should fail in parsing: {}",
            err(&output)
        );
    }
    assert!(!cache.exists());
}

#[test]
fn custom_cache_directory_can_read_without_an_existing_source() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("gone");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("remembered.md"), b"x").unwrap();
    let tree = Scanner::new(ScanOptions::default()).scan(&source).unwrap();
    let cache = home.path().join("custom");
    snapshot::store::save_automatic_in(&cache, &tree, UNIX_EPOCH).unwrap();
    std::fs::remove_file(source.join("remembered.md")).unwrap();
    std::fs::remove_dir(&source).unwrap();
    let root = tree.root_path().to_string_lossy();
    for args in [
        vec![
            root.as_ref(),
            "--cache",
            "--cache-dir",
            cache.to_str().unwrap(),
            "--export",
            "csv",
        ],
        vec![
            "search",
            "--path",
            root.as_ref(),
            "--cache",
            "--cache-dir",
            cache.to_str().unwrap(),
            "--export",
            "csv",
            "files",
        ],
    ] {
        let output = run(&args, home.path());
        assert!(output.status.success(), "{}", err(&output));
        assert!(out(&output).starts_with("path,") && out(&output).contains("remembered.md"));
    }
}

#[test]
fn json_description_is_parseable_and_recipes_resolve_before_source_io() {
    let home = tempfile::tempdir().unwrap();
    let config = config_dir(home.path());
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("search-recipes.json"),
        r#"[{"phrase":"my documents","dsl":"file and ext = md"}]"#,
    )
    .unwrap();
    let output = run(
        &["search", "--describe", "--json", "MY \u{20}\u{20}documents"],
        home.path(),
    );
    assert!(output.status.success(), "{}", err(&output));
    let object: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(object["schema"], "kosha.search.describe.v1");
    assert_eq!(object["dsl"], "file and ext = md");
    assert_eq!(object["source_opened"], false);
    assert_eq!(object["entities"][0]["kind"], "Recipe");
    let explicit = home.path().join("override.json");
    std::fs::write(
        &explicit,
        r#"[{"phrase":"my documents","dsl":"file and ext = txt"}]"#,
    )
    .unwrap();
    let output = run(
        &[
            "search",
            "--recipes",
            explicit.to_str().unwrap(),
            "--describe",
            "--json",
            "my documents",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", err(&output));
    let object: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(object["dsl"], "file and ext = txt");
    std::fs::write(&explicit, "bad json").unwrap();
    let output = run(
        &[
            "search",
            "--recipes",
            explicit.to_str().unwrap(),
            "--path",
            "nonexistent",
            "files",
        ],
        home.path(),
    );
    assert!(!output.status.success());
    assert!(err(&output).contains("invalid recipes") && !err(&output).contains("No such file"));
}

#[test]
fn absolute_machine_exports_include_all_search_matches() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("source");
    std::fs::create_dir(&source).unwrap();
    for name in ["a.md", "b.md"] {
        std::fs::write(source.join(name), b"x").unwrap();
    }
    for format in ["jsonl", "csv", "json", "txt"] {
        let output = run(
            &[
                "search",
                "--path",
                source.to_str().unwrap(),
                "--top",
                "1",
                "--absolute-paths",
                "--export",
                format,
                "files ending with md",
            ],
            home.path(),
        );
        assert!(output.status.success(), "{}", err(&output));
        let text = out(&output);
        assert!(text.contains("a.md") && text.contains("b.md"), "{text}");
        assert!(!text.contains("SEARCH") && !text.contains("SCAN"));
        if format == "jsonl" {
            let rows: Vec<serde_json::Value> = text
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            assert_eq!(rows.len(), 2);
            for row in rows {
                assert!(Path::new(row["path"].as_str().unwrap()).is_absolute());
            }
        }
        if format == "txt" {
            assert_eq!(text.lines().count(), 2);
            assert!(text.lines().all(|l| Path::new(l).is_absolute()));
        }
    }
}

#[test]
fn snapshot_headers_and_save_reports_do_not_contaminate_machine_stdout() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("a.md"), b"x").unwrap();
    let tree = Scanner::new(ScanOptions::default()).scan(&source).unwrap();
    let saved =
        snapshot::store::save_in(&home.path().join("saved"), &tree, "fixture", UNIX_EPOCH).unwrap();
    let output = run(&[saved.to_str().unwrap(), "--export", "json"], home.path());
    assert!(output.status.success(), "{}", err(&output));
    let object: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(object["entries"].is_array());
    let output = run(
        &[source.to_str().unwrap(), "--snapshot", "--export", "json"],
        home.path(),
    );
    assert!(output.status.success(), "{}", err(&output));
    let object: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(object["entries"].is_array());
    assert!(err(&output).contains("snapshot kept at"));
}

#[test]
fn every_export_warns_about_incomplete_and_access_limited_sources_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("a.md"), b"x").unwrap();
    let tree = Scanner::new(ScanOptions::default()).scan(&source).unwrap();
    for (cancelled, depth_limited, unreadable) in [
        (false, false, 2),
        (true, false, 0),
        (false, true, 0),
        (true, true, 3),
    ] {
        let mut stats = tree.stats().clone();
        stats.partial = true;
        stats.cancelled = cancelled;
        stats.depth_limited = depth_limited;
        stats.unreadable = unreadable;
        let record = kosha_core::SizeTree::from_parts(
            tree.nodes_slice().to_vec(),
            kosha_core::NameArena::from_bytes(tree.names_bytes().to_vec()),
            kosha_core::Owners::default(),
            tree.root_path().to_path_buf(),
            stats,
        );
        let saved =
            snapshot::store::save_in(&home.path().join("saved"), &record, "fixture", UNIX_EPOCH)
                .unwrap();
        for format in ["json", "csv", "jsonl", "txt"] {
            let output = run(&[saved.to_str().unwrap(), "--export", format], home.path());
            assert_eq!(output.status.success(), !cancelled, "{}", err(&output));
            let stderr = err(&output);
            let expected = if cancelled || depth_limited {
                "source is incomplete"
            } else {
                "source is partial due to inaccessible"
            };
            assert!(
                stderr.contains(expected) && stderr.contains("lower bounds"),
                "{stderr}"
            );
            assert!(
                stderr.contains(&format!("unreadable_entries={unreadable}")),
                "{stderr}"
            );
            assert!(!out(&output).contains("SNAPSHOT") && !out(&output).contains("warning:"));
            if format == "json" {
                let object: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(object["incomplete"], cancelled || depth_limited);
                assert_eq!(object["access_limited"], unreadable > 0);
                assert_eq!(object["partial"], true);
                assert_eq!(object["lower_bound"], true);
            }
        }
    }
}
