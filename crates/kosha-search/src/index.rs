// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use kosha_core::{NodeId, SizeTree};
use kosha_query::{Expr, Field, Op, Query, Results, Value};
use rustc_hash::FxHashMap;

/// Read-only arena handle plus compact extension postings. Names and paths
/// remain in the original arena; labels are shared per extension, capped at
/// 4096 distinct labels. Unsupported predicates use the original evaluator.
#[derive(Clone, Debug)]
pub struct SearchIndex {
    tree: Arc<SizeTree>,
    extensions: Arc<ExtensionPostings>,
    creation: CreationCoverage,
}

/// Recorded birth-time coverage for searchable entries, excluding the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreationCoverage {
    pub known: usize,
    pub total: usize,
}

impl SearchIndex {
    pub fn new(tree: Arc<SizeTree>) -> Self {
        Self::new_cancellable(tree, &AtomicBool::new(false)).expect("uncancelled build")
    }

    /// Build postings on a worker; obsolete tree/index builds can be cancelled.
    pub fn new_cancellable(tree: Arc<SizeTree>, cancel: &AtomicBool) -> Option<Self> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut labels: FxHashMap<String, Vec<NodeId>> = FxHashMap::default();
        // Reserve the documented file families before admitting arbitrary
        // extensions: a tree containing thousands of unique suffixes must not
        // displace Markdown, C, Office, or media acceleration.
        for extension in crate::translate::common_extensions() {
            labels.entry(extension.into()).or_default();
        }
        let mut complete = true;
        let mut creation = CreationCoverage {
            known: 0,
            total: tree.len().saturating_sub(1),
        };
        for i in 1..tree.len() {
            if i & 1023 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            let id = NodeId::from_index(i);
            creation.known += usize::from(tree.created(id).is_some());
            let name = tree.name_lossy(id);
            let extension = match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => ext,
                _ => "",
            };
            let lower;
            let extension = if extension.bytes().any(|b| b.is_ascii_uppercase()) {
                lower = extension.to_ascii_lowercase();
                lower.as_str()
            } else {
                extension
            };
            if let Some(posting) = labels.get_mut(extension) {
                posting.push(id);
            } else if labels.len() < 4096 {
                labels.insert(extension.into(), vec![id]);
            } else {
                complete = false;
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let labels = labels
            .into_iter()
            .map(|(label, ids)| (label, ids.into_boxed_slice()))
            .collect();
        Some(Self {
            tree,
            extensions: Arc::new(ExtensionPostings { labels, complete }),
            creation,
        })
    }

    pub fn tree(&self) -> &Arc<SizeTree> {
        &self.tree
    }

    pub fn creation_coverage(&self) -> CreationCoverage {
        self.creation
    }

    /// Approximate extra resident storage, excluding the shared scan arena.
    pub fn memory_bytes(&self) -> usize {
        self.extensions.labels.capacity() * (size_of::<String>() + size_of::<Box<[NodeId]>>() + 1)
            + self
                .extensions
                .labels
                .iter()
                .map(|(label, ids)| label.capacity() + ids.len() * size_of::<NodeId>())
                .sum::<usize>()
    }

    /// Search the whole arena, retaining only the largest `limit` rows. Counts
    /// and non-overlapping bytes cover every match, independently of the cap.
    /// Cancellation returns `None`, so partial counts cannot look complete.
    /// Scratch is one bit per node for ancestor coverage, an additional bit per
    /// node for indexed predicates, and O(limit) heap entries.
    pub fn run(
        &self,
        query: &Query,
        now: u32,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Option<Results> {
        let tree = &self.tree;
        self.run_sorted(query, now, limit, cancel, |a, b| {
            let (a_node, b_node) = (tree.node(a), tree.node(b));
            b_node
                .allocated
                .cmp(&a_node.allocated)
                .then_with(|| b_node.logical.cmp(&a_node.logical))
                .then(a.cmp(&b))
        })
    }

    /// Globally select the first `limit` rows under the requested ordering,
    /// rather than sorting an arbitrary retained subset. Less displays first.
    pub fn run_sorted(
        &self,
        query: &Query,
        now: u32,
        limit: usize,
        cancel: &AtomicBool,
        compare: impl Fn(NodeId, NodeId) -> CmpOrdering,
    ) -> Option<Results> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let tree = &self.tree;
        let candidates = match self.extensions.select(query.expression()) {
            Candidates::All => None,
            Candidates::Postings(postings) => {
                let mut bits = vec![0u64; tree.len().div_ceil(64)];
                for ids in postings {
                    for (n, id) in ids.iter().enumerate() {
                        if n & 1023 == 0 && cancel.load(Ordering::Relaxed) {
                            return None;
                        }
                        let i = id.index();
                        bits[i / 64] |= 1u64 << (i % 64);
                    }
                }
                Some(bits)
            }
        };
        let mut covered = vec![0u64; tree.len().div_ceil(64)];
        let mut best = BinaryHeap::with_capacity(limit.min(tree.len()));
        let mut total_matched = 0usize;
        let mut total_allocated = 0u64;
        for (i, node) in tree.nodes().iter().enumerate().skip(1) {
            if i & 1023 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            let parent = node.parent as usize;
            let ancestor_matches = covered[parent / 64] & (1u64 << (parent % 64)) != 0;
            let id = NodeId::from_index(i);
            let possible = candidates
                .as_ref()
                .is_none_or(|bits| bits[i / 64] & (1u64 << (i % 64)) != 0);
            let matched = possible && query.matches(tree, id, now);
            if ancestor_matches || matched {
                covered[i / 64] |= 1u64 << (i % 64);
            }
            if !matched {
                continue;
            }
            total_matched += 1;
            if !ancestor_matches {
                total_allocated = total_allocated.saturating_add(node.allocated);
            }
            if limit == 0 {
                continue;
            }
            let key = Ranked {
                id,
                compare: &compare,
            };
            if best.len() < limit {
                best.push(key);
            } else if best.peek().is_some_and(|worst| key < *worst) {
                *best.peek_mut().expect("nonempty bounded heap") = key;
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let matches = best
            .into_sorted_vec()
            .into_iter()
            .map(|ranked| ranked.id)
            .collect();
        Some(Results {
            matches,
            total_matched,
            total_allocated,
        })
    }
}

#[derive(Debug)]
struct ExtensionPostings {
    labels: FxHashMap<String, Box<[NodeId]>>,
    complete: bool,
}
enum Candidates<'a> {
    All,
    Postings(Vec<&'a [NodeId]>),
}
impl ExtensionPostings {
    fn one(&self, value: &str) -> Candidates<'_> {
        let lower = value.to_ascii_lowercase();
        match self.labels.get(&lower) {
            Some(ids) => Candidates::Postings(vec![ids]),
            None if self.complete => Candidates::Postings(vec![]),
            None => Candidates::All,
        }
    }
    fn select(&self, expr: &Expr) -> Candidates<'_> {
        match expr {
            Expr::Compare {
                field: Field::Ext,
                op: Op::Eq,
                value: Value::Text(value),
            } => self.one(value),
            Expr::In {
                field: Field::Ext,
                values,
                negated: false,
            } => {
                let mut result = vec![];
                for value in values {
                    if let Value::Text(value) = value {
                        match self.one(value) {
                            Candidates::All => return Candidates::All,
                            Candidates::Postings(ids) => result.extend(ids),
                        }
                    }
                }
                Candidates::Postings(result)
            }
            Expr::And(a, b) => match (self.select(a), self.select(b)) {
                (Candidates::All, other) | (other, Candidates::All) => other,
                (Candidates::Postings(a), Candidates::Postings(b)) => {
                    if a.iter().map(|ids| ids.len()).sum::<usize>()
                        <= b.iter().map(|ids| ids.len()).sum::<usize>()
                    {
                        Candidates::Postings(a)
                    } else {
                        Candidates::Postings(b)
                    }
                }
            },
            Expr::Or(a, b) => match (self.select(a), self.select(b)) {
                (Candidates::Postings(mut a), Candidates::Postings(b)) => {
                    a.extend(b);
                    Candidates::Postings(a)
                }
                _ => Candidates::All,
            },
            _ => Candidates::All,
        }
    }
}

struct Ranked<'a, F> {
    id: NodeId,
    compare: &'a F,
}
impl<F: Fn(NodeId, NodeId) -> CmpOrdering> PartialEq for Ranked<'_, F> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<F: Fn(NodeId, NodeId) -> CmpOrdering> Eq for Ranked<'_, F> {}
impl<F: Fn(NodeId, NodeId) -> CmpOrdering> PartialOrd for Ranked<'_, F> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl<F: Fn(NodeId, NodeId) -> CmpOrdering> Ord for Ranked<'_, F> {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        (self.compare)(self.id, other.id).then(self.id.cmp(&other.id))
    }
}
