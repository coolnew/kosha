// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The query help panel, and the suggestion strip under the query box.
//!
//! Both are answers to the same problem: a query language nobody can discover
//! is a query language nobody uses. The panel is for "what can I even ask?",
//! the strip is for "that did not work, now what?".
//!
//! Everything in both is a complete query, and clicking one puts it in the box.
//! That is the fastest way to learn a small language — take a working line and
//! change one word — and it is why the catalogue is tested for every example
//! actually running.
//!
//! Clicking is the only way anything changes. Nothing here rewrites what is
//! being typed: a filter silently corrected to a different question would show
//! plausible results for something nobody asked, which is the one failure the
//! user has no way to notice.

use gpui_kit::base::{Input, h_flex, v_flex};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, relative,
};

use kosha_query::{SuggestionKind, help};

use crate::app::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

/// The strip of suggestions under the query box.
///
/// Returns nothing when there is nothing confident to say, which is most of the
/// time — a strip that always has an opinion is one people stop reading.
pub fn suggestions(
    app: &KoshaApp,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> Option<AnyElement> {
    let found = app.suggestions();
    if found.is_empty() {
        return None;
    }

    let mut chips = Vec::with_capacity(found.len());
    for (index, suggestion) in found.iter().enumerate() {
        let query = suggestion.query.clone();
        let accent = match suggestion.kind {
            SuggestionKind::Correction => palette.caution,
            SuggestionKind::Interpretation => palette.accent,
            SuggestionKind::Example => palette.text_faint,
        };
        chips.push(
            ui::chip(("suggestion", index))
                .gap(px(6.0))
                .bg(palette.raised)
                .border_1()
                .border_color(palette.hairline)
                .hover(|this| this.bg(palette.hover).border_color(accent))
                .child(
                    div()
                        .flex_none()
                        .font_family(app.mono())
                        .text_size(px(metrics::TEXT_SM))
                        .text_color(accent)
                        .child(SharedString::from(suggestion.query.clone())),
                )
                .child(
                    div()
                        .text_size(px(metrics::TEXT_XS))
                        .text_color(palette.text_faint)
                        .child(SharedString::from(suggestion.reason.clone())),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_query(query.clone(), window, cx);
                }))
                .into_any_element(),
        );
    }

    Some(
        h_flex()
            .id("suggestions")
            .w_full()
            .px(px(12.0))
            .py(px(6.0))
            .gap(px(6.0))
            .items_center()
            .flex_wrap()
            .bg(palette.surface)
            .border_b_1()
            .border_color(palette.hairline)
            .child(
                div()
                    .flex_none()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.text_faint)
                    .child("try"),
            )
            .children(chips)
            .into_any_element(),
    )
}

/// The help panel: a search box over the catalogue, and click-to-insert rows.
pub fn panel(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> AnyElement {
    let needle = app.help_search_text(cx);
    let topics = help::search(&needle, 40);

    let mut rows = Vec::with_capacity(topics.len());
    for (index, topic) in topics.iter().enumerate() {
        rows.push(row(
            (
                topic.kind.label(),
                topic.label,
                topic.summary,
                topic.example,
            ),
            index,
            palette,
            cx,
        ));
    }
    if let Some(book) = app.recipe_book() {
        let label = book
            .metadata()
            .map_or("Your recipes", |pack| pack.title.as_str());
        for recipe in book.matching_recipes(&needle).take(40) {
            rows.push(row(
                ("recipe", &recipe.phrase, label, &recipe.dsl),
                rows.len(),
                palette,
                cx,
            ));
        }
    }
    let needle = needle.to_lowercase();
    for group in kosha_search::BUILTIN_RECIPE_GROUPS {
        for &(phrase, dsl) in group.recipes {
            if needle.split_whitespace().all(|word| {
                phrase.contains(word)
                    || group.label.to_lowercase().contains(word)
                    || dsl.to_lowercase().contains(word)
            }) {
                rows.push(row(
                    ("recipe", phrase, group.label, dsl),
                    rows.len(),
                    palette,
                    cx,
                ));
            }
        }
    }

    ui::scrim("help-scrim", palette)
        .items_center()
        .justify_center()
        .on_click(cx.listener(|this, _, window, cx| this.close_help(window, cx)))
        .child(
            ui::dialog("help-panel", palette)
                .w(px(620.0))
                .max_w(relative(0.96))
                .h(px(640.0))
                .max_h(relative(0.9))
                .child(
                    h_flex()
                        .flex_none()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_LG))
                                .text_color(palette.text)
                                .child("What can I search for?"),
                        )
                        .child(ui::close("help-close", palette, "Close  (Esc)").on_click(
                            cx.listener(|this, _, window, cx| this.close_help(window, cx)),
                        )),
                )
                .child(
                    div()
                        .text_size(px(metrics::TEXT_SM))
                        .text_color(palette.text_dim)
                        .child(
                            "Search in your own words — \"big\", \"old\", \"video\", \
                             \"folder\" — then click an example to use it.",
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .mt(px(4.0))
                        .px(px(10.0))
                        .py(px(4.0))
                        .gap(px(8.0))
                        .items_center()
                        .rounded(px(metrics::RADIUS_SM))
                        .bg(palette.surface)
                        .text_color(palette.text)
                        .border_1()
                        .border_color(palette.hairline)
                        .child(
                            div()
                                .flex_none()
                                .text_color(palette.text_faint)
                                .child("\u{2315}"),
                        )
                        .child(div().flex_1().child(Input::new(app.help_search()))),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_hidden()
                        .child(
                            v_flex()
                                .id("help-list")
                                .h_full()
                                .pr(px(12.0))
                                .mt(px(4.0))
                                .gap(px(1.0))
                                .overflow_y_scroll()
                                .track_scroll(app.help_scroll())
                                .when(rows.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .p(px(12.0))
                                            .text_size(px(metrics::TEXT_SM))
                                            .text_color(palette.text_faint)
                                            .child(
                                                "Nothing matches that. Try a plainer word — \
                                         \"big\", \"old\", \"name\", \"folder\".",
                                            ),
                                    )
                                })
                                .children(rows),
                        )
                        .child(
                            div().absolute().inset_0().child(
                                Scrollbar::vertical(app.help_scroll())
                                    .id("help-scrollbar")
                                    .viewport_from_layout()
                                    .mode(ScrollbarMode::Always),
                            ),
                        ),
                ),
        )
        .into_any_element()
}

/// One topic: what it is, what it means, and a query you can click.
fn row(
    (kind, label, summary, query): (&str, &str, &str, &str),
    index: usize,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let example = query.to_string();

    v_flex()
        .id(("help-topic", index))
        .flex_none()
        .cursor_pointer()
        .p(px(8.0))
        .gap(px(4.0))
        .rounded(px(metrics::RADIUS_SM))
        .hover(|this| this.bg(palette.hover))
        .child(
            h_flex()
                .gap(px(8.0))
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .w(px(56.0))
                        .text_size(px(metrics::TEXT_XS))
                        // One neutral for all five kinds. There are five and
                        // the palette has four colours that mean something, so
                        // the fifth had been taken from `category` — the
                        // treemap's array, where that exact hue means "this
                        // tile is documents". Chrome is neutral here so that a
                        // colour means something when it appears, and five
                        // hues telling you what a word already says is the
                        // decoration that rule exists to keep out.
                        .text_color(palette.text_faint)
                        .child(SharedString::from(kind.to_owned())),
                )
                .child(
                    div()
                        .text_color(palette.text)
                        .text_size(px(metrics::TEXT))
                        .child(SharedString::from(label.to_owned())),
                )
                .child(
                    div()
                        .ml_auto()
                        .text_size(px(metrics::TEXT_XS))
                        .text_color(palette.text_faint)
                        .child("click to use"),
                ),
        )
        .child(
            h_flex()
                .gap(px(8.0))
                .items_start()
                .child(div().flex_none().w(px(56.0)).child(""))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(metrics::TEXT_SM))
                        .text_color(palette.text_dim)
                        .child(SharedString::from(summary.to_owned())),
                ),
        )
        .child(
            h_flex()
                .gap(px(8.0))
                .child(div().flex_none().w(px(56.0)).child(""))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .whitespace_normal()
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(px(metrics::RADIUS_XS))
                        .bg(palette.surface)
                        .border_1()
                        .border_color(palette.hairline)
                        .text_size(px(metrics::TEXT_SM))
                        // The accent, which is what a query is drawn in
                        // everywhere else: the result count, the resume chip,
                        // a saved search in the command palette. This is the
                        // one thing on the row worth colouring, because it is
                        // the one thing a click puts in the box.
                        .text_color(palette.accent)
                        .child(SharedString::from(example.clone())),
                ),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.apply_query(example.clone(), window, cx);
            this.close_help(window, cx);
        }))
        .into_any_element()
}
