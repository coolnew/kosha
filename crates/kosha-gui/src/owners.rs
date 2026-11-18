// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Who is using the disk.
//!
//! A panel rather than a rail card, because it answers a different shape of
//! question from everything else in the window. The treemap and the table
//! answer "where are the bytes", which is a question about the tree; this
//! answers "whose are they", which cuts across the tree and has no place in it.
//! A person's files are scattered through a hundred directories and no view
//! organised by directory will ever add them up.
//!
//! Three things it has to get right, and all three are about not lying:
//!
//! * **Every byte lands in exactly one row**, including the bytes whose owner
//!   could not be read. The rows total the scan, which is what makes the
//!   percentages mean anything.
//! * **An account this machine cannot name is still its own row**, labelled by
//!   its identifier. On a shared machine the unrecognised account is often the
//!   answer, and a table that folded it into "other" would hide the one thing
//!   somebody opened this to find.
//! * **The operating system's own accounts are shown, below a divider.**
//!   `TrustedInstaller` and `SYSTEM` own most of a Windows installation. Hiding
//!   them loses bytes; listing them first buries the people under them.
//!
//! See [`kosha_core::owner`] for how the four outcomes are told apart.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use kosha_core::{Share, format};

use crate::app::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

/// The panel.
pub fn panel(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> AnyElement {
    let body = match app.tree() {
        Some(tree) if tree.owners().were_collected() => {
            let shares = kosha_core::owner::shares(tree);
            table(&shares, tree.total_allocated(), app, palette)
        }
        Some(_) => not_collected(palette, cx),
        None => nothing_yet(app, palette),
    };

    ui::scrim("owners-scrim", palette)
        .items_center()
        .justify_center()
        .on_click(cx.listener(|this, _, _, cx| this.close_owners(cx)))
        .child(
            ui::dialog("owners-panel", palette)
                .w(px(660.0))
                .max_h(px(640.0))
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_LG))
                                .text_color(palette.text)
                                .child("Who is using the disk"),
                        )
                        .child(
                            ui::close("owners-close", palette, "Close  (Esc)")
                                .on_click(cx.listener(|this, _, _, cx| this.close_owners(cx))),
                        ),
                )
                .child(body),
        )
        .into_any_element()
}

/// The rows, people first.
fn table(shares: &[Share], total: u64, app: &KoshaApp, palette: &Palette) -> AnyElement {
    let units = app.units();
    let (people, machine): (Vec<&Share>, Vec<&Share>) = shares
        .iter()
        .partition(|share| share.owner.as_ref().is_none_or(|o| o.kind.is_somebody()));

    v_flex()
        .id("owners-list")
        .flex_1()
        .min_h(px(0.0))
        .gap(px(2.0))
        .overflow_y_scroll()
        .children(
            people
                .iter()
                .enumerate()
                .map(|(index, share)| row(share, index, total, units, palette, palette.text)),
        )
        .when(!machine.is_empty(), |this| {
            this.child(
                div()
                    .mt(px(8.0))
                    .pt(px(6.0))
                    .border_t_1()
                    .border_color(palette.hairline)
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.text_faint)
                    .child(
                        "The operating system's own accounts. These are real owners and \
                         real bytes — they are set apart because they are nobody's files.",
                    ),
            )
        })
        .children(machine.iter().enumerate().map(|(index, share)| {
            row(
                share,
                people.len() + index,
                total,
                units,
                palette,
                palette.text_dim,
            )
        }))
        .into_any_element()
}

/// One account: a proportion bar, what it occupies, and what it is.
fn row(
    share: &Share,
    index: usize,
    total: u64,
    units: format::Units,
    palette: &Palette,
    text: gpui_kit::Hsla,
) -> AnyElement {
    let fraction = if total == 0 {
        0.0
    } else {
        (share.allocated as f64 / total as f64) as f32
    };

    // The identifier of an account the machine cannot name, said plainly. It is
    // not an error and the row is not a guess — there really is an account, and
    // this is the only handle a user has on it.
    let unknown = share
        .owner
        .as_ref()
        .is_some_and(|owner| !owner.is_named())
        .then_some("this computer does not know this account");
    let unreadable = share
        .owner
        .is_none()
        .then_some("kosha was not allowed to read who owns these");

    v_flex()
        .id(("owner", index))
        .w_full()
        .px(px(10.0))
        .py(px(6.0))
        .gap(px(4.0))
        .rounded(px(metrics::RADIUS_SM))
        .hover(|this| this.bg(palette.hover))
        .child(
            h_flex()
                .w_full()
                .gap(px(10.0))
                .items_baseline()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(metrics::TEXT_SM))
                        .text_color(text)
                        .child(SharedString::from(share.label())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(metrics::TEXT_SM))
                        .text_color(text)
                        .child(SharedString::from(format::bytes_in(share.allocated, units))),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(46.0))
                        .text_size(px(metrics::TEXT_SM))
                        .text_color(palette.text_faint)
                        .child(SharedString::from(format::percent(share.allocated, total))),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(palette.surface)
                .child(
                    div()
                        .h_full()
                        .w(gpui_kit::relative(fraction.clamp(0.0, 1.0)))
                        .rounded(px(2.0))
                        .bg(if unreadable.is_some() {
                            palette.text_faint
                        } else {
                            palette.accent
                        }),
                ),
        )
        .child(
            div()
                .text_size(px(metrics::TEXT_XS))
                .text_color(palette.text_faint)
                .child(SharedString::from(match unknown.or(unreadable) {
                    Some(note) => format!(
                        "{} · {} · {note}",
                        format::plural(share.files as usize, "file"),
                        format::plural(share.dirs as usize, "folder"),
                    ),
                    None => format!(
                        "{} · {}",
                        format::plural(share.files as usize, "file"),
                        format::plural(share.dirs as usize, "folder"),
                    ),
                })),
        )
        .into_any_element()
}

/// The scan did not record owners, so there is nothing to add up.
///
/// An offer rather than an apology: the setting is off by default and the only
/// way to the answer is another scan, so the button that starts one is the
/// whole content of this state.
fn not_collected(palette: &Palette, cx: &mut Context<KoshaApp>) -> AnyElement {
    v_flex()
        .gap(px(10.0))
        .child(
            div()
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text_dim)
                .child(
                    "This scan did not record who owns each file, so there is nothing to \
                     add up. It is off by default because on a machine one person uses it \
                     answers a question nobody asked.",
                ),
        )
        .when(kosha_core::owner_costs_extra(), |this| {
            this.child(
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.text_faint)
                    .child(
                        "On Windows the ordinary scan asks the filesystem once per file for \
                         this, which is slower. Running as administrator makes it free: the \
                         volume's own table of owners is read once instead.",
                    ),
            )
        })
        .child(
            ui::primary("owners-rescan", palette, false)
                .mt(px(4.0))
                .child("Scan again, recording owners")
                .on_click(cx.listener(|this, _, _, cx| this.rescan_with_owners(cx))),
        )
        .into_any_element()
}

/// No tree to add up — either because a scan is running, or because none has.
///
/// Two sentences rather than one, because telling somebody to start a scan
/// while their scan is running reads as the window having lost track of it.
fn nothing_yet(app: &KoshaApp, palette: &Palette) -> AnyElement {
    div()
        .text_size(px(metrics::TEXT_SM))
        .text_color(palette.text_dim)
        .child(if app.is_scanning() {
            "The scan is still running. This will fill in when it finishes."
        } else {
            "Scan somewhere first, and this will say whose the bytes are."
        })
        .into_any_element()
}
