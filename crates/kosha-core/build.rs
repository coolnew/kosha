// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Stamp the build with what it was built from.
//!
//! "Which build is this?" is the first question on any bug report about a
//! program distributed as a binary, and a version number alone cannot answer
//! it: between two releases there are hundreds of commits that all say the
//! same number.
//!
//! Everything here degrades rather than fails. A source tarball has no `.git`,
//! a build machine may have no `git`, and neither is a reason to refuse to
//! compile a disk scanner — they are reasons to say "unknown" and carry on.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let root = workspace_root();
    // A commit changes both of these, so the stamp keeps up without asking
    // cargo to rerun this on every build.
    if let Some(root) = root.as_deref() {
        // A linked worktree has a .git file, with HEAD/index kept elsewhere.
        // Watch their actual locations instead of appending names to that file.
        let marker = root.join(".git");
        if !marker.is_dir() {
            // Watch a worktree pointer or a missing future marker, never the
            // entire ordinary .git directory and all of its object files.
            println!("cargo:rerun-if-changed={}", marker.display());
        }
        if owns_checkout(root) {
            for name in ["HEAD", "index"] {
                if let Some(path) = run(root, &["rev-parse", "--git-path", name]) {
                    println!("cargo:rerun-if-changed={}", root.join(path).display());
                }
            }
        }
    }

    println!(
        "cargo:rustc-env=KOSHA_COMMIT={}",
        root.as_deref()
            .map(commit)
            .unwrap_or_else(|| "unknown".into())
    );
    println!(
        "cargo:rustc-env=KOSHA_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );
}

fn workspace_root() -> Option<PathBuf> {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    manifest.parent()?.parent()?.canonicalize().ok()
}

fn owns_checkout(root: &Path) -> bool {
    // Git searches parent directories. An unpacked source archive inside a
    // different repository must never borrow that repository's identity.
    if !root.join(".git").exists() {
        return false;
    }
    run(root, &["rev-parse", "--show-toplevel"])
        .and_then(|path| PathBuf::from(path).canonicalize().ok())
        .is_some_and(|top| top == root)
}

/// The short commit, with a `+` if the tree had uncommitted changes.
///
/// The marker matters more than it looks: a bug report from a modified tree is
/// a different conversation from one against a commit anybody else can check
/// out, and without it the two are indistinguishable.
fn commit(root: &Path) -> String {
    if !owns_checkout(root) {
        return "unknown".into();
    }
    let Some(hash) = run(root, &["rev-parse", "--short", "HEAD"]) else {
        return "unknown".into();
    };
    // `--quiet` exits non-zero when there is a difference. Staged changes are
    // checked separately because `git diff` alone does not see them.
    let clean =
        status(root, &["diff", "--quiet"]) && status(root, &["diff", "--cached", "--quiet"]);
    if clean { hash } else { format!("{hash}+") }
}

fn git(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root).args(args);
    // A caller's Git plumbing overrides must not redirect stamp lookup to a
    // different checkout or index. Linked worktrees are discovered from .git.
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

fn run(root: &Path, args: &[&str]) -> Option<String> {
    let out = git(root, args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn status(root: &Path, args: &[&str]) -> bool {
    git(root, args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
