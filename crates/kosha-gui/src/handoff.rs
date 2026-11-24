// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reviewable local handoffs. Typed tool execution lives in kosha-fsops;
//! no prompt is sent to an AI provider.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, relative,
};
use kosha_analyze::cleanup::{Guide, Workflow};
use kosha_core::SizeTree;

use crate::{
    app::KoshaApp,
    theme::{Palette, metrics},
    ui,
};

pub struct Panel {
    pub tree: Arc<SizeTree>,
    pub guides: Vec<Guide>,
    pub ai: Option<(String, String)>,
    pub include_names: bool,
    pub workflow: Option<Workflow>,
    pub historical: bool,
    pub scroll: gpui_kit::ScrollHandle,
    pub tool_plan: Option<kosha_fsops::tool_cleanup::UvPrunePlan>,
    pub tool_run: Option<CancelOnDrop>,
    pub tool_result: Option<String>,
    pub tool_finished: bool,
}

pub struct CancelOnDrop(pub Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub fn render(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> Option<AnyElement> {
    let panel = app.handoff.as_ref()?;
    let title = if panel.ai.is_some() {
        "Ask AI · review what you share"
    } else {
        "Cleanup guide · your recorded workflows"
    };
    let mut content = v_flex().gap(px(12.0));
    if panel.tool_run.is_some() {
        content = content.child(ui::note(palette, "uv cache prune is running offline. Stop terminates the process; work already completed cannot be rolled back."))
            .child(ui::button("tool-stop", palette).child("Stop cleanup")
                .on_click(cx.listener(|app, _, _, cx| app.close_handoff(cx))));
    } else if let Some(result) = &panel.tool_result {
        content = content.child(div().child(SharedString::from(result.clone())))
            .child(ui::note(palette, "Rescan to measure usage, including after failure or cancellation. No recovered-byte estimate is inferred from the tool's exit status."))
            .child(ui::button("tool-rescan", palette).child("Close and rescan")
                .on_click(cx.listener(|app, _, _, cx| app.finish_tool_handoff(cx))));
    } else if let Some(plan) = &panel.tool_plan {
        content = content
            .child(div().text_size(metrics::scaled(metrics::TEXT_LG)).child("Preview · prune unused uv cache"))
            .child(ui::note(palette, "Permanent removal by your trusted installed uv. No Trash or undo. No downloads, project config or shell. This does not empty the whole cache or promise specific savings."))
            .child(div().child(SharedString::from(format!("Executable: {}", kosha_core::json::escaped(&plan.executable().to_string_lossy())))))
            .child(ui::note(palette, SharedString::from(format!("BLAKE3: {}", plan.executable_digest()))))
            .child(div().child(SharedString::from(format!("Cache: {}", kosha_core::json::escaped(&plan.cache().to_string_lossy())))))
            .child(div().child("Arguments: --no-config --offline --no-progress cache prune --cache-dir <cache above>"))
            .child(ui::note(palette, "Only continue if you trust this executable. External code is not sandboxed. Cache and executable are rechecked and the operation is audited. Five-minute timeout; Stop cannot restore pruned files."))
            .child(ui::button("tool-confirm", palette).child("Confirm permanent uv cache prune")
                .on_click(cx.listener(|app, _, _, cx| app.confirm_uv_prune(cx))));
    } else if let Some((private, named)) = &panel.ai {
        content = content
            .child(ui::note(palette, "Nothing is sent to an AI. Review this prompt, copy it, then paste it into the assistant you choose. Paths may be sensitive; your clipboard may sync across devices."))
            .child(ui::button("handoff-names", palette)
                .child(if panel.include_names { "Hide names and paths" } else { "Include names and paths (preview)" })
                .on_click(cx.listener(|app, _, _, cx| {
                    if let Some(panel) = app.handoff.as_mut() { panel.include_names = !panel.include_names; }
                    cx.notify();
                })))
            .child(div().p(px(12.0)).rounded(px(metrics::RADIUS)).bg(palette.raised).text_color(palette.text)
                .child(SharedString::from(if panel.include_names { named.clone() } else { private.clone() })))
            .child(ui::button("handoff-copy", palette).child("Copy reviewed prompt")
                .on_click(cx.listener(|app, _, _, cx| {
                    if let Some(panel) = &app.handoff && let Some((private, named)) = &panel.ai {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(if panel.include_names { named.clone() } else { private.clone() }));
                        app.handoff_copied(cx);
                    }
                })));
    } else {
        content = content
            .child(ui::note(palette, "These locations suggest workflows, not who you are. Sizes describe recorded data, not promised savings. Use the owning application's controls and rescan afterwards. Custom locations may not be detected."))
            .child(h_flex().flex_wrap().gap(px(6.0))
                .children([None, Some(Workflow::Development), Some(Workflow::Browsing), Some(Workflow::Assistant)].into_iter().enumerate().map(|(index, workflow)| {
                    ui::button(("handoff-workflow", index), palette)
                        .when(panel.workflow == workflow, |button| button.border_color(palette.accent).text_color(palette.accent))
                        .child(workflow.map_or("All", Workflow::label))
                        .on_click(cx.listener(move |app, _, _, cx| {
                            if let Some(panel) = app.handoff.as_mut() { panel.workflow = workflow; panel.scroll.set_offset(Default::default()); }
                            cx.notify();
                        }))
                })));
        let matches: Vec<_> = panel
            .guides
            .iter()
            .filter(|guide| {
                panel
                    .workflow
                    .is_none_or(|workflow| guide.workflow == workflow)
            })
            .collect();
        if matches.is_empty() {
            content = content.child(ui::note(palette, "No supported locations recorded for this filter. Scan your home or cache directory, or install a review-only insight pack for custom locations. A missing match does not mean the application is unused."));
        }
        for (index, guide) in matches.into_iter().enumerate() {
            let id = guide.node;
            let tree = Arc::clone(&panel.tree);
            let documentation = guide.documentation;
            let can_execute = guide.uv_prune
                && !panel.historical
                && !panel.tree.is_incomplete()
                && app.handoff_writable(&panel.tree);
            content = content.child(
                v_flex()
                    .gap(px(6.0))
                    .p(px(12.0))
                    .rounded(px(metrics::RADIUS))
                    .bg(palette.raised)
                    .child(
                        div()
                            .text_size(metrics::scaled(metrics::TEXT_LG))
                            .child(guide.title),
                    )
                    .child(ui::note(
                        palette,
                        SharedString::from(format!(
                            "{} · {} recorded",
                            guide.workflow.label(),
                            kosha_core::format::bytes_in(
                                panel.tree.node(id).allocated,
                                app.units()
                            )
                        )),
                    ))
                    .child(div().text_color(palette.text_dim).child(guide.steps))
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(
                                ui::button(("handoff-show", index), palette)
                                    .child("Show location")
                                    .on_click(cx.listener(move |app, _, window, cx| {
                                        app.handoff_reveal(id, &tree, window, cx)
                                    })),
                            )
                            .child(
                                ui::button(("handoff-docs", index), palette)
                                    .child("Official instructions ↗")
                                    .on_click(move |_, _, cx| cx.open_url(documentation)),
                            ),
                    )
                    .when(guide.uv_prune, |card| {
                        card.child(
                            ui::button(("uv-preview", index), palette)
                                .child(if can_execute {
                                    "Preview uv cache prune…"
                                } else if kosha_core::elevated() {
                                    "uv prune (non-admin required)"
                                } else if panel.historical || panel.tree.is_incomplete() {
                                    "uv prune (live scan required)"
                                } else {
                                    "uv prune (Read-write required)"
                                })
                                .when(!can_execute, |button| button.text_color(palette.text_faint))
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    if can_execute {
                                        app.prepare_uv_prune(id, cx);
                                    }
                                })),
                        )
                    }),
            );
        }
        content = content.child(ui::note(palette, "Project build output remains in Insights beside its manifest evidence. Browser time ranges must be selected and confirmed in the browser. History, cookies, offline data and AI conversations can be unique; never assume they are disposable."));
    }
    Some(ui::scrim("handoff-scrim", palette).items_center().justify_center()
        .on_click(cx.listener(|app, _, _, cx| app.close_handoff(cx)))
        .child(ui::dialog("handoff-panel", palette).w(px(720.0)).max_w(relative(0.95)).max_h(relative(0.88))
            .child(h_flex().justify_between().items_center()
                .child(div().text_size(metrics::scaled(metrics::TEXT_LG)).child(title))
                .child(ui::close("handoff-close", palette, "Close (Esc)").on_click(cx.listener(|app, _, _, cx| app.close_handoff(cx)))))
            .when(panel.tool_finished, |dialog| dialog.child(ui::note(palette, "Cleanup may have changed disk contents. Closing this panel rescans before further file actions.")))
            .when(!panel.tool_finished && (panel.historical || panel.tree.is_partial() || panel.tree.is_incomplete()), |dialog| dialog.child(ui::note(palette, "Historical, incomplete or partially unreadable scan: recorded data may be stale or incomplete. Rescan before making decisions.")))
            .child(div().relative().flex_1().min_h(px(0.0)).overflow_hidden()
                .child(div().id("handoff-scroll").max_h(px(560.0)).overflow_y_scroll().track_scroll(&panel.scroll).pr(px(16.0)).child(content))
                .child(div().absolute().inset_0().child(Scrollbar::vertical(&panel.scroll).id("handoff-scrollbar").viewport_from_layout().mode(ScrollbarMode::Always)))))
        .into_any_element())
}
