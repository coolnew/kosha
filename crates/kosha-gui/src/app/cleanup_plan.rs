// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use kosha_analyze::{Insight, Suggested};
use kosha_core::{NodeId, SizeTree, Strategy, format};
use kosha_fsops::Action;

use super::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

pub(super) struct Draft {
    tree: Arc<SizeTree>,
    action: Action,
    findings: Vec<Finding>,
}

struct Finding {
    title: String,
    nodes: Vec<NodeId>,
}

impl Draft {
    pub(super) fn len(&self) -> usize {
        self.findings.len()
    }

    fn position(&self, insight: &Insight) -> Option<usize> {
        self.findings
            .iter()
            .position(|entry| entry.title == insight.title && entry.nodes == insight.nodes)
    }

    pub(super) fn contains(&self, insight: &Insight) -> bool {
        finding_action(insight) == Some(self.action) && self.position(insight).is_some()
    }
}

fn finding_action(insight: &Insight) -> Option<Action> {
    match insight.action {
        Suggested::Trash => Some(Action::Trash),
        Suggested::EmptyContents => Some(Action::EmptyContents),
        Suggested::Compress if kosha_core::supports_compression() => Some(Action::Compress),
        Suggested::ReviewOnly | Suggested::Compress => None,
    }
}

impl KoshaApp {
    pub(super) fn can_draft_cleanup(&self) -> bool {
        self.state.ready().is_some_and(|ready| {
            !ready.source.is_past()
                && !matches!(
                    ready.tree.stats().strategy,
                    Strategy::DiskImage | Strategy::Imported
                )
        })
    }

    pub(super) fn current_cleanup_draft(&self) -> Option<&Draft> {
        let ready = self.state.ready()?;
        self.cleanup_draft
            .as_ref()
            .filter(|draft| self.can_draft_cleanup() && Arc::ptr_eq(&draft.tree, &ready.tree))
    }

    pub(super) fn toggle_cleanup_finding(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.can_draft_cleanup()
            || self.planning_action.is_some()
            || self.state.pending.is_some()
        {
            return;
        }
        let Some(ready) = self.state.ready() else {
            return;
        };
        let Some(insight) = ready.insights.as_ref().and_then(|items| items.get(index)) else {
            return;
        };
        let Some(action) = finding_action(insight).filter(|_| !insight.nodes.is_empty()) else {
            return;
        };
        let tree = Arc::clone(&ready.tree);
        if self
            .cleanup_draft
            .as_ref()
            .is_some_and(|draft| !Arc::ptr_eq(&draft.tree, &tree))
        {
            self.cleanup_draft = None;
        }
        let draft = self.cleanup_draft.get_or_insert_with(|| Draft {
            tree,
            action,
            findings: Vec::new(),
        });
        if draft.action != action {
            self.state.notice = Some(format!(
                "Current action: {}. Review the plan or remove its findings before adding a different action.",
                draft.action.label()
            ));
        } else if let Some(position) = draft.position(insight) {
            self.remove_cleanup_finding(position, cx);
            return;
        } else {
            draft.findings.push(Finding {
                title: insight.title.clone(),
                nodes: insight.nodes.clone(),
            });
            self.state.notice = None;
        }
        cx.notify();
    }

    fn remove_cleanup_finding(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.planning_action.is_some() || self.state.pending.is_some() {
            return;
        }
        if let Some(draft) = self.cleanup_draft.as_mut()
            && index < draft.findings.len()
        {
            draft.findings.remove(index);
            if draft.findings.is_empty() {
                self.cleanup_draft = None;
                self.cleanup_plan_open = false;
            }
            self.state.notice = None;
            cx.notify();
        }
    }

    fn preview_cleanup_draft(&mut self, cx: &mut Context<Self>) {
        if !self.state.mode().is_read_write() {
            return;
        }
        let Some(draft) = self.current_cleanup_draft() else {
            return;
        };
        self.prepare_cleanup(
            Arc::clone(&draft.tree),
            draft
                .findings
                .iter()
                .flat_map(|finding| finding.nodes.iter().copied())
                .collect(),
            draft.action,
            format!("Cleanup plan · {}", draft.action.label()),
            cx,
        );
    }
}

pub(super) fn render(
    draft: &Draft,
    read_write: bool,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let mut findings = Vec::with_capacity(draft.len());
    for (index, finding) in draft.findings.iter().enumerate() {
        findings.push(
            h_flex()
                .justify_between()
                .gap(px(12.))
                .p(px(12.))
                .rounded(px(metrics::RADIUS_SM))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.hairline)
                .child(
                    v_flex()
                        .min_w(px(0.))
                        .gap(px(4.))
                        .child(div().truncate().child(finding.title.clone()))
                        .child(
                            div()
                                .text_color(palette.text_dim)
                                .child(format::plural(finding.nodes.len(), "target")),
                        ),
                )
                .child(
                    ui::button(("cleanup-plan-remove", index), palette)
                        .flex_none()
                        .child("Remove")
                        .on_click(
                            cx.listener(move |app, _, _, cx| app.remove_cleanup_finding(index, cx)),
                        ),
                ),
        );
    }
    v_flex()
        .id("cleanup-plan")
        .size_full()
        .overflow_y_scroll()
        .items_center()
        .p(px(20.))
        .child(
            v_flex()
                .w_full()
                .max_w(px(640.))
                .gap(px(14.))
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(div().text_size(metrics::scaled(metrics::TEXT_XL)).child("Cleanup plan"))
                        .child(ui::button("cleanup-plan-close", palette).child("Back to scan").on_click(
                            cx.listener(|app, _, _, cx| {
                                app.cleanup_plan_open = false;
                                cx.notify();
                            })
                        )),
                )
                .child(div().child(format!("{} · {}", draft.action.label(), format::plural(draft.len(), "finding"))))
                .children(findings)
                .child(ui::note(palette, "The preview combines overlapping targets and shows the estimated size before you confirm."))
                .when(read_write, |panel| {
                    panel.child(ui::button("cleanup-plan-preview", palette)
                        .child("Preview cleanup…")
                        .on_click(cx.listener(|app, _, _, cx| app.preview_cleanup_draft(cx))))
                })
                .when(!read_write, |panel| {
                    panel.child(ui::note(palette, "Switch to Read-write in the title bar to preview cleanup."))
                }),
        )
        .into_any_element()
}
