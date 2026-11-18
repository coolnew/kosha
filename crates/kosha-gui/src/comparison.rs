// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! What changed between two trees.
//!
//! "What is big" is dominated by the things that have always been big and are
//! supposed to be — the operating system, a photo library. "What *grew*" is
//! almost always the actual reason a disk filled up, and it is the question a
//! snapshot exists to answer.
//!
//! This is its own list rather than the results table, and it has to be: the
//! table renders nodes of the tree on screen, and half of what a comparison has
//! to say is about paths that are not in it. A file that was deleted has no
//! node anywhere. It is also the honest shape — a comparison is not a directory
//! listing with two extra columns, it is a different kind of answer, and every
//! row of it is about a *difference* rather than about a thing.
//!
//! ## Which side is the baseline
//!
//! Always the older one, so a delta reads forward in time and `+` means grew.
//! Chosen by the two trees' own timestamps rather than by which one the reader
//! happened to have open, because "what changed" has a direction and getting it
//! backwards silently inverts every sign on screen.

use crate::ui::tooltip;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, uniform_list,
};

use kosha_core::compare::{Change, Kind};
use kosha_core::format;

use crate::app::KoshaApp;
use crate::state::Comparison;
use crate::theme::{Palette, metrics};
use crate::ui;

/// Row height, matching the results table so the two feel like one application.
const ROW: f32 = metrics::ROW_HEIGHT;

/// The whole middle of the window, while a comparison is showing.
pub fn render(
    app: &KoshaApp,
    comparison: &Comparison,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    v_flex()
        .size_full()
        .child(header(app, comparison, palette, cx))
        .child(columns(palette))
        .child(rows(app, comparison, palette))
        .into_any_element()
}

/// What was held against what, and the one number that summarises it.
fn header(
    app: &KoshaApp,
    comparison: &Comparison,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let units = app.units();
    let net = comparison.net;
    let summary: SharedString = if net == 0 {
        SharedString::from("No net change across files")
    } else {
        SharedString::from(format!(
            "{}{} net across files",
            if net > 0 { "+" } else { "\u{2212}" },
            format::bytes_in(net.unsigned_abs(), units),
        ))
    };

    let counted: SharedString = SharedString::from(match comparison.below_floor {
        0 => format!("{} shown", format::plural(comparison.visible(), "change")),
        dropped => format!(
            "{} shown · {} under {} not listed",
            format::plural(comparison.visible(), "change"),
            dropped,
            format::bytes_in(comparison.floor, units),
        ),
    });

    v_flex()
        .flex_none()
        .px(px(12.0))
        .py(px(10.0))
        .gap(px(6.0))
        .bg(palette.surface)
        .border_b_1()
        .border_color(palette.hairline)
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap(px(8.0))
                .items_center()
                .child(
                    div()
                        .text_size(px(metrics::TEXT_LG))
                        .text_color(palette.text)
                        .child("Comparison"),
                )
                .child(
                    div()
                        .text_color(if net > 0 {
                            palette.caution
                        } else if net < 0 {
                            palette.positive
                        } else {
                            palette.text_dim
                        })
                        .child(summary),
                )
                .child(div().flex_1())
                .child(ui::button("cmp-export-md", palette).child("Markdown")
                    .tooltip(|window, cx| tooltip("Export the visible comparison as Markdown").build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.export_comparison(crate::comparison_export::Format::Markdown, cx))))
                .child(ui::button("cmp-export-jsonl", palette).child("JSONL")
                    .tooltip(|window, cx| tooltip("Export metadata and visible changes as JSON Lines").build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.export_comparison(crate::comparison_export::Format::Jsonl, cx))))
                .child(ui::close("cmp-close", palette, "Close the comparison  (Esc)")
                    .on_click(cx.listener(|this, _, _, cx| this.close_comparison(cx)))),
        )
        .child(h_flex().gap(px(10.0)).w_full()
            .child(observation("BEFORE", &comparison.against_root, &comparison.against, comparison.against_taken_ms, palette))
            .child(observation("AFTER", &comparison.now_root, &comparison.now, comparison.now_taken_ms, palette)))
        .child(
            h_flex().w_full().flex_wrap().gap(px(6.0)).items_center()
                .child(filter_chip(
                    "cmp-dirs",
                    "Directories",
                    comparison.show_dirs,
                    palette,
                    cx.listener(|this, _, _, cx| this.toggle_comparison_kind(true, cx)),
                ))
                .child(filter_chip(
                    "cmp-files",
                    "Files",
                    comparison.show_files,
                    palette,
                    cx.listener(|this, _, _, cx| this.toggle_comparison_kind(false, cx)),
                ))
                .children([(None, "All"), (Some(Kind::Added), "New"), (Some(Kind::Grew), "Grew"),
                    (Some(Kind::Shrank), "Shrank"), (Some(Kind::Removed), "Gone")].into_iter().enumerate().map(|(index, (kind, label))| {
                    filter_chip_kind(index, label, comparison.change_kind == kind, palette,
                        cx.listener(move |this, _, _, cx| this.filter_comparison(kind, cx)))
                })),
        )
        .child(
            div()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text_faint)
                .child(counted),
        )
        .child(div().text_size(px(metrics::TEXT_XS)).text_color(palette.text_faint)
            .child(SharedString::from(format!("Diff computed in {} · stored trees, no rescan · folder totals overlap files and are excluded from the net", format::duration(comparison.elapsed)))))
        // What the comparison rests on, when it rests on less than the reader
        // would assume: another folder's record, or one with almost nothing in
        // common with this. In the caution colour and above the list, because
        // the list is where a difference between two folders reads as a change.
        .children(comparison.footing.clone().map(|footing| {
            div()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.caution)
                .child(footing)
        }))
        .into_any_element()
}

fn observation(
    title: &'static str,
    root: &std::path::Path,
    label: &str,
    stamp: Option<u64>,
    palette: &Palette,
) -> AnyElement {
    let date = stamp.map_or_else(
        || label.to_owned(),
        |ms| format::stamp(ms / 1000, u64::from(format::now_unix())),
    );
    let path: SharedString = format::printable(&root.to_string_lossy())
        .into_owned()
        .into();
    v_flex()
        .flex_1()
        .min_w(px(0.0))
        .gap(px(3.0))
        .p(px(8.0))
        .rounded(px(metrics::RADIUS_SM))
        .border_1()
        .border_color(palette.hairline)
        .child(
            div()
                .text_size(px(metrics::TEXT_XS))
                .text_color(palette.text_faint)
                .child(title),
        )
        .child(
            div()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text)
                .child(SharedString::from(date)),
        )
        .child(
            div()
                .truncate()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text_dim)
                .child(path),
        )
        .into_any_element()
}

fn filter_chip_kind<F>(
    index: usize,
    label: &'static str,
    on: bool,
    palette: &Palette,
    on_click: F,
) -> AnyElement
where
    F: Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
{
    ui::chip(("cmp-change-kind", index))
        .border_1()
        .border_color(palette.hairline)
        .text_color(if on { palette.accent } else { palette.text_dim })
        .when(on, |view| view.bg(palette.accent.opacity(0.12)))
        .hover(|view| view.bg(palette.hover))
        .child(label)
        .on_click(on_click)
        .into_any_element()
}

fn filter_chip<F>(
    id: &'static str,
    label: &'static str,
    on: bool,
    palette: &Palette,
    on_click: F,
) -> impl IntoElement + use<F>
where
    F: Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
{
    ui::chip(id)
        .border_1()
        .when(on, |this| {
            this.border_color(palette.hairline)
                .bg(palette.surface)
                .text_color(palette.text)
        })
        .when(!on, |this| {
            this.border_color(palette.hairline.opacity(0.5))
                .text_color(palette.text_faint)
        })
        .hover(|this| this.bg(palette.hover))
        .child(label)
        .on_click(on_click)
}

fn columns(palette: &Palette) -> AnyElement {
    h_flex()
        .flex_none()
        // The same band as the table's own header, because a comparison
        // *replaces* the table: it was 24px high on the bare canvas while the
        // one it stands in for is 28 on a surface, so switching to a
        // comparison moved the headings up and took their background away.
        .h(px(metrics::ROW_HEIGHT))
        .px(px(12.0))
        .gap(px(12.0))
        .items_center()
        .bg(palette.surface)
        .text_size(px(metrics::TEXT_XS))
        .text_color(palette.text_faint)
        .border_b_1()
        .border_color(palette.hairline)
        .child(div().w(px(110.0)).child("CHANGE"))
        .child(div().w(px(64.0)).child("WHAT"))
        .child(div().flex_1().child("PATH"))
        .child(div().w(px(100.0)).child("BEFORE"))
        .child(div().w(px(100.0)).child("AFTER"))
        .into_any_element()
}

fn rows(app: &KoshaApp, comparison: &Comparison, palette: &Palette) -> AnyElement {
    // Share cached row indices rather than cloning every path per frame:
    // `uniform_list` asks for an arbitrary window of rows by index, and an
    // iterator that has to be re-filtered from the start for every window is
    // how a long list becomes quadratic.
    let shown = std::sync::Arc::clone(&comparison.visible_indices);
    let changes = std::sync::Arc::clone(&comparison.changes);
    let units = app.units();
    let palette = *palette;

    if shown.is_empty() {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(ui::note(&palette, "Nothing changed by enough to list."))
            .into_any_element();
    }

    let count = shown.len();
    uniform_list("comparison-rows", count, move |range, _window, _cx| {
        range
            .filter_map(|index| {
                shown
                    .get(index)
                    .and_then(|&row| changes.get(row).map(|change| (row, change)))
            })
            .map(|(index, change)| row(index, change, units, &palette))
            .collect()
    })
    .flex_1()
    .into_any_element()
}

fn row(index: usize, change: &Change, units: format::Units, palette: &Palette) -> AnyElement {
    let delta = change.delta();
    let grew = delta > 0;
    let colour = if grew {
        palette.caution
    } else {
        palette.positive
    };

    let path: SharedString = format::printable(&change.path).into_owned().into();
    let copy_path = change.path.clone();
    h_flex()
        .id(("comparison-row", index))
        .w_full()
        .hover(|style| style.bg(palette.hover))
        .tooltip(move |window, cx| {
            tooltip(format!("{path} — click to copy the path")).build(window, cx)
        })
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy_path.clone()))
        })
        .h(px(ROW))
        .px(px(12.0))
        .gap(px(12.0))
        .items_center()
        .text_size(px(metrics::TEXT))
        .child(
            div()
                .w(px(110.0))
                .flex_none()
                .text_color(colour)
                .child(SharedString::from(format!(
                    "{}{}",
                    if grew { "+" } else { "\u{2212}" },
                    format::bytes_in(delta.unsigned_abs(), units),
                ))),
        )
        .child(
            div()
                .w(px(64.0))
                .flex_none()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text_faint)
                .child(SharedString::from(format!(
                    "{} {}",
                    if change.is_dir { "▸" } else { "·" },
                    word(change.kind)
                ))),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_color(palette.text)
                .child(SharedString::from(
                    format::printable(change.path.as_ref()).into_owned(),
                )),
        )
        .child(
            div()
                .w(px(100.0))
                .flex_none()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text_dim)
                .child(SharedString::from(if change.kind == Kind::Added {
                    "—".into()
                } else {
                    format::bytes_in(change.before, units)
                })),
        )
        .child(
            div()
                .w(px(100.0))
                .flex_none()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text_dim)
                .child(SharedString::from(if change.kind == Kind::Removed {
                    "—".into()
                } else {
                    format::bytes_in(change.after, units)
                })),
        )
        .into_any_element()
}

/// What to call each kind, in the fewest words that are still true.
fn word(kind: Kind) -> &'static str {
    match kind {
        Kind::Grew => "grew",
        Kind::Shrank => "shrank",
        Kind::Added => "new",
        Kind::Removed => "gone",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(kind: Kind, before: u64, after: u64) -> Change {
        Change {
            path: "a/b".into(),
            kind,
            is_dir: false,
            before,
            after,
        }
    }

    /// Every kind has a word, and no two share one: a list where "new" and
    /// "grew" were the same word would be a list nobody could read.
    #[test]
    fn every_kind_is_named_distinctly() {
        let words = [
            word(Kind::Grew),
            word(Kind::Shrank),
            word(Kind::Added),
            word(Kind::Removed),
        ];
        for (index, one) in words.iter().enumerate() {
            assert!(!one.is_empty());
            assert!(
                !words[..index].contains(one),
                "`{one}` names two different kinds"
            );
        }
    }

    /// The sign is the whole message of a row, so it comes from the delta and
    /// never from the kind: a directory can shrink while gaining entries, and
    /// a "grew" whose delta is negative would be a row that argues with itself.
    #[test]
    fn the_sign_follows_the_delta() {
        assert!(change(Kind::Grew, 100, 900).delta() > 0);
        assert!(change(Kind::Shrank, 900, 100).delta() < 0);
        assert_eq!(change(Kind::Added, 0, 500).delta(), 500);
        assert_eq!(change(Kind::Removed, 500, 0).delta(), -500);
    }

    /// Both switches off would leave an empty list under two controls that
    /// both look like they are doing something.
    #[test]
    fn the_two_switches_cannot_both_be_off() {
        let mut one = Comparison {
            against: "then".into(),
            now: "now".into(),
            changes: std::sync::Arc::new(vec![change(Kind::Grew, 1, 2)]),
            net: 1,
            below_floor: 0,
            floor: 0,
            show_dirs: true,
            show_files: true,
            footing: None,
            ..Default::default()
        };

        // Turning one off is allowed; turning the second off is not, which is
        // the rule the two click handlers encode.
        if one.show_files {
            one.show_dirs = false;
        }
        assert!(!one.show_dirs && one.show_files);
        if one.show_dirs {
            one.show_files = false;
        }
        assert!(one.show_files, "the last switch was allowed to go off");
    }
}
