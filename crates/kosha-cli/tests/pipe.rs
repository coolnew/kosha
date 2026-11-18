// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! What happens when the thing reading our output stops reading.
//!
//! `kosha /usr | head -4` exited 101 with a Rust panic and a backtrace note.
//! So did `| grep -q`, and so did quitting out of `| less`. This is the most
//! ordinary shape a command-line tool is used in, and it was broken in the
//! binary whose whole argument is that you can script it — see
//! `kosha_core::say` for the fix and why it is not the usual one-liner.
//!
//! The test spawns the real binary, because that is the only way to have a real
//! pipe with a real reader that closes: nothing about this reproduces in
//! process.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// Build a tree whose export cannot fit in a pipe buffer.
///
/// Both platforms buffer about 64 KiB. If everything kosha writes fits in
/// that, the child finishes and exits before the reader closes anything, the
/// write never fails, and the test passes without having tested anything.
/// Long names, so each CSV row is wide.
fn crowded_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    for index in 0..2_000 {
        let name = format!("file-{index:05}-with-a-name-long-enough-to-widen-the-row.txt");
        std::fs::write(dir.path().join(name), b"x").expect("a file");
    }
    dir
}

#[test]
fn a_reader_that_walks_away_is_not_a_failure() {
    let tree = crowded_tree();

    let mut child = Command::new(common::kosha())
        .arg(tree.path())
        .args(["--export", "csv"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kosha runs");

    // One line, then the pipe goes away — which is exactly what `head -1`
    // does. Dropping the reader closes the read end.
    {
        let stdout = child.stdout.take().expect("a pipe");
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        reader.read_line(&mut first).expect("at least one line");
        assert!(!first.is_empty(), "no output at all");
    }

    let finished = child.wait_with_output().expect("kosha exits");
    let complaints = String::from_utf8_lossy(&finished.stderr);

    assert!(
        !complaints.contains("panicked"),
        "a closed pipe panicked:\n{complaints}"
    );
    // Zero rather than 141 or 101: the reader chose to stop listening, which
    // is a normal end to a pipeline. A script checking the exit status of
    // `kosha ... | head` should not have to know that.
    assert!(
        finished.status.success(),
        "exited {:?} after the reader closed the pipe\n{complaints}",
        finished.status.code()
    );
}

/// The version stamp specifically, because it is what a bug report is asked
/// for and it is read through a pipe every single time.
#[test]
fn the_version_survives_a_pipe() {
    let out = Command::new(common::kosha())
        .arg("--version")
        .output()
        .expect("kosha runs");

    let line = String::from_utf8_lossy(&out.stdout);
    assert!(line.starts_with("kosha "), "got {line:?}");
    assert!(out.status.success());
}
