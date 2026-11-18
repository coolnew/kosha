// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! What the tests in this directory share.
//!
//! A directory rather than a file so that cargo does not build it as a test
//! binary of its own, which is the one way to share code between integration
//! tests: each `tests/*.rs` is its own crate.

use std::path::PathBuf;

/// The binary under test.
///
/// `CARGO_BIN_EXE_kosha` is baked in when the test is compiled and is an
/// absolute path into this checkout's `target/`. That is the right answer
/// everywhere except the place it matters most — **a machine with no checkout on
/// it.** The clean-machine sandbox scenario copies these test binaries and the
/// program onto a Windows installed minutes ago, and the two that drive the
/// program were the only ones it could not run: the path they were built with
/// names a drive that does not exist there.
///
/// So the environment may name the binary instead. Nothing else changes: the
/// tests still run the program as a process and read what it prints, which is
/// the whole point of them.
pub fn kosha() -> PathBuf {
    std::env::var_os("KOSHA_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_kosha")))
}
