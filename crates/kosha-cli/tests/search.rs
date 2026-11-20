// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Natural search as a real command: interpretation never needs a source,
//! saved records never trigger a scan, and execution uses complete matches.

mod common;
use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree, format, snapshot};
use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, Output},
    time::{Duration, UNIX_EPOCH},
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
        .expect("kosha search runs")
}
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("large.md"), vec![b'x'; 1024]).unwrap();
    std::fs::write(temp.path().join("small.md"), b"x").unwrap();
    std::fs::write(temp.path().join("note.txt"), b"a short note").unwrap();
    temp
}

#[test]
fn describe_translates_entities_without_scanning_or_creating_files() {
    let home = tempfile::tempdir().unwrap();
    let output = run(
        &[
            "search",
            "--describe",
            "files with apparent size greater than ten megabytes",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("DSL: file and apparent > 10mib"), "{text}");
    assert!(text.contains("ENTITIES") && text.contains("Size ["));
    assert!(!text.contains("SCAN"));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    let output = run(
        &[
            "search",
            "--describe",
            "files in \"this-path-does-not-exist\"",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn invalid_intent_and_conflicting_options_are_refused_before_source_io() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["search", "--load", "missing.kosha", "files greater than"],
        vec!["search", "--describe", "--load", "missing.kosha", "files"],
        vec![
            "search",
            "--load",
            "missing.kosha",
            "--path",
            "missing-dir",
            "files",
        ],
        vec!["search", "--path", "missing-dir", "--query", "file"],
        vec![
            "search",
            "--path",
            "missing-dir",
            "--out",
            "untouched.csv",
            "files",
        ],
        vec!["search", "--path", "missing-dir", "--top", "nope", "files"],
        vec![
            "search",
            "--describe",
            "--export",
            "csv",
            "--out",
            "untouched.csv",
            "files",
        ],
    ] {
        let output = run(&args, home.path());
        assert!(!output.status.success(), "{args:?}");
        let text = stderr(&output);
        assert!(
            !text.contains("could not load")
                && !text.contains("No such file")
                && !text.contains("cannot find the file"),
            "{args:?}: {text}"
        );
        assert!(!home.path().join("untouched.csv").exists());
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }
    let missing_source = run(&["search", "files ending with md"], home.path());
    assert!(!missing_source.status.success());
    assert!(stderr(&missing_source).contains("--describe to inspect without a source"));
    let bad_describe = run(&["search", "--describe", "files greater than"], home.path());
    assert!(!bad_describe.status.success());
    assert!(stderr(&bad_describe).contains("bytes "));
    assert!(!stdout(&bad_describe).contains("DSL:"));
}

#[test]
fn search_scans_only_the_explicit_path_and_top_keeps_complete_counts() {
    let source = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = source.path().to_string_lossy();
    let output = run(
        &[
            "search",
            "--path",
            &path,
            "--top",
            "1",
            "files ending with md",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("2 matched") && text.contains("showing 1 of 2"),
        "{text}"
    );
    let output = run(
        &[
            "search",
            "--path",
            &path,
            "files ending with md apparent size greater than ten bytes",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("1 matched") && text.contains("large.md"),
        "{text}"
    );
    assert!(!text.contains("small.md"));
}

#[test]
fn export_contains_all_matches_and_no_human_report_even_with_top_one() {
    let source = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = source.path().to_string_lossy();
    let output = run(
        &[
            "search",
            "--path",
            &path,
            "--top",
            "1",
            "--stats",
            "--export",
            "csv",
            "files ending with md",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("path,"), "{text}");
    assert!(text.contains("large.md") && text.contains("small.md"));
    assert!(!text.contains("SEARCH") && !text.contains("SCAN"));
    assert!(stderr(&output).contains("exported 2 entries"));
    let out = home.path().join("results.json");
    let out_text = out.to_string_lossy();
    let output = run(
        &[
            "search",
            "--path",
            &path,
            "--export",
            "json",
            "--out",
            &out_text,
            "files ending with md",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = std::fs::read_to_string(out).unwrap();
    assert!(text.contains("large.md") && text.contains("small.md") && !text.contains("note.txt"));
}

fn saved_source(home: &Path, known_births: bool) -> std::path::PathBuf {
    saved_source_flags(home, known_births, false, false)
}
fn saved_source_flags(
    home: &Path,
    known_births: bool,
    partial: bool,
    stopped: bool,
) -> std::path::PathBuf {
    let now = format::now_unix();
    let local = format::local_date(now).unwrap();
    let today = format::days_from_civil(i64::from(local[0]), local[1] as u32, local[2] as u32);
    let previous_tuesday = today - (today + 3).rem_euclid(7) - 7 + 1;
    let (year, month, day) = format::civil_from_days(previous_tuesday);
    let old = format::local_midnight(year, month, day).unwrap() + 12 * 3600;
    let mut names = NameArena::default();
    let nodes = ["record", "old.md", "new.md", "unknown.md"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let (name_off, name_len) = names.push(OsStr::new(name));
            Node {
                logical: if i == 0 { 3 } else { 1 },
                allocated: if i == 0 { 3 } else { 1 },
                parent: 0,
                first_child: if i == 0 { 1 } else { 0 },
                child_count: if i == 0 { 3 } else { 0 },
                subtree_files: if i == 0 { 3 } else { 0 },
                name_off,
                name_len,
                mtime: if i == 2 { now } else { old as u32 },
                flags: if i == 0 { Flags::DIR } else { Flags::default() },
                depth: u8::from(i != 0),
            }
        })
        .collect();
    let tree = SizeTree::from_parts(
        nodes,
        names,
        Owners::default(),
        home.join("deleted-source"),
        ScanStats {
            partial,
            cancelled: stopped,
            entries: 4,
            dirs: 1,
            files: 3,
            ..Default::default()
        },
    )
    .with_creation_times(if known_births {
        vec![u64::MAX, old, u64::from(now), u64::MAX]
    } else {
        vec![]
    });
    let meta = snapshot::Meta::of(
        &tree,
        UNIX_EPOCH + Duration::from_secs(u64::from(now)),
        "search fixture",
    );
    let path = home.join("saved.kosha");
    snapshot::write(&tree, &meta, std::fs::File::create(&path).unwrap()).unwrap();
    path
}

#[test]
fn load_searches_recorded_birth_dates_without_touching_the_missing_root() {
    let home = tempfile::tempdir().unwrap();
    let saved = saved_source(home.path(), true);
    let path = saved.to_string_lossy();
    let output = run(
        &[
            "search",
            "--load",
            &path,
            "folders or files created last week",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("SNAPSHOT") && text.contains("1 matched") && text.contains("old.md"),
        "{text}"
    );
    assert!(!text.contains("new.md") && !text.contains("unknown.md"));
    assert!(!home.path().join("deleted-source").exists());
    assert!(
        stderr(&output).contains("creation timestamps are known for 2 of 3 entries"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn missing_birth_metadata_is_reported_and_never_replaced_by_mtime() {
    let home = tempfile::tempdir().unwrap();
    let saved = saved_source(home.path(), false);
    let path = saved.to_string_lossy();
    let output = run(
        &["search", "--load", &path, "files created last week"],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("0 matched"));
    assert!(
        stderr(&output).contains("creation timestamps are unavailable (0 of 3 entries recorded)")
    );
    assert!(stderr(&output).contains("Modification times are never substituted"));
}

#[test]
fn subcommand_help_has_no_source_requirement_or_side_effects() {
    let home = tempfile::tempdir().unwrap();
    let output = run(&["search", "--help"], home.path());
    assert!(output.status.success());
    assert!(stdout(&output).contains("--describe") && stdout(&output).contains("--load"));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn source_warnings_distinguish_partial_permissions_from_stopped_scans() {
    for (stopped, expected, absent) in [
        (
            false,
            "source is partial due to inaccessible or unreadable entries",
            "source is incomplete",
        ),
        (
            true,
            "source is incomplete because its scan was stopped or depth-limited",
            "source is partial due",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let saved = saved_source_flags(home.path(), true, true, stopped);
        let path = saved.to_string_lossy();
        let output = run(&["search", "--load", &path, "files"], home.path());
        assert!(output.status.success(), "{}", stderr(&output));
        let text = stderr(&output);
        assert!(text.contains(expected) && !text.contains(absent), "{text}");
        if stopped {
            assert!(stdout(&output).contains("incomplete — the scan was stopped"));
        } else {
            assert!(stdout(&output).contains("partial — the scan could not read everything"));
            assert!(!stdout(&output).contains("incomplete"));
        }
    }
}

#[test]
fn describe_keeps_date_boundaries_inside_negation_labeled() {
    let home = tempfile::tempdir().unwrap();
    let output = run(
        &["search", "--describe", "files not created last week"],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("boundary (inside NOT)"));
}

#[cfg(windows)]
#[test]
fn freshly_scanned_files_keep_their_recorded_creation_dates() {
    let source = fixture();
    let home = tempfile::tempdir().unwrap();
    let path = source.path().to_string_lossy();
    let output = run(
        &[
            "search",
            "--path",
            &path,
            "files created in last twenty-four hours",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("3 matched"), "{}", stdout(&output));
    assert!(!stderr(&output).contains("creation timestamps are known for 0"));
    let today = run(
        &["search", "--path", &path, "files created today"],
        home.path(),
    );
    assert!(today.status.success(), "{}", stderr(&today));
    assert!(stdout(&today).contains("3 matched"), "{}", stdout(&today));
}

#[test]
fn recipe_discovery_tracks_the_platform_without_opening_a_source() {
    let home = tempfile::tempdir().unwrap();
    let output = run(&["search", "--list-recipes"], home.path());
    assert!(output.status.success(), "{}", stderr(&output));
    let listing = stdout(&output);
    if cfg!(target_os = "linux") {
        assert!(listing.contains("codex sessions") && listing.contains("cmake build files"));
    } else {
        assert!(listing.contains("No platform-specific recipes"));
        assert!(!listing.contains("codex sessions"));
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    let output = run(&["search", "--list-recipes", "--path", "."], home.path());
    assert!(!output.status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn builtin_recipes_match_metadata_within_the_selected_root() {
    let home = tempfile::tempdir().unwrap();
    for path in [
        ".codex/sessions/2026/rollout.jsonl",
        ".codex/archived_sessions/old.jsonl",
        ".codex/sessions/2026/unrelated.txt",
        ".codex/config.toml",
        "some.codex/sessions/unrelated.jsonl",
        "project/out/CMakeCache.txt",
        "project/out/CMakeFiles/tool.o",
        "project/build/source.c",
        ".lmstudio/models/vendor/model/weight.gguf",
    ] {
        let path = home.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture").unwrap();
    }
    for (root, phrase, expected) in [
        (
            ".codex",
            "CoDeX  SeSSions",
            &["archived_sessions/old.jsonl", "sessions/2026/rollout.jsonl"][..],
        ),
        (
            "project",
            "cmake build files",
            &["out/CMakeCache.txt", "out/CMakeFiles/tool.o"],
        ),
        (
            ".lmstudio/models",
            "lm studio models",
            &["vendor/model/weight.gguf"],
        ),
    ] {
        let output = run(
            &["search", "--path", root, "--export", "jsonl", phrase],
            home.path(),
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let mut paths: Vec<String> = stdout(&output)
            .lines()
            .map(|row| {
                serde_json::from_str::<serde_json::Value>(row).unwrap()["path"]
                    .as_str()
                    .unwrap()
                    .into()
            })
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, expected, "{phrase}");
    }
    let override_file = home.path().join("override.json");
    std::fs::write(
        &override_file,
        r#"[{"phrase":"codex sessions","os":"linux","dsl":"file and ext = txt"}]"#,
    )
    .unwrap();
    let output = run(
        &[
            "search",
            "--describe",
            "--recipes",
            "override.json",
            "codex sessions",
        ],
        home.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("DSL: file and ext = txt"));
}

#[test]
fn local_pack_management_preserves_personal_recipes_and_uses_id_order() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let config = if cfg!(windows) {
        root.join("roaming/coolnew/kosha")
    } else if cfg!(target_os = "macos") {
        root.join("Library/Application Support/app.coolnew.kosha")
    } else {
        root.join("config/kosha")
    };
    std::fs::create_dir_all(&config).unwrap();
    let personal = config.join("search-recipes.json");
    let personal_json = r#"[{"phrase":"workflow alias","dsl":"file and ext = pdf"}]"#;
    std::fs::write(&personal, personal_json).unwrap();

    for (id, extension) in [("z-local", "md"), ("a-local", "txt")] {
        let pack = serde_json::json!({
            "schema": 1,
            "kind": "recipe-pack",
            "metadata": {"id": id, "title": id, "description": "User authored files", "version": "1.0"},
            "recipes": [
                {"phrase": "workflow alias", "dsl": format!("file and ext = {extension}")},
                {"phrase": "review alias", "dsl": format!("file and ext = {extension}")}
            ]
        });
        let name = format!("{id}.json");
        std::fs::write(root.join(&name), pack.to_string()).unwrap();
        let output = run(&["marketplace", "install", &name], root);
        assert!(output.status.success(), "{}", stderr(&output));
        let duplicate = run(&["marketplace", "install", &name], root);
        assert!(!duplicate.status.success());
        assert!(stderr(&duplicate).contains("already installed"));
    }
    std::fs::write(root.join("broken.json"), "{").unwrap();
    assert!(
        !run(&["marketplace", "install", "broken.json"], root)
            .status
            .success()
    );
    assert!(
        !run(&["marketplace", "uninstall", "../search-recipes"], root)
            .status
            .success()
    );

    let listing = run(&["marketplace", "list"], root);
    assert!(listing.status.success(), "{}", stderr(&listing));
    let text = stdout(&listing);
    assert_eq!(text.lines().count(), 2);
    assert!(text.starts_with("a-local  "));

    let describe = |phrase: &str| {
        let output = run(&["search", "--describe", "--json", phrase], root);
        assert!(output.status.success(), "{}", stderr(&output));
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    assert_eq!(describe("workflow alias")["dsl"], "file and ext = pdf");
    assert_eq!(describe("review alias")["dsl"], "file and ext = txt");
    assert!(
        run(&["marketplace", "uninstall", "a-local"], root)
            .status
            .success()
    );
    assert_eq!(describe("review alias")["dsl"], "file and ext = md");
    assert_eq!(std::fs::read_to_string(personal).unwrap(), personal_json);
    assert!(root.join("a-local.json").is_file());
}

#[test]
fn insight_validation_and_kind_qualified_removal_preserve_recipe_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let metadata = serde_json::json!({"publisher":"tests","id":"sessions","title":"Session tools","description":"Synthetic metadata","version":"1.0.0"});
    let recipe = serde_json::json!({"schema":1,"kind":"recipe-pack","metadata":metadata,"recipes":[{"phrase":"session documents","dsl":"file and ext = md"}]}).to_string();
    let insight = serde_json::json!({"schema":1,"kind":"insight-pack","metadata":metadata,"requires":["insight.locations.v1"],"platforms":[],
        "insights":[{"id":"sessions","title":"Review sessions","detail":"Keep valuable history","subject":"directory","location":{"base":"home","path":".assistant/sessions"},"action":"review-only"}]}).to_string();
    std::fs::write(root.join("recipe.json"), &recipe).unwrap();
    std::fs::write(root.join("insight.json"), &insight).unwrap();
    let before = std::fs::read_dir(root).unwrap().count();
    let validated = run(&["marketplace", "validate", "insight.json"], root);
    assert!(validated.status.success(), "{}", stderr(&validated));
    assert_eq!(
        std::fs::read_dir(root).unwrap().count(),
        before,
        "validation wrote configuration"
    );
    for source in ["recipe.json", "insight.json"] {
        let installed = run(&["marketplace", "install", source], root);
        assert!(installed.status.success(), "{}", stderr(&installed));
    }
    let listing = run(&["marketplace", "list"], root);
    assert!(listing.status.success());
    assert!(stdout(&listing).contains("recipe-pack:tests/sessions"));
    assert!(stdout(&listing).contains("insight-pack:tests/sessions"));
    assert!(
        !run(&["marketplace", "uninstall", "tests/sessions"], root)
            .status
            .success()
    );
    assert!(
        run(
            &["marketplace", "uninstall", "insight-pack:tests/sessions"],
            root
        )
        .status
        .success()
    );
    let describe = run(
        &["search", "--describe", "--json", "session documents"],
        root,
    );
    assert!(describe.status.success(), "{}", stderr(&describe));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&describe.stdout).unwrap()["dsl"],
        "file and ext = md"
    );
    assert!(
        run(&["marketplace", "uninstall", "tests/sessions"], root)
            .status
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(root.join("recipe.json")).unwrap(),
        recipe
    );
    assert_eq!(
        std::fs::read_to_string(root.join("insight.json")).unwrap(),
        insight
    );
}
