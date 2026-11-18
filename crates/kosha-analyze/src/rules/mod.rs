// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The rule catalogue.
//!
//! Every rule is a pure function of `(tree, context)`. No I/O, no clock, no
//! randomness — which is why the whole engine is testable against synthetic
//! trees and produces byte-identical output on every run. A rule that needs to
//! read file contents does not belong here; see [`crate::duplicates`].

use kosha_core::SizeTree;

use crate::context::Context;
use crate::insight::{Insight, RuleId};

mod caches;
mod stale;
mod waste;

pub(crate) mod util;

pub trait Rule: Send + Sync {
    fn id(&self) -> RuleId;
    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight>;
}

/// Every rule, in no particular order — the engine ranks the output, so
/// registration order carries no meaning.
pub fn all() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(caches::BuildArtifacts),
        Box::new(caches::PackageManagerCache),
        Box::new(caches::BrowserCache),
        Box::new(caches::AppCache),
        Box::new(caches::TrashNotEmptied),
        Box::new(stale::StaleInstallers),
        Box::new(stale::LargeAndCold),
        Box::new(stale::LogFiles),
        Box::new(stale::OrphanedVmImages),
        Box::new(waste::ClusterWaste),
        Box::new(waste::SparseSavings),
        Box::new(waste::Compressible),
        Box::new(waste::DominantChild),
        Box::new(waste::DeepNesting),
    ]
}
