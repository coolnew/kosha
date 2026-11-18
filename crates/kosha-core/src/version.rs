// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Which build this is.
//!
//! Both front ends have to be able to answer that, and they have to answer it
//! the same way — a bug report saying `0.1.0` from the CLI and something else
//! from the window would be worse than neither saying anything. One definition,
//! in the crate they both depend on.
//!
//! The commit comes from `build.rs`, which degrades to `unknown` where there is
//! no `.git` or no `git`. That is the normal case for a source tarball and not
//! worth failing a build over.

/// The release number, from `Cargo.toml`.
pub const NUMBER: &str = env!("CARGO_PKG_VERSION");

/// Short commit hash, with a trailing `+` if the tree had uncommitted changes
/// when this was built. `unknown` where it could not be determined.
///
/// The `+` is the part that earns its place. A report from a modified tree is a
/// different conversation from one against a commit somebody else can check
/// out, and without the marker the two look identical.
pub const COMMIT: &str = env!("KOSHA_COMMIT");

/// The target triple this was compiled for, which is not always the one the
/// reporter thinks they are running.
pub const TARGET: &str = env!("KOSHA_TARGET");

/// One line: `kosha 0.1.0 (a1b2c3d, x86_64-pc-windows-msvc)`.
///
/// Everything needed to reproduce a build, in a form somebody can paste into an
/// issue without being asked three follow-up questions.
pub fn long() -> String {
    format!("kosha {NUMBER} ({COMMIT}, {TARGET})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_number_is_a_version_rather_than_a_placeholder() {
        assert!(
            NUMBER.split('.').count() >= 2,
            "not a version number: {NUMBER}"
        );
        assert!(NUMBER.chars().next().is_some_and(|c| c.is_ascii_digit()));
    }

    /// A build script that silently produced nothing would leave these empty,
    /// and the line would read `kosha 0.1.0 (, )` — which looks like a bug in
    /// the formatting rather than a missing stamp.
    #[test]
    fn the_stamp_is_never_blank() {
        assert!(!COMMIT.is_empty());
        assert!(!TARGET.is_empty());
    }

    #[test]
    fn the_long_form_carries_all_three() {
        let line = long();
        assert!(line.starts_with("kosha "), "{line}");
        for part in [NUMBER, COMMIT, TARGET] {
            assert!(line.contains(part), "`{part}` missing from `{line}`");
        }
    }

    /// The dirty marker is a suffix, so anything parsing the hash can strip it
    /// and anything displaying it does not have to.
    #[test]
    fn a_dirty_marker_is_only_ever_a_trailing_plus() {
        let stem = COMMIT.strip_suffix('+').unwrap_or(COMMIT);
        assert!(!stem.contains('+'), "the marker is not a suffix: {COMMIT}");
        if stem != "unknown" {
            assert!(
                stem.chars().all(|c| c.is_ascii_hexdigit()),
                "not a commit hash: {COMMIT}"
            );
        }
    }
}
