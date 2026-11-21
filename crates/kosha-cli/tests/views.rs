// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

mod common;
use kosha_core::{ScanOptions, Scanner, snapshot};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    time::UNIX_EPOCH,
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    snapshot: PathBuf,
    view: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("HOST_PRIVATE_ROOT");
        fs::create_dir_all(root.join("Project/private")).unwrap();
        fs::create_dir_all(root.join("OUTSIDE_PRIVATE")).unwrap();
        fs::write(root.join("Project/public.md"), b"shared document").unwrap();
        fs::write(root.join("Project/private/SECRET_FILE"), b"do not disclose").unwrap();
        fs::write(
            root.join("OUTSIDE_PRIVATE/OTHER_SECRET"),
            b"outside the project",
        )
        .unwrap();
        let tree = Scanner::new(ScanOptions {
            dedup_hardlinks: Some(false),
            ..ScanOptions::default()
        })
        .scan(&root)
        .unwrap();
        let snapshot = temp.path().join("SOURCE_PRIVATE.kosha");
        snapshot::write(
            &tree,
            &snapshot::Meta::of(
                &tree,
                UNIX_EPOCH + std::time::Duration::from_secs(10000),
                "LABEL_PRIVATE",
            ),
            fs::File::create(&snapshot).unwrap(),
        )
        .unwrap();
        let view = temp.path().join("approved.koshaview");
        Self {
            _temp: temp,
            root,
            snapshot,
            view,
        }
    }
    fn publish(&self, confirm: bool) -> std::process::Output {
        let mut c = Command::new(common::kosha());
        c.args(["view", "create", "--snapshot"])
            .arg(&self.snapshot)
            .args([
                "--root",
                "Project",
                "--exclude",
                "private",
                "--alias",
                "shared",
                "--out",
            ])
            .arg(&self.view);
        if confirm {
            let preview = self.publish(false);
            assert!(preview.status.success());
            let v: Value = serde_json::from_slice(&preview.stdout).unwrap();
            c.args([
                "--confirm",
                "--expect-sha256",
                v["sha256"].as_str().unwrap(),
            ]);
        }
        c.output().unwrap()
    }
}

#[test]
fn publication_preview_and_noclobber_preserve_source_and_existing_destination() {
    let f = Fixture::new();
    let before = fs::read(&f.snapshot).unwrap();
    let preview = f.publish(false);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&preview.stdout).unwrap()["file_written"],
        false
    );
    assert!(!f.view.exists());
    assert!(f.publish(true).status.success());
    let bytes = fs::read(&f.view).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for hidden in [
        "SECRET",
        "PRIVATE",
        "private",
        "LABEL_PRIVATE",
        &f.root.to_string_lossy(),
    ] {
        assert!(!text.contains(hidden), "leaked {hidden}");
    }
    fs::write(&f.view, b"valuable existing output").unwrap();
    let refused = f.publish(true);
    assert!(!refused.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&refused.stderr).unwrap()["code"],
        "view_output_refused"
    );
    assert_eq!(fs::read(&f.view).unwrap(), b"valuable existing output");
    assert_eq!(fs::read(&f.snapshot).unwrap(), before);
    assert_eq!(
        fs::read(f.root.join("Project/private/SECRET_FILE")).unwrap(),
        b"do not disclose"
    );
}

#[test]
fn fixed_session_queries_after_artifact_and_live_sources_are_removed_and_rejects_escape_requests() {
    let f = Fixture::new();
    assert!(f.publish(true).status.success());
    let fake_home = f._temp.path().join("profile");
    for dir in [
        "Library/Application Support/app.coolnew.kosha",
        ".config/kosha",
        "roaming/coolnew/kosha",
    ] {
        let d = fake_home.join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("search-recipes.json"),
            b"invalid recipe: must not be read",
        )
        .unwrap();
    }
    let mut child = Command::new(common::kosha())
        .args(["view", "session", "--input"])
        .arg(&f.view)
        .env("HOME", &fake_home)
        .env("XDG_CONFIG_HOME", fake_home.join(".config"))
        .env("APPDATA", fake_home.join("roaming"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let read = |output: &mut BufReader<std::process::ChildStdout>| {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    assert_eq!(read(&mut output)["operation"], "session_ready");
    fs::remove_file(&f.view).unwrap();
    fs::remove_file(&f.snapshot).unwrap();
    fs::remove_dir_all(&f.root).unwrap();
    for request in [
        json!({"op":"scan","path":"/private"}),
        json!({"op":"create"}),
        json!({"op":"query","query":"file","out":"do-not-create"}),
        json!({"op":"query","query":"file","input":"/private"}),
        json!({"op":"query","query":"not owner = admin"}),
    ] {
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        let response = read(&mut output);
        assert!(response.get("code").is_some(), "accepted {request}");
        assert!(!response.to_string().contains("/private"));
    }
    writeln!(
        input,
        "{}",
        json!({"op":"query","query":"files ending with md","mode":"natural","id":"q1"})
    )
    .unwrap();
    input.flush().unwrap();
    let response = read(&mut output);
    assert_eq!(response["id"], "q1");
    assert_eq!(response["matches"], 1);
    assert_eq!(response["rows"][0]["path"], "shared/public.md");
    assert_eq!(response["coverage"]["scope"], "owner_selected_metadata");
    assert_eq!(response["observed_unix"], 10000);
    writeln!(input, "{}", json!({"op":"close"})).unwrap();
    input.flush().unwrap();
    assert_eq!(read(&mut output)["operation"], "closed");
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn forbidden_cli_options_fail_before_opening_input_or_writing_output() {
    let f = Fixture::new();
    for flag in [
        "--out",
        "--snapshot",
        "--duplicates",
        "--cache",
        "--mft",
        "--image",
        "--recipes",
        "--absolute-paths",
    ] {
        let result = Command::new(common::kosha())
            .args([
                "view",
                "query",
                "--input",
                "DOES_NOT_EXIST_PRIVATE",
                "--query",
                "file",
                flag,
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let error: Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_eq!(error["code"], "operation_not_granted");
        assert!(!String::from_utf8_lossy(&result.stderr).contains("DOES_NOT_EXIST_PRIVATE"));
    }
    assert!(!f.view.exists());
}

#[test]
fn consumers_reject_other_formats_and_unshared_date_queries() {
    let f = Fixture::new();
    let invalid = Command::new(common::kosha())
        .args(["view", "inspect", "--input"])
        .arg(&f.snapshot)
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&invalid.stderr).unwrap()["code"],
        "view_invalid"
    );
    assert!(f.publish(true).status.success());
    let unknown = Command::new(common::kosha())
        .args(["view", "query", "--input"])
        .arg(&f.view)
        .args(["--query", "not created > 2020-01-01"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&unknown.stderr).unwrap()["code"],
        "operation_not_granted"
    );
}

#[test]
fn session_request_budget_is_enforced_before_unbounded_allocation() {
    let f = Fixture::new();
    assert!(f.publish(true).status.success());
    let mut child = Command::new(common::kosha())
        .args(["view", "session", "--input"])
        .arg(&f.view)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&vec![b'x'; 8193])
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stderr).unwrap()["code"],
        "view_limit"
    );
}

#[test]
fn confirmation_refuses_a_changed_artifact_before_creating_output() {
    let f = Fixture::new();
    let preview: Value = serde_json::from_slice(&f.publish(false).stdout).unwrap();
    let result = Command::new(common::kosha())
        .args(["view", "create", "--snapshot"])
        .arg(&f.snapshot)
        .args([
            "--root",
            "Project",
            "--exclude",
            "private",
            "--alias",
            "different",
            "--out",
        ])
        .arg(&f.view)
        .args([
            "--confirm",
            "--expect-sha256",
            preview["sha256"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stderr).unwrap()["code"],
        "view_changed"
    );
    assert!(!f.view.exists());
}

#[cfg(unix)]
#[test]
fn publication_never_replaces_destination_links_or_their_targets() {
    use std::os::unix::fs::symlink;
    for dangling in [true, false] {
        let f = Fixture::new();
        let target = f._temp.path().join("valuable-target");
        if !dangling {
            fs::write(&target, b"preserve me").unwrap();
        }
        symlink(&target, &f.view).unwrap();
        assert!(!f.publish(true).status.success());
        assert_eq!(fs::read_link(&f.view).unwrap(), target);
        if dangling {
            assert!(!target.exists());
        } else {
            assert_eq!(fs::read(&target).unwrap(), b"preserve me");
        }
    }
}

#[test]
fn pagination_has_bounded_rows_and_no_invented_totals() {
    let f = Fixture::new();
    assert!(f.publish(true).status.success());
    for offset in ["0", "1", "18446744073709551615"] {
        let output = Command::new(common::kosha())
            .args(["view", "query", "--input"])
            .arg(&f.view)
            .args(["--query", "file", "--limit", "1", "--offset", offset])
            .output()
            .unwrap();
        assert!(output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["matches"], 1);
        assert_eq!(
            result["rows"].as_array().unwrap().len(),
            usize::from(offset == "0")
        );
        assert_eq!(result["has_more"], false);
    }
}
