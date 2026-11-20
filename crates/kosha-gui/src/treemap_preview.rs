// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Delayed treemap previews from recorded metadata only. Selection runs on a
//! worker with six retained candidates, cancellation and a sixteen-entry cache.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Task, Window, div, px, relative,
};
use kosha_core::{Flags, NodeId, SizeTree, format};

use crate::theme::{Palette, metrics};

const CHILDREN: usize = 6;
const CACHED_FOLDERS: usize = 16;

#[derive(Debug)]
struct Summary {
    children: Vec<NodeId>,
    total: usize,
}

#[derive(Default)]
pub(crate) struct Cache {
    entries: Mutex<VecDeque<(NodeId, Arc<Summary>)>>,
}

impl Cache {
    fn get(&self, id: NodeId) -> Option<Arc<Summary>> {
        let mut entries = self.entries.lock().ok()?;
        let index = entries.iter().position(|(key, _)| *key == id)?;
        let row = entries.remove(index)?;
        let result = Arc::clone(&row.1);
        entries.push_back(row);
        Some(result)
    }

    fn insert(&self, id: NodeId, summary: Arc<Summary>) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|(key, _)| *key != id);
            if entries.len() == CACHED_FOLDERS {
                entries.pop_front();
            }
            entries.push_back((id, summary));
        }
    }
}

/// A replacement scan gets its own cache. A late old worker can populate only
/// its old cache, never attach a previous scan's NodeIds to the current tree.
#[derive(Default)]
pub(crate) struct Store {
    tree: Weak<SizeTree>,
    cache: Arc<Cache>,
}

impl Store {
    pub fn for_tree(&mut self, tree: &Arc<SizeTree>) -> Arc<Cache> {
        let current = Arc::downgrade(tree);
        if !self.tree.ptr_eq(&current) {
            self.tree = current;
            self.cache = Arc::default();
        }
        Arc::clone(&self.cache)
    }
}

fn largest<K: Ord>(keys: impl IntoIterator<Item = K>, cancelled: &AtomicBool) -> Option<Vec<K>> {
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    let mut heap = BinaryHeap::with_capacity(CHILDREN);
    for (index, key) in keys.into_iter().enumerate() {
        if index % 256 == 0 && cancelled.load(Ordering::Relaxed) {
            return None;
        }
        if heap.len() < CHILDREN {
            heap.push(Reverse(key));
        } else if heap.peek().is_some_and(|worst| key > worst.0) {
            heap.pop();
            heap.push(Reverse(key));
        }
    }
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    let mut ordered: Vec<_> = heap.into_iter().map(|key| key.0).collect();
    ordered.sort_unstable_by(|a, b| b.cmp(a));
    Some(ordered)
}

fn summarize(tree: &SizeTree, id: NodeId, cancelled: &AtomicBool) -> Option<Summary> {
    let keys = tree.children(id).map(|child| {
        let node = tree.node(child);
        (
            node.allocated,
            node.logical,
            Reverse(tree.name(child)),
            Reverse(child.index()),
        )
    });
    let children = largest(keys, cancelled)?
        .into_iter()
        .map(|(_, _, _, Reverse(index))| NodeId::from_index(index))
        .collect();
    Some(Summary {
        children,
        total: tree.node(id).child_count as usize,
    })
}

struct Preview {
    tree: Arc<SizeTree>,
    id: NodeId,
    palette: Palette,
    units: format::Units,
    summary: Option<Arc<Summary>>,
    cancelled: Arc<AtomicBool>,
    _task: Option<Task<()>>,
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Preview {
    fn new(
        tree: Arc<SizeTree>,
        id: NodeId,
        cache: Arc<Cache>,
        palette: Palette,
        units: format::Units,
        cx: &mut Context<Self>,
    ) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let summary = cache.get(id);
        let task = (summary.is_none() && tree.node(id).is_dir()).then(|| {
            let tree = Arc::clone(&tree);
            let cancelled = Arc::clone(&cancelled);
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { summarize(&tree, id, &cancelled).map(Arc::new) })
                    .await;
                if let Some(summary) = result {
                    cache.insert(id, Arc::clone(&summary));
                    this.update(cx, |view, cx| {
                        view.summary = Some(summary);
                        cx.notify();
                    })
                    .ok();
                }
            })
        });
        Self {
            tree,
            id,
            palette,
            units,
            summary,
            cancelled,
            _task: task,
        }
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let node = self.tree.node(self.id);
        let mut body = v_flex()
            .w(px(320.0))
            .min_w(px(0.0))
            .gap(px(7.0))
            .text_color(palette.text)
            .text_size(px(metrics::TEXT_SM))
            .child(
                div()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_middle()
                    .child(SharedString::from(
                        format::printable(&self.tree.name_lossy(self.id)).into_owned(),
                    )),
            )
            .child(
                div()
                    .text_color(palette.text_dim)
                    .child(SharedString::from(format!(
                        "{} on disk · {} apparent",
                        format::bytes_in(node.allocated, self.units),
                        format::bytes_in(node.logical, self.units)
                    ))),
            );
        if node.is_dir() {
            if let Some(summary) = &self.summary {
                body = body.child(
                    div()
                        .text_color(palette.text_faint)
                        .text_size(px(metrics::TEXT_XS))
                        .child(SharedString::from(if summary.total == 0 {
                            "No children recorded".into()
                        } else {
                            format!(
                                "Largest children · {} directly inside",
                                format::count(summary.total as u64)
                            )
                        })),
                );
                for &id in &summary.children {
                    let child = self.tree.node(id);
                    let fraction = if node.allocated == 0 {
                        0.0
                    } else {
                        (child.allocated as f64 / node.allocated as f64).clamp(0.0, 1.0) as f32
                    };
                    body = body.child(
                        v_flex()
                            .gap(px(3.0))
                            .child(
                                h_flex()
                                    .gap(px(10.0))
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis_middle()
                                            .child(SharedString::from(format!(
                                                "{}{}",
                                                format::printable(&self.tree.name_lossy(id)),
                                                if child.is_dir() { "/" } else { "" }
                                            ))),
                                    )
                                    .child(div().flex_none().text_color(palette.text_dim).child(
                                        SharedString::from(format::bytes_in(
                                            child.allocated,
                                            self.units,
                                        )),
                                    )),
                            )
                            .child(
                                div().h(px(2.0)).w_full().bg(palette.hairline).child(
                                    div()
                                        .h_full()
                                        .w(relative(fraction))
                                        .bg(palette.accent.opacity(0.55)),
                                ),
                            ),
                    );
                }
                if summary.total > summary.children.len() {
                    body = body.child(
                        div()
                            .text_color(palette.text_faint)
                            .text_size(px(metrics::TEXT_XS))
                            .child(SharedString::from(format!(
                                "+{} more",
                                format::count((summary.total - summary.children.len()) as u64)
                            ))),
                    );
                }
            } else {
                body = body.child(
                    div()
                        .text_color(palette.text_faint)
                        .child("Loading recorded children…"),
                );
            }
        }
        if node.flags.contains(Flags::PARTIAL) || node.flags.contains(Flags::UNREADABLE) {
            body = body.child(
                div()
                    .text_color(palette.caution)
                    .whitespace_normal()
                    .child("Some contents were not read; sizes are a lower bound."),
            );
        }
        body.child(
            div()
                .text_color(palette.text_faint)
                .text_size(px(metrics::TEXT_XS))
                .whitespace_normal()
                .child(if node.is_dir() {
                    "Click to browse · Right-click for actions"
                } else {
                    "Double-click to open · Right-click for actions"
                }),
        )
    }
}

pub(crate) fn tooltip(
    tree: Arc<SizeTree>,
    id: NodeId,
    cache: Arc<Cache>,
    palette: Palette,
    units: format::Units,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let preview = cx.new(|cx| Preview::new(tree, id, cache, palette, units, cx));
    Tooltip::element(move |_, _| preview.clone())
        .max_w(px(360.0))
        .whitespace_normal()
        .build(window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kosha_core::Scanner;

    fn fixture() -> (tempfile::TempDir, Arc<SizeTree>) {
        let root = tempfile::tempdir().unwrap();
        for n in 0..12 {
            std::fs::write(root.path().join(format!("file-{n}.bin")), vec![0; n * 4096]).unwrap();
        }
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("nested/large.bin"), vec![0; 100_000]).unwrap();
        let tree = Arc::new(Scanner::new(Default::default()).scan(root.path()).unwrap());
        (root, tree)
    }

    #[test]
    fn preview_matches_full_size_order_but_keeps_only_immediate_children() {
        let (_root, tree) = fixture();
        let root = NodeId::from_index(0);
        let actual = summarize(&tree, root, &AtomicBool::new(false)).unwrap();
        assert_eq!(actual.children, tree.children_by_size(root)[..CHILDREN]);
        assert_eq!(actual.total, 13);
        assert!(actual.children.iter().all(|id| tree.node(*id).parent == 0));
    }

    #[test]
    fn a_million_entries_do_not_expand_the_retained_selection() {
        let started = std::time::Instant::now();
        let result = largest(0..1_000_000, &AtomicBool::new(false)).unwrap();
        eprintln!(
            "Synthetic one-million-key selection: {:?}; {} retained candidates",
            started.elapsed(),
            result.len()
        );
        assert_eq!(
            result,
            vec![999_999, 999_998, 999_997, 999_996, 999_995, 999_994]
        );
    }

    #[test]
    fn empty_and_short_inputs_are_not_padded_with_invented_children() {
        let cancelled = AtomicBool::new(false);
        assert_eq!(largest(0..0, &cancelled).unwrap(), Vec::<i32>::new());
        assert_eq!(largest([0, 3, 1], &cancelled).unwrap(), vec![3, 1, 0]);
        let root = tempfile::tempdir().unwrap();
        let tree = Scanner::new(Default::default()).scan(root.path()).unwrap();
        let preview = summarize(&tree, NodeId::from_index(0), &cancelled).unwrap();
        assert_eq!(preview.total, 0);
        assert!(preview.children.is_empty());
    }

    #[test]
    fn cancelled_previews_stop_within_one_measurement_chunk() {
        let cancelled = AtomicBool::new(false);
        let consumed = std::cell::Cell::new(0);
        let input = (0..10_000).inspect(|value| {
            consumed.set(consumed.get() + 1);
            if *value == 257 {
                cancelled.store(true, Ordering::Relaxed);
            }
        });
        assert!(largest(input, &cancelled).is_none());
        assert!(consumed.get() <= 513);
        assert!(largest(0..0, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn cache_is_bounded_and_late_old_results_cannot_pollute_a_new_scan() {
        let (_root, tree) = fixture();
        let mut store = Store::default();
        let cache = store.for_tree(&tree);
        for id in 0..100 {
            cache.insert(
                NodeId::from_index(id),
                Arc::new(Summary {
                    children: vec![],
                    total: 0,
                }),
            );
        }
        assert_eq!(cache.entries.lock().unwrap().len(), CACHED_FOLDERS);
        assert!(cache.get(NodeId::from_index(0)).is_none());
        assert!(cache.get(NodeId::from_index(99)).is_some());
        let (_next_root, next_tree) = fixture();
        let next = store.for_tree(&next_tree);
        cache.insert(
            NodeId::from_index(0),
            Arc::new(Summary {
                children: vec![],
                total: 999,
            }),
        );
        assert!(next.get(NodeId::from_index(0)).is_none());
        assert!(Arc::ptr_eq(&next, &store.for_tree(&next_tree)));
    }
}
