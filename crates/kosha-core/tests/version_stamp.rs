// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Execute the real standalone build script against unrelated/archive/worktree
//! fixtures. No source copy can borrow an enclosing checkout's commit identity.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn successful(mut command: Command) -> Output {
    let output = command.output().expect("test process starts");
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root).args(args);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    command
}

fn optional_git(root: &Path, args: &[&str]) -> Option<String> {
    let output = git_command(root, args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn git(root: &Path, args: &[&str]) -> String {
    String::from_utf8(successful(git_command(root, args)).stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn fixture(root: &Path) -> PathBuf {
    let manifest = root.join("crates/kosha-core");
    std::fs::create_dir_all(&manifest).unwrap();
    std::fs::write(root.join("Cargo.toml"), b"[workspace]\n").unwrap();
    manifest
}

fn probe(executable: &Path, root: &Path) -> (String, String) {
    let mut command = Command::new(executable);
    command
        .current_dir(root.join("crates/kosha-core"))
        .env("CARGO_MANIFEST_DIR", root.join("crates/kosha-core"))
        .env("TARGET", "stamp-fixture-target");
    let output = String::from_utf8(successful(command).stdout).unwrap();
    let stamp = output
        .lines()
        .find_map(|line| line.strip_prefix("cargo:rustc-env=KOSHA_COMMIT="))
        .expect("script reports a commit value")
        .to_string();
    (stamp, output)
}

#[test]
fn source_archives_never_borrow_an_enclosing_repository_stamp() {
    if !Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!(
            "Skipping Git stamp fixtures: git is unavailable; production source builds still fall back to unknown."
        );
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join(if cfg!(windows) {
        "stamp-probe.exe"
    } else {
        "stamp-probe"
    });
    let mut compiler = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    compiler
        .arg("--edition=2024")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"))
        .arg("-o")
        .arg(&executable);
    successful(compiler);

    let real_workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    let real_stamp = probe(&executable, &real_workspace).0;
    let own_checkout = real_workspace.join(".git").exists()
        && optional_git(&real_workspace, &["rev-parse", "--show-toplevel"])
            .and_then(|path| PathBuf::from(path).canonicalize().ok())
            .is_some_and(|top| top == real_workspace);
    let real_head = own_checkout
        .then(|| optional_git(&real_workspace, &["rev-parse", "--short", "HEAD"]))
        .flatten();
    if let Some(real_head) = real_head {
        assert_eq!(
            real_stamp.strip_suffix('+').unwrap_or(&real_stamp),
            real_head
        );
    } else {
        // Exported source archives and unborn public checkouts have no usable
        // commit. Running this suite must not require the private Git history.
        assert_eq!(real_stamp, "unknown");
    }

    let outer = temp.path().join("unrelated-checkout");
    let outer_manifest = fixture(&outer);
    git(&outer, &["init", "-q"]);
    assert_eq!(probe(&executable, &outer).0, "unknown", "unborn checkout");
    git(&outer, &["add", "Cargo.toml"]);
    let hooks = temp.path().join("empty-hooks");
    std::fs::create_dir(&hooks).unwrap();
    git(
        &outer,
        &[
            "-c",
            "user.name=Stamp Fixture",
            "-c",
            "user.email=stamp@example.invalid",
            "-c",
            &format!("core.hooksPath={}", hooks.display()),
            "commit",
            "--no-gpg-sign",
            "-qm",
            "fixture",
        ],
    );
    let head = git(&outer, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(probe(&executable, &outer).0, head);

    // Before the fix, rev-parse from this unpacked crate found outer/.git and
    // confidently stamped a source archive with the unrelated repository hash.
    let unpacked = outer.join("unpacked-kosha-source");
    fixture(&unpacked);
    assert_eq!(probe(&executable, &unpacked).0, "unknown");

    let standalone = temp.path().join("standalone-source");
    fixture(&standalone);
    assert_eq!(probe(&executable, &standalone).0, "unknown");

    std::fs::write(outer.join("Cargo.toml"), b"[workspace]\n# edited\n").unwrap();
    assert_eq!(probe(&executable, &outer).0, format!("{head}+"));
    git(&outer, &["add", "Cargo.toml"]);
    assert_eq!(probe(&executable, &outer).0, format!("{head}+"));

    let worktree = temp.path().join("linked-worktree");
    git(
        &outer,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "stamp-linked",
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(worktree.join("crates/kosha-core")).unwrap();
    assert!(worktree.join(".git").is_file());
    let (stamp, output) = probe(&executable, &worktree);
    assert_eq!(stamp, head);
    let actual_head = git(&worktree, &["rev-parse", "--git-path", "HEAD"]);
    assert!(output.contains(&actual_head), "{output}");

    // An inherited plumbing override must not substitute the other checkout.
    let mut overridden = Command::new(&executable);
    overridden
        .current_dir(&outer_manifest)
        .env("CARGO_MANIFEST_DIR", &outer_manifest)
        .env("GIT_DIR", worktree.join(".git"))
        .env("GIT_WORK_TREE", &worktree);
    let output = String::from_utf8(successful(overridden).stdout).unwrap();
    assert!(
        output.contains(&format!("KOSHA_COMMIT={head}+")),
        "{output}"
    );

    let mut no_git = Command::new(&executable);
    no_git
        .current_dir(&outer_manifest)
        .env("CARGO_MANIFEST_DIR", &outer_manifest)
        .env("PATH", &hooks);
    let output = String::from_utf8(successful(no_git).stdout).unwrap();
    assert!(output.contains("KOSHA_COMMIT=unknown"), "{output}");
}
