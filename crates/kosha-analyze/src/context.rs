// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Everything a rule is allowed to know beyond the tree itself.
//!
//! Rules read the clock and the environment from here rather than calling them
//! directly, which is what makes the whole engine deterministic and testable:
//! build a `Context`, and every rule produces the same output every time.

use std::path::PathBuf;

use kosha_core::format::Units;

#[derive(Clone, Debug)]
pub struct Context {
    /// "Now" as Unix seconds. Every age comparison is relative to this.
    pub now: u32,
    /// The user's home directory, when we can identify it. Cache rules that
    /// match on well-known locations need it; everything else ignores it.
    /// Use `kosha_core::path::canonical` so its spelling matches scanned paths.
    pub home: Option<PathBuf>,
    /// Findings smaller than this are dropped before ranking. A 40 MB tip in a
    /// panel about a 900 GB volume is noise.
    pub min_reclaimable: u64,
    /// Age thresholds, in days, exposed so the UI can offer them as settings.
    pub stale_installer_days: u32,
    pub cold_file_days: u32,
    pub vm_image_days: u32,
    /// How big a file has to be before its age is worth mentioning.
    ///
    /// Here for the same reason the day thresholds are, and it was a private
    /// constant in the rule for longer than it should have been: unreachable
    /// from a settings file, and untestable without writing a fixture of
    /// literally that size, which is why `large-and-cold` was the last rule in
    /// the catalogue with no test. "Large" also means different things on a
    /// 256 GB laptop and an 8 TB workstation.
    pub cold_file_bytes: u64,
    /// How to spell byte figures that appear in a finding's own prose.
    ///
    /// Presentation, in what is otherwise an analysis type — but a rule that
    /// writes a sentence for a human has to spell numbers the same way the rest
    /// of the interface does, or one card ends up reading "39.4 MB" in its
    /// corner and "6.24 MiB" in its text.
    pub units: Units,
}

impl Context {
    /// A context anchored to the current wall clock and environment.
    pub fn detect() -> Self {
        Context {
            now: kosha_core::format::now_unix(),
            home: home_dir(),
            ..Context::at(kosha_core::format::now_unix())
        }
    }

    /// A context anchored to a fixed instant, with no environment. This is what
    /// tests use, and it is the reason rule output is reproducible.
    pub fn at(now: u32) -> Self {
        Context {
            now,
            home: None,
            min_reclaimable: 32 * 1024 * 1024,
            stale_installer_days: 90,
            cold_file_days: 365,
            vm_image_days: 180,
            cold_file_bytes: 1024 * 1024 * 1024,
            units: Units::Binary,
        }
    }

    /// Seconds since `mtime`, saturating for clocks that ran backwards.
    #[inline]
    pub fn age_secs(&self, mtime: u32) -> u32 {
        self.now.saturating_sub(mtime)
    }

    #[inline]
    pub fn older_than_days(&self, mtime: u32, days: u32) -> bool {
        // An unknown mtime is not evidence of staleness.
        mtime != 0 && self.age_secs(mtime) > days.saturating_mul(86_400)
    }
}

impl Default for Context {
    fn default() -> Self {
        Context::detect()
    }
}

/// Resolved from the environment rather than a crate, because this is the whole
/// of what we need and the platform variables are stable.
fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";

    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .map(|path| kosha_core::path::canonical(&path))
}
