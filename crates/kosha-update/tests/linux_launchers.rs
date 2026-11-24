// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io::{Seek, Write};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use kosha_update::managed_linux::Installation;
use semver::Version;

fn program(contents: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(contents).unwrap();
    file.rewind().unwrap();
    file
}

fn executable(version: &str) -> File {
    program(format!("#!/bin/sh\nprintf '%s' '{version}'\n").as_bytes())
}

fn run(path: &Path) -> String {
    let output = Command::new(path).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn native_launchers_follow_install_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("installation");
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let old = Version::new(1, 0, 0);
    let new = Version::new(1, 1, 0);
    let install = Installation::open(&root).unwrap();
    install
        .install(&old, &mut executable("cli 1"), &mut executable("gui 1"))
        .unwrap();
    for name in ["kosha", "kosha-gui"] {
        symlink(root.join("current").join(name), bin.join(name)).unwrap();
    }
    assert_eq!(run(&bin.join("kosha")), "cli 1");
    assert_eq!(run(&bin.join("kosha-gui")), "gui 1");

    // Reopen like a separate CLI or GUI process operating on this profile.
    drop(install);
    let install = Installation::open(&root).unwrap();
    install
        .install(&new, &mut executable("cli 2"), &mut executable("gui 2"))
        .unwrap();
    assert_eq!(run(&bin.join("kosha")), "cli 2");
    assert_eq!(run(&bin.join("kosha-gui")), "gui 2");
    assert!(
        install
            .install(
                &old,
                &mut executable("stale cli"),
                &mut executable("stale gui")
            )
            .is_err()
    );
    assert_eq!(install.rollback().unwrap(), old);
    assert_eq!(run(&bin.join("kosha")), "cli 1");
    assert_eq!(run(&bin.join("kosha-gui")), "gui 1");
    assert!(install.rollback().is_err());
    install.activate(&new).unwrap();
    assert_eq!(run(&bin.join("kosha")), "cli 2");
}
