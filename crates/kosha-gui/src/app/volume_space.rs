// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use kosha_core::format;
use kosha_core::volume_space::{SpaceGap, SpaceSummary};

use super::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

impl KoshaApp {
    pub(super) fn current_volume_space(&self) -> Option<&SpaceSummary> {
        let ready = self.state.ready()?;
        let (tree, summary) = self.volume_space.as_ref()?;
        (!ready.source.is_past() && Arc::ptr_eq(&tree.upgrade()?, &ready.tree)).then_some(summary)
    }

    pub(super) fn volume_space_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.folder_comparison.is_some()
            || self.volume_history.is_some()
            || self.volume_space_open
        {
            return None;
        }
        self.current_volume_space()?;
        Some(
            super::clickable_in_title_bar(ui::button("volume-space", palette))
                .child("Volume space")
                .on_click(cx.listener(|app, _, _, cx| {
                    app.volume_space_open = true;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }
}

pub(super) fn render(
    summary: &SpaceSummary,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let value = |bytes: Option<u64>| bytes.map_or_else(|| "Unavailable".to_owned(), format::bytes);
    let row = |label: &'static str, amount: String| {
        h_flex()
            .justify_between()
            .gap(px(16.))
            .child(div().text_color(palette.text_dim).child(label))
            .child(div().flex_shrink_0().child(amount))
    };
    let card = || {
        v_flex()
            .w_full()
            .gap(px(10.))
            .p(px(16.))
            .rounded(px(metrics::RADIUS_SM))
            .bg(palette.surface)
            .border_1()
            .border_color(palette.hairline)
    };
    let scan_label = match (summary.estimated, summary.partial) {
        (true, true) => "Scanned allocation (partial estimate)",
        (true, false) => "Scanned allocation (estimated)",
        (false, true) => "Scanned allocation (partial)",
        (false, false) => "Scanned allocation",
    };
    let summary_panel = v_flex()
        .w_full()
        .max_w(px(640.))
        .gap(px(14.))
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(metrics::scaled(metrics::TEXT_XL))
                        .child("Volume space"),
                )
                .child(
                    ui::button("volume-space-close", palette)
                        .child("Back to scan")
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.volume_space_open = false;
                            cx.notify();
                        })),
                ),
        )
        .child(
            card()
                .child(row(
                    SpaceSummary::usage_label(),
                    value(summary.reported_used()),
                ))
                .child(row(scan_label, value(summary.scanned_bytes)))
                .children(summary.scanned_trash_bytes.map(|bytes| {
                    row(
                        "Volume Trash folders (included in scan)",
                        value(Some(bytes)),
                    )
                }))
                .child(row("Free", value(summary.usage.map(|usage| usage.free))))
                .child(row(
                    "Available to you",
                    value(summary.usage.map(|usage| usage.available)),
                ))
                .child(row(
                    "Unexplained",
                    value(match summary.gap {
                        SpaceGap::Unexplained(bytes) => Some(bytes),
                        _ => None,
                    }),
                ))
                .child(
                    div()
                        .border_t_1()
                        .border_color(palette.hairline)
                        .pt(px(10.))
                        .text_color(palette.text_dim)
                        .child(summary.gap.explanation()),
                ),
        )
        .child(
            card()
                .child("Scan coverage")
                .child(row("Unreadable entries", summary.unreadable.to_string()))
                .child(row(
                    "Other mounts skipped",
                    summary.skipped_mounts.to_string(),
                ))
                .children(summary.filesystem_snapshot_count.map(|count| {
                    row(
                        "Filesystem snapshots",
                        if count == 0 {
                            "0".to_owned()
                        } else {
                            format!("{count} · space unavailable")
                        },
                    )
                }))
                .children(
                    summary
                        .depth_limited
                        .then_some("The scan stopped at its depth limit."),
                ),
        );
    v_flex()
        .id("volume-space-panel")
        .size_full()
        .overflow_y_scroll()
        .items_center()
        .p(px(20.))
        .child(summary_panel)
        .into_any_element()
}
