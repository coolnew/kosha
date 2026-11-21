// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The plan, shown before anything runs.
//!
//! Every number on this dialog comes from the dry run, which exercised the real
//! validation path — the guards, the root check, and the fingerprint
//! re-verification. So "3 will be skipped" here means the same three will be
//! skipped on execute, rather than being a separate estimate that can drift
//! away from what the code actually does.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use kosha_core::format;

use crate::app::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

/// The question asked before this window is allowed to change anything.
///
/// **Three documents described this dialog and none of them was a program.**
/// The keyboard table says `Ctrl+Shift+M` "always confirms", the safety model
/// says a grant "comes only from `Session::enable_read_write`, which still
/// needs the dialog", and `UserConfirmed::from_dialog_acceptance` says in its
/// own doc: call it "only from the branch where the user accepted the
/// confirmation dialog — never to satisfy the type checker". The toggle made
/// the token on the spot, under a comment admitting the real product shows a
/// dialog.
///
/// It names what the mode permits rather than asking "are you sure": the
/// reader is being told what changes, and every one of those actions still
/// shows its own plan and its own figure before it runs. That sentence is the
/// last line here, because the thing most worth knowing is that this is not
/// the last question they will be asked.
pub fn unlock(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> Option<AnyElement> {
    if !app.asking_to_unlock() {
        return None;
    }

    Some(
        ui::scrim("unlock-scrim", palette)
            .items_center()
            .justify_center()
            .child(
                ui::dialog("unlock-panel", palette)
                    .w(px(460.0))
                    .p(px(16.0))
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(metrics::TEXT_LG))
                            .text_color(palette.text)
                            .child("Let this window change files?"),
                    )
                    .child(
                        div()
                            .text_color(palette.text_dim)
                            .child("Read-write mode permits these file operations:"),
                    )
                    .child(
                        v_flex()
                            .gap(px(2.0))
                            .px(px(8.0))
                            .py(px(6.0))
                            .rounded(px(metrics::RADIUS_SM))
                            .bg(palette.surface)
                            .border_1()
                            .border_color(palette.hairline)
                            .text_size(px(metrics::TEXT_SM))
                            .text_color(palette.text_dim)
                            .child(if cfg!(windows) {
                                "Move to trash — recoverable from the Recycle Bin"
                            } else {
                                "Move to trash — recoverable from the system Trash"
                            })
                            .child("Empty a cache directory — permanent")
                            .child("Delete permanently — permanent")
                            .child("Compress — reversible, and loses nothing")
                            .child("Prune unused uv cache — permanent, separate tool preview"),
                    )
                    .child(
                        div()
                            .text_size(px(metrics::TEXT_SM))
                            .text_color(palette.text_faint)
                            .child(
                                "Nothing happens without a second confirmation: each action \
                                 shows exactly what it will do first. The mode is not \
                                 remembered — a restart is read-only again.",
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .justify_end()
                            .child(
                                ui::button("unlock-cancel", palette)
                                    .child("Stay read-only")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_unlock(cx))),
                            )
                            .child(
                                // Danger-coloured, because red is this window's
                                // word for "this can destroy something" and the
                                // mode chip is about to turn that colour.
                                ui::primary("unlock-go", palette, true)
                                    .child("Enable read-write")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.confirm_unlock(cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element(),
    )
}

pub fn render(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> Option<AnyElement> {
    let pending = app.pending()?;
    let plan = &pending.plan;
    let preview = &pending.preview;
    let irreversible = plan.has_irreversible();
    let compressing = plan.action() == Some(kosha_fsops::Action::Compress);

    let verb = plan
        .items()
        .first()
        .map(|item| item.action.label().to_string())
        .unwrap_or_else(|| "Run".into());

    // A handful of real paths beats a count. Users check whether the tool
    // understood them by reading the first few lines.
    let paths: Vec<SharedString> = plan
        .items()
        .iter()
        .take(8)
        .map(|item| {
            SharedString::from(format::printable(&item.path.to_string_lossy()).into_owned())
        })
        .collect();
    let more = plan.items().len().saturating_sub(paths.len());

    let caution: Vec<SharedString> = caution_lines(preview.skipped, plan.rejected())
        .into_iter()
        .map(SharedString::from)
        .collect();

    // Files stored only in the cloud. Every word comes from `kosha-fsops`, so
    // this dialog can draw the warning but cannot leave it out or reword it,
    // and the plan will not run without the token only its button makes. An
    // ordinary plan has no warning and this adds nothing to the dialog.
    let cloud = plan.cloud_warning();
    let cloud_title: Option<SharedString> = cloud
        .as_ref()
        .map(|w| SharedString::from(w.title().to_string()));
    let cloud_body: Option<SharedString> = cloud
        .as_ref()
        .map(|w| SharedString::from(w.body().to_string()));
    let cloud_confirm: Option<SharedString> = cloud
        .as_ref()
        .map(|w| SharedString::from(w.confirm_label().to_string()));
    let cloud_skip: Option<&'static str> = cloud.as_ref().and_then(|w| w.skip_label());
    let ordinary = cloud_confirm.is_none();
    let skipped_note: Option<SharedString> = plan.skipped_cloud_note().map(SharedString::from);

    Some(
        ui::scrim("confirm-scrim", palette)
            .items_center()
            .justify_center()
            .child(
                ui::dialog("confirm-panel", palette)
                    .w(px(560.0))
                    .max_h(px(560.0))
                    .gap(px(metrics::CARD_PADDING))
                    .child(
                        div()
                            .text_size(px(metrics::TEXT_LG))
                            .text_color(palette.text)
                            .child(SharedString::from(pending.title.clone())),
                    )
                    // After "Skip cloud-only items" the title still names the
                    // whole folder; this says part of it stays. The words and
                    // the count come from `kosha-fsops`.
                    .when_some(skipped_note, |this, note| {
                        this.child(
                            div()
                                .text_size(px(metrics::TEXT_SM))
                                .text_color(palette.text_dim)
                                .child(note),
                        )
                    })
                    .child(div().text_color(palette.text_dim).child(SharedString::from(
                        if compressing {
                            // No figure promised. How much compression
                            // saves depends entirely on the bytes, and a
                            // dry run has not read them — a number here
                            // would be one we made up and the user acted
                            // on.
                            format!(
                                "{} {}, {} on disk. How much that saves depends on \
                                     what is in them.",
                                verb,
                                format::plural(preview.succeeded, "item"),
                                app.bytes(preview.bytes_examined),
                            )
                        } else if preview.bytes_trashed > 0 {
                            // The trash is on the same volume, so this is a
                            // move, not a saving. Saying "freeing" here would
                            // be a lie the user acts on.
                            format!(
                                "{} {}, moving {} to the trash.",
                                verb,
                                format::plural(preview.succeeded, "item"),
                                app.bytes(preview.bytes_trashed),
                            )
                        } else {
                            format!(
                                "{} {}, freeing {}.",
                                verb,
                                format::plural(preview.succeeded, "item"),
                                app.bytes(preview.bytes_freed),
                            )
                        },
                    )))
                    .when(preview.bytes_trashed > 0, |this| {
                        this.child(
                            div()
                                .text_size(px(metrics::TEXT_SM))
                                .text_color(palette.text_faint)
                                .child(
                                    "The trash is on this volume, so the space comes back when \
                                     you empty it.",
                                ),
                        )
                    })
                    .when(irreversible, |this| {
                        this.child(
                            div()
                                .p(px(8.0))
                                .rounded(px(metrics::RADIUS_SM))
                                .bg(palette.danger.opacity(0.14))
                                .text_color(palette.danger)
                                .text_size(px(metrics::TEXT_SM))
                                .child("This cannot be undone from the trash."),
                        )
                    })
                    .when_some(cloud_title, |this, title| {
                        this.child(
                            v_flex()
                                .gap(px(4.0))
                                .p(px(8.0))
                                .rounded(px(metrics::RADIUS_SM))
                                .bg(palette.danger.opacity(0.14))
                                .text_color(palette.danger)
                                .text_size(px(metrics::TEXT_SM))
                                .child(title)
                                .children(cloud_body),
                        )
                    })
                    .child(
                        v_flex()
                            .id("confirm-paths")
                            .flex_1()
                            .min_h(px(0.0))
                            .gap(px(2.0))
                            .p(px(8.0))
                            .rounded(px(metrics::RADIUS_SM))
                            .bg(palette.surface)
                            .border_1()
                            .border_color(palette.hairline)
                            .overflow_y_scroll()
                            .text_size(px(metrics::TEXT_SM))
                            .text_color(palette.text_dim)
                            .children(paths.into_iter().map(|path| div().child(path)))
                            .when(more > 0, |this| {
                                this.child(div().text_color(palette.text_faint).child(
                                    SharedString::from(format!(
                                        "and {} more",
                                        format::count(more as u64)
                                    )),
                                ))
                            }),
                    )
                    // Skipped and refused items are stated, never rounded off.
                    // A plan that quietly does less than it claims is worse than
                    // one that admits the gap up front.
                    .when(!caution.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap(px(2.0))
                                .text_size(px(metrics::TEXT_SM))
                                .text_color(palette.caution)
                                .children(caution.into_iter().map(|line| div().child(line))),
                        )
                    })
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .justify_end()
                            .child(
                                ui::button("confirm-cancel", palette)
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_action(cx))),
                            )
                            .when_some(cloud_skip, |this, label| {
                                this.child(
                                    ui::button("confirm-skip-cloud", palette)
                                        .child(label)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.skip_cloud_only(cx)),
                                        ),
                                )
                            })
                            .when_some(cloud_confirm, |this, label| {
                                this.child(
                                    ui::primary("confirm-go-cloud", palette, true)
                                        .child(label)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm_action_including_cloud(cx)
                                        })),
                                )
                            })
                            .when(ordinary, |this| {
                                this.child(
                                    ui::primary("confirm-go", palette, irreversible)
                                        .child(SharedString::from(format!(
                                            "{verb} {}",
                                            format::plural(preview.succeeded, "item")
                                        )))
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.confirm_action(cx)),
                                        ),
                                )
                            }),
                    ),
            )
            .into_any_element(),
    )
}

/// What to say under the list about the items that will not be acted on.
///
/// **A refusal is a sentence, and this dialog was showing the count.** Every
/// refusal the guard produces is worded to name what is wrong *and* what would
/// work instead — "is on a network location, which the Recycle Bin does not
/// serve — a permanent delete would work", "is too long for this machine's
/// Recycle Bin (315 characters, and the limit is 259…)", "is a protected
/// system location" — and none of it reached anybody. This dialog is the only
/// place they can be shown, because the command line does not delete, and all
/// it said was how many there were: "1 will be skipped; 1 were refused
/// outright", plural and all.
///
/// Three of them, then a count. The dialog has a scrolling list above this for
/// the items themselves; this is the part a person reads before deciding, and
/// twenty sentences in it would be read by nobody.
///
/// A plain function over plain data so the wording can be tested without a
/// window, which is also why it makes the strings rather than the elements.
fn caution_lines(skipped: usize, rejected: &[(std::path::PathBuf, String)]) -> Vec<String> {
    const SHOWN: usize = 3;
    let mut lines = Vec::new();
    if skipped > 0 {
        lines.push(format!(
            "{} will be skipped, having changed since the scan.",
            format::plural(skipped, "item"),
        ));
    }
    if !rejected.is_empty() {
        lines.push(format!(
            "{} refused outright:",
            format::plural(rejected.len(), "item"),
        ));
        // Sanitised like every other name this window draws: a refusal quotes
        // the path, and a path can hold a newline on the filesystem this was
        // scanned from even when it cannot on this one.
        lines.extend(
            rejected
                .iter()
                .take(SHOWN)
                .map(|(_, why)| format::printable(why).into_owned()),
        );
        if rejected.len() > SHOWN {
            lines.push(format!(
                "and {} more",
                format::count((rejected.len() - SHOWN) as u64)
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::caution_lines;
    use std::path::PathBuf;

    fn refusal(name: &str, why: &str) -> (PathBuf, String) {
        (PathBuf::from(name), why.to_string())
    }

    #[test]
    fn a_refusal_is_shown_with_its_reason_and_not_merely_counted() {
        let lines = caution_lines(
            0,
            &[refusal(
                "/net/share/a.bin",
                "`/net/share/a.bin` is on a network location, which the Recycle \
                 Bin does not serve — a permanent delete would work",
            )],
        );
        assert_eq!(lines[0], "1 item refused outright:");
        assert!(
            lines[1].contains("a permanent delete would work"),
            "{lines:?}"
        );
    }

    /// One of anything is "1 item", which is what the sentence used to get
    /// wrong in both halves: "1 will be skipped; 1 were refused outright".
    #[test]
    fn one_of_each_reads_as_one() {
        let lines = caution_lines(1, &[refusal("a", "because")]);
        assert_eq!(
            lines[0],
            "1 item will be skipped, having changed since the scan."
        );
        assert_eq!(lines[1], "1 item refused outright:");
        let many = caution_lines(2, &[refusal("a", "x"), refusal("b", "y")]);
        assert!(many[0].starts_with("2 items"), "{many:?}");
        assert!(many[1].starts_with("2 items"), "{many:?}");
    }

    /// Long lists are cut, and the cut is stated rather than silent.
    #[test]
    fn beyond_three_reasons_the_rest_are_counted() {
        let many: Vec<_> = (0..7)
            .map(|i| refusal("p", &format!("reason {i}")))
            .collect();
        let lines = caution_lines(0, &many);
        assert_eq!(lines.len(), 1 + 3 + 1, "{lines:?}");
        assert_eq!(lines.last().unwrap(), "and 4 more");
    }

    /// The reason quotes a path, and a path is not ours to trust.
    #[test]
    fn a_reason_with_a_control_character_in_it_is_made_printable() {
        let lines = caution_lines(0, &[refusal("p", "`two\nlines` is protected")]);
        assert!(!lines[1].chars().any(char::is_control), "{lines:?}");
        assert!(lines[1].contains("two?lines"), "{lines:?}");
    }

    /// Nothing to say is nothing said: no empty band of colour on a dialog
    /// where everything is fine.
    #[test]
    fn nothing_refused_and_nothing_skipped_says_nothing() {
        assert!(caution_lines(0, &[]).is_empty());
    }
}
