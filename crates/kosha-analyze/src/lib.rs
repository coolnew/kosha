// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `kosha-analyze` — turn a scanned tree into a short list of things worth
//! doing about it.
//!
//! Not machine learning: a deterministic rule engine. Every finding can be
//! explained in one sentence, reproduced exactly, and traced back to the nodes
//! that caused it. The built-in catalogue is exposed by [`registered_rules`].
//!
//! ```no_run
//! use kosha_analyze::{Context, analyze};
//!
//! let tree = kosha_core::scan(std::path::Path::new("."))?;
//! for insight in analyze(&tree, &Context::detect()) {
//!     println!("{} — {}", insight.title, insight.detail);
//! }
//! # Ok::<(), kosha_core::ScanError>(())
//! ```
//!
//! Rules are pure: no I/O, no clock, no randomness. Anything that needs to read
//! file contents lives in [`duplicates`] instead, runs only when asked, and is
//! priced in bytes rather than entries.
//!
//! This crate cannot modify the filesystem. It does not depend on
//! `kosha-fsops`, which is the only crate that can.

pub mod cleanup;
pub mod context;
pub mod duplicates;
mod engine;
pub mod fragmented;
mod insight;
pub mod location;
pub mod pack;
mod rules;

pub use context::{Context, KnownLocations};
pub use engine::{analyze, findings, findings_for_source, findings_with, rank, total_reclaimable};
pub use insight::{Confidence, Insight, RuleId, Severity, Suggested};
pub use rules::Rule;

/// The id of every registered rule.
///
/// Exposed so a test can assert that the documented catalogue and the running
/// one are the same set — a catalogue that promises rules it does not run is
/// one nobody can trust to be complete.
pub fn registered_rules() -> Vec<RuleId> {
    rules::all().iter().map(|rule| rule.id()).collect()
}
