// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The handful of shapes every panel is built out of.
//!
//! Not a widget library. These are the four or five recipes that were already
//! written out by hand in six different files, each time with slightly
//! different numbers — a scrim at 0.45 in four places and 0.35 in the fifth,
//! buttons with five different paddings, a dialog radius of 12 spelled out in
//! every overlay. None of those differences was a decision. They are what
//! happens when the next panel is written from memory instead of from a
//! neighbour, and they are invisible one at a time and obvious side by side.
//!
//! So the shapes live here and the numbers live in [`metrics`]. A new panel
//! then looks like the others because it is made of the same parts, rather than
//! because whoever wrote it remembered.
//!
//! **What is deliberately not here.** Anything a single panel does once. The
//! treemap's tiles, the table's rows and the title bar's controls each have one
//! author and one call site, and pulling them in would make this the file
//! everything depends on and nothing can change — which is the failure mode of
//! every shared-components module that ever grew past its usefulness.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    Div, InteractiveElement, IntoElement, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::theme::{Palette, metrics};

/// Keep the component library's themed tooltip shell while bounding its text.
/// A plain text child uses GPUI's word wrapping, including its character-level
/// fallback for paths and other tokens wider than the available line.
pub fn tooltip(text: impl Into<SharedString>) -> Tooltip {
    let text = text.into();
    Tooltip::element(move |_, _| {
        div()
            .min_w(px(0.0))
            .max_w(px(344.0))
            .whitespace_normal()
            .child(text.clone())
    })
    .max_w(px(360.0))
    .whitespace_normal()
}

/// The dimmed backdrop behind anything modal.
///
/// The colour is the palette's rather than a constant, because one opacity of
/// black dims a light window and a dark one by wildly different amounts — see
/// [`Palette::scrim`].
///
/// Carries the click that dismisses — the caller attaches it — and nothing
/// else. The panel on top has to stop clicks reaching this, which every caller
/// does with `.on_click(|_, _, _| {})`; that is the one part of the recipe a
/// caller must remember, and it is why [`dialog`] exists to remember it.
pub fn scrim(id: &'static str, palette: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .bg(palette.scrim)
        // **A modal that lets clicks through is not modal.** Read in
        // `gpui::Window::hit_test`: every hitbox containing the pointer is
        // collected from the top down and all of them count as hovered — the
        // walk stops only at a hitbox whose behaviour is `BlockMouse`, which is
        // what `occlude` sets and which nothing in this application had. So a
        // click on the dimmed area reached whatever was under it: an insight's
        // action button, a treemap tile, a row. With a plan on screen waiting
        // to be approved, that means the plan could be replaced by another one
        // under the reader's hand.
        //
        // Here rather than on each panel, because the scrim is what "there is
        // something in front of the window" is made of, and a panel that
        // forgot would be a modal with a hole in it.
        .occlude()
}

/// A panel floating over the window: the one elevation this app has.
///
/// A hairline and a raised surface give the panel subtle depth.
/// Swallows its own clicks so that clicking inside a dialog does
/// not dismiss it through the scrim underneath, which is a bug every one of
/// these panels would otherwise have to avoid on its own.
pub fn dialog(id: &'static str, palette: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .p(px(metrics::PANEL_PADDING))
        .gap(px(8.0))
        .rounded(px(metrics::RADIUS_LG))
        .bg(palette.raised)
        .border_1()
        .border_color(palette.hairline)
        .on_click(|_, _, cx| cx.stop_propagation())
}

/// The shape of a button, without any of its colours.
///
/// Split out because **GPUI panics if `hover` is set twice on one element** —
/// `hover style already set`, at render, with nothing said at compile time. So
/// the two public builders below cannot be written as one calling the other and
/// adding its own hover, which is exactly how they were written first and
/// exactly how far that got: the app came up, drew one frame, and died.
///
/// The height is stated here rather than left to padding. Every hit target
/// must be at least 28px tall, and several were not:
/// padding of 3 or 5 around a 12px label lands well short, and nothing about
/// reading the code would tell you so.
fn shell(id: impl Into<gpui_kit::ElementId>) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(metrics::HIT_TARGET))
        .px(px(metrics::CARD_PADDING))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .rounded(px(metrics::RADIUS_SM))
        .border_1()
        .cursor_pointer()
        .text_size(px(metrics::TEXT_SM))
}

/// What a button looks like when it is not asking for anything.
pub fn button(id: impl Into<gpui_kit::ElementId>, palette: &Palette) -> Stateful<Div> {
    shell(id)
        .border_color(palette.hairline)
        .bg(palette.surface)
        .text_color(palette.text_dim)
        .hover(|this| this.bg(palette.hover).text_color(palette.text))
}

/// The one button on a panel that is the reason the panel is open.
///
/// Accent-filled, or `danger` when what it does cannot be undone — the only
/// two fills a button is allowed, because colour is reserved for meaning
/// and a second accent-coloured button on the same panel
/// would make neither of them the answer.
pub fn primary(
    id: impl Into<gpui_kit::ElementId>,
    palette: &Palette,
    irreversible: bool,
) -> Stateful<Div> {
    shell(id)
        .border_color(if irreversible {
            palette.danger
        } else {
            palette.accent
        })
        .bg(if irreversible {
            palette.danger
        } else {
            palette.accent
        })
        .text_color(palette.accent_text)
        .hover(|this| this.opacity(0.9))
}

/// A small control that sits in a row with others of its kind.
///
/// The segmented choices in the settings panel and the filter toggles above a
/// comparison. Shorter-looking than [`button`] because it carries less padding
/// either side, and exactly as tall, because the 28px minimum applies to the
/// clickable area regardless of appearance. Both were previously only 18px high.
///
/// The caller supplies the colours *and the hover*, because what they mean
/// differs: on and off are "the one selected" in a segmented choice and "this
/// is showing" in a filter, and an insight's action button has a third state —
/// present but inert in read-only mode, where a hover highlight would promise
/// something a click does not deliver.
pub fn chip(id: impl Into<gpui_kit::ElementId>) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(metrics::HIT_TARGET))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(metrics::RADIUS_SM))
        .cursor_pointer()
        .text_size(px(metrics::TEXT_SM))
}

/// One character you can click: `✕`, `?`, `☆`.
///
/// **The hint is not optional.** Glyph controls need tooltips to explain their
/// actions, wherever they appear. Taking the hint as an
/// argument is what stops the ninth being written without one: there is no way
/// to call this and forget.
///
/// The hover is the caller's, because on these it is the whole of what
/// distinguishes them — [`close`] and [`remove`] are this function plus the two
/// hovers that were worth naming.
///
/// **The size is stated here too**, for the same reason the height is. Nothing
/// set it before, so each mark came out at whatever its container happened to
/// be running at and three call sites had reached for an override: the same
/// `✕` was 11px in the snapshots rail, 12px in the settings panel and 13px in
/// four dialogs, and which one you got depended on whether whoever wrote it
/// thought to say. A glyph is an icon rather than prose, so it is one size
/// everywhere, and the one it is is the window's own.
///
/// They were six separate recipes in five files, none with a height, all
/// between fourteen and eighteen pixels tall.
pub fn glyph(
    id: impl Into<gpui_kit::ElementId>,
    palette: &Palette,
    hint: impl Into<SharedString>,
) -> Stateful<Div> {
    let hint = hint.into();
    div()
        .id(id.into())
        .flex_none()
        .h(px(metrics::HIT_TARGET))
        .px(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(metrics::RADIUS_SM))
        .cursor_pointer()
        .text_size(px(metrics::TEXT))
        .text_color(palette.text_faint)
        .tooltip(move |window, cx| tooltip(hint.clone()).build(window, cx))
}

/// The mark on a button that puts something away or takes it away.
///
/// U+2715 MULTIPLICATION X, not the letter x and not U+00D7: it is the one
/// drawn with even arms at the weight of the surrounding text. Spelled once,
/// because it had been spelled two ways — `"\u{2715}"` in eight places and a
/// literal `✕` in two — and a search for one of them found neither all of the
/// close buttons nor all of the delete buttons.
const MARK: &str = "\u{2715}";

/// `✕` meaning *put this away*: shut a panel, clear a search.
///
/// Neutral on hover, and it draws its own mark. Every one of the six is a plain
/// `✕` and always will be — a close button that closed with some other
/// character would be a different button — so the caller says where it goes and
/// what it closes, and not what it looks like.
///
/// There were five of these — the search row, a comparison, the owners panel,
/// the query help and the settings panel — in three radii, two paddings, with
/// only one of the five showing a pointer, and none of them with a height at
/// all.
pub fn close(
    id: impl Into<gpui_kit::ElementId>,
    palette: &Palette,
    hint: impl Into<SharedString>,
) -> Stateful<Div> {
    glyph(id, palette, hint)
        .hover(|this| this.bg(palette.hover).text_color(palette.text))
        .child(MARK)
}

/// `✕` meaning *take this away for good*: forget a saved search, unpin a
/// folder, delete a snapshot.
///
/// The same shape as [`close`] and a different hover, because they are
/// different promises and the glyph alone cannot say which. Red on hover.
///
/// **And it always asks.** That sentence used to be in this doc comment and
/// false: of the three, only the snapshot's asked. The other two fired on the
/// first click, behind a mark drawn in the same red, in the same box, in a
/// panel you reach the same way — so which kind of button you had pressed was
/// something you found out by pressing it. Whether the thing that goes is easy
/// to get back is a real difference between these three, and it is an argument
/// about how loudly to ask rather than about whether to.
///
/// Asking is the shape's job rather than the caller's, so the three ask the
/// same way: the mark becomes `verb?` and the box fills faintly red. It was the
/// snapshot row's own idea and it was good; it was just one row's.
///
/// `verb` is a word, not a mark, so it is set at the size the type scale gives
/// buttons and chips rather than at the glyph size [`glyph`] states.
///
/// Both were a glyph with five pixels of side padding and no height, so the
/// thing you had to hit to delete something was about sixteen pixels tall: the
/// smallest targets in the window were the only two that destroy anything.
pub fn remove(
    id: impl Into<gpui_kit::ElementId>,
    palette: &Palette,
    hint: impl Into<SharedString>,
    verb: &'static str,
    armed: bool,
) -> Stateful<Div> {
    let asking =
        glyph(id, palette, hint).hover(|this| this.bg(palette.hover).text_color(palette.danger));
    if armed {
        asking
            .bg(palette.danger.opacity(0.18))
            .text_color(palette.danger)
            .text_size(px(metrics::TEXT_SM))
            .child(SharedString::from(format!("{verb}?")))
    } else {
        asking.child(MARK)
    }
}

/// The small uppercase label that names a group of rows.
///
/// One of these, not two. `app.rs` padded it below and `settings_panel.rs`
/// padded it above and below, so the rails and the settings panel disagreed by
/// four pixels about what a heading is.
pub fn section_label(palette: &Palette, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .pt(px(4.0))
        .pb(px(4.0))
        .text_size(px(metrics::TEXT_XS))
        .text_color(palette.text_faint)
        .child(text.into())
}

/// A sentence explaining the control above it.
///
/// Subordinate to that control, and sized to say so.
pub fn hint(palette: &Palette, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .pb(px(8.0))
        .text_size(px(metrics::TEXT_XS))
        .text_color(palette.text_faint)
        .child(text.into())
}

/// What a panel says when it has nothing else to say.
///
/// A size larger than [`hint`], and nothing hanging off it, because this *is*
/// the panel's content rather than a footnote to something else — "Nothing
/// obviously reclaimable here." is the answer, not an aside.
///
/// Both of these were called `hint`, one in `app.rs` and one in
/// `settings_panel.rs`, which is how two different jobs came to share a name
/// and then quietly disagree about the size to do them at.
pub fn note(palette: &Palette, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(metrics::TEXT_SM))
        .text_color(palette.text_faint)
        .child(text.into())
}
