// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! What `kosha-gui` does before it opens a window.
//!
//! These run the real binary, which is safe for exactly these arguments: each
//! one is answered or refused without a window ever being created, so the
//! tests need no display and leave nothing behind.
//!
//! **The exit code is the thing under test.** A window that refuses to open
//! and exits 0 tells a launcher, a script or a desktop entry that it opened —
//! and `kosha-gui --nonsense` did exactly that, as did `--query` with nothing
//! after it. The command line's own front end has always exited 1 for the same
//! two mistakes, and two front ends of one program disagreeing about whether
//! something failed is the kind of difference nobody reports as a bug.

use std::process::Command;

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kosha-gui"))
        .args(args)
        .output()
        .expect("kosha-gui runs");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn what_was_asked_for_is_answered_and_exits_zero() {
    let (code, text) = run(&["--help"]);
    assert_eq!(code, 0, "--help: {text}");
    assert!(text.contains("USAGE:"), "{text}");
    assert!(
        text.contains("--since"),
        "the usage is missing a flag: {text}"
    );

    let (code, text) = run(&["--version"]);
    assert_eq!(code, 0, "--version: {text}");
    // The same line the scanner gives, which is the whole reason this is not
    // `CARGO_PKG_VERSION`: two front ends of one program must not answer
    // "which build is this?" differently.
    assert_eq!(text.trim(), kosha_core::version::long());
}

#[test]
fn a_command_line_it_cannot_use_is_refused_and_exits_non_zero() {
    for (args, expected) in [
        (vec!["--nonsense"], "unknown option"),
        (vec!["--query"], "needs a query"),
        (vec!["--since"], "needs a snapshot"),
    ] {
        let (code, text) = run(&args);
        assert_eq!(code, 1, "`{args:?}` exited {code}: {text}");
        assert!(
            text.contains(expected),
            "`{args:?}` did not say why: {text}"
        );
    }
}
