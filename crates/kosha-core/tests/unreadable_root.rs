// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Run in a separate test process. The live MFT test in scan.rs can enable
//! SeBackupPrivilege on the shared Windows process token, bypassing this
//! fixture's deny ACL even though this test only requests a directory walk.

use std::path::Path;

use kosha_core::{ScanOptions, Scanner};

/// A folder nothing could be read from is not a folder with nothing in it.
///
/// **Measured on `C:\System Volume Information`**, which an ordinary account
/// cannot open: the window drew *"This folder is empty."* in the middle of the
/// treemap and the command line printed `~0 B on disk` and exited 0. Each had
/// one line to the contrary — `1 unreadable` in the status bar, "1 entry could
/// not be read" above the table — and neither is an answer to "what is in
/// here".
///
/// The flag was already on the root node; nothing asked it. `root_unreadable`
/// is the question, and it is deliberately not `is_partial`: partial means the
/// totals are a floor, and a floor of nothing is not a number worth showing.
#[test]
fn a_root_that_could_not_be_read_says_so_rather_than_reading_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("inside.bin"), vec![0u8; 4096]).unwrap();
    let _guard = deny_reading(&locked);
    let error = std::fs::read_dir(&locked).expect_err("fixture must deny directory enumeration");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

    let tree = Scanner::new(ScanOptions::default()).scan(&locked).unwrap();
    assert!(
        tree.root_unreadable(),
        "the scan read a folder that cannot be read: {} entries",
        tree.len()
    );
    assert_eq!(tree.len(), 1, "nothing but the root itself is in the tree");
    assert!(tree.is_partial(), "and the totals are not a total");

    // An ordinary folder is not mistaken for one, including an empty one —
    // which is the case this has to stay distinct from.
    let ordinary = dir.path().join("ordinary");
    std::fs::create_dir(&ordinary).unwrap();
    let scanned = Scanner::new(ScanOptions::default())
        .scan(&ordinary)
        .unwrap();
    assert!(!scanned.root_unreadable(), "an empty folder is readable");
    assert!(!scanned.is_partial());
}

/// Make a directory unopenable, and put it back afterwards.
///
/// Both platforms, because the defect was found on one and the code is shared.
/// On Windows that means an explicit deny for this account — the temporary
/// directory is ours, so no privilege is needed — and on Unix a mode with no
/// bits on it. Either way the guard restores it, or `tempfile` cannot clean up
/// and the test leaves a folder nobody can delete.
fn deny_reading(path: &Path) -> impl Drop {
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            #[cfg(windows)]
            {
                let who = std::env::var("USERNAME").unwrap_or_default();
                let _ = std::process::Command::new("icacls.exe")
                    .args([&self.0.to_string_lossy() as &str, "/remove:d", &who, "/q"])
                    .output();
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
    }

    #[cfg(windows)]
    {
        let who = std::env::var("USERNAME").expect("an account name");
        let done = std::process::Command::new("icacls.exe")
            .args([
                &path.to_string_lossy() as &str,
                "/deny",
                &format!("{who}:(RX)"),
                "/q",
            ])
            .output()
            .expect("icacls runs");
        assert!(
            done.status.success(),
            "icacls: {}",
            String::from_utf8_lossy(&done.stderr)
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    Restore(path.to_path_buf())
}
