// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use kosha_core::format;
use kosha_fsops::ExecutionReport;

use super::{KoshaApp, tooltip};
use crate::theme::{Palette, metrics};
use crate::ui;

pub(super) fn render(
    report: &ExecutionReport,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let row = |label, value| {
        h_flex()
            .justify_between()
            .gap(px(12.))
            .child(div().text_color(palette.text_dim).child(label))
            .child(div().flex_shrink_0().child(value))
    };
    let free = |id, label, bytes: Option<u64>| {
        h_flex()
            .justify_between()
            .gap(px(12.))
            .child(div().text_color(palette.text_dim).child(label))
            .child(
                div()
                    .id(id)
                    .flex_shrink_0()
                    .child(bytes.map_or_else(|| "Unavailable".into(), format::bytes))
                    .when_some(bytes, |value, bytes| {
                        value.tooltip(move |window, cx| {
                            tooltip(format!("{} bytes", format::count(bytes))).build(window, cx)
                        })
                    }),
            )
    };
    v_flex()
        .id("cleanup-receipt")
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
                        .child(
                            div()
                                .text_size(metrics::scaled(metrics::TEXT_XL))
                                .child("Cleanup receipt"),
                        )
                        .child(
                            ui::button("cleanup-receipt-close", palette)
                                .child("Back to scan")
                                .on_click(cx.listener(|app, _, _, cx| {
                                    app.cleanup_receipt_open = false;
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().child(report.measured_summary()))
                .children(
                    (report.unobserved_volume_targets > 0)
                        .then_some("Volume free-space counters were unavailable for some targets."),
                )
                .children(
                    report
                        .volume_changes
                        .iter()
                        .enumerate()
                        .map(|(index, change)| {
                            let delta = change.delta().map_or_else(
                                || "Unavailable".to_owned(),
                                |delta| {
                                    let sign = match delta.cmp(&0) {
                                        std::cmp::Ordering::Less => "−",
                                        std::cmp::Ordering::Equal => "",
                                        std::cmp::Ordering::Greater => "+",
                                    };
                                    format!("{sign}{}", format::bytes(delta.unsigned_abs() as u64))
                                },
                            );
                            let root = kosha_core::path::simplify(change.root.clone());
                            v_flex()
                                .w_full()
                                .gap(px(8.))
                                .p(px(16.))
                                .rounded(px(metrics::RADIUS_SM))
                                .bg(palette.surface)
                                .border_1()
                                .border_color(palette.hairline)
                                .child(
                                    div().child(
                                        format::printable(&root.to_string_lossy()).into_owned(),
                                    ),
                                )
                                .child(free(
                                    ("free-before", index),
                                    "Free before",
                                    change.before_free,
                                ))
                                .child(free(("free-after", index), "Free after", change.after_free))
                                .child(row("Observed change", delta))
                        }),
                )
                .children(
                    (!report.volume_changes.is_empty()).then_some(
                        div()
                            .text_color(palette.text_dim)
                            .child("Other disk activity can affect free space during cleanup."),
                    ),
                ),
        )
        .into_any_element()
}
