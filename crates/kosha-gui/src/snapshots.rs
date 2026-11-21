// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The snapshots rail, and how a snapshot describes itself.
//!
//! A snapshot's whole value is that it is *old*, so everywhere one appears it
//! is named by how long ago it was taken rather than by its filename. "Three
//! days ago" is the fact somebody is deciding on; `001789214946-home-me.kosha`
//! is a detail of how it is stored.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::ui::tooltip;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use kosha_core::format;
use kosha_core::snapshot::{Meta, store};

use crate::app::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

/// How long ago a snapshot was taken, in words.
///
/// Rounded hard and deliberately. The difference between 71 and 73 minutes is
/// not a thing anybody is deciding on, and printing it invites a reader to
/// believe the number is exact when what it is measuring — "is this still
/// roughly what my disk looks like?" — never is.
pub fn when(meta: &Meta) -> SharedString {
    ago(meta, SystemTime::now())
}

/// The same, from a clock the caller supplies, so it can be tested.
pub fn ago(meta: &Meta, now: SystemTime) -> SharedString {
    let Some(age) = meta.age(now) else {
        // Stamped in the future. Somebody's clock is wrong, or the file came
        // from a machine whose clock is, and "in -3 days" helps nobody.
        return SharedString::from("at an unknown time");
    };

    // Each unit holds until the next one has something to say. Minutes run to
    // an hour and a half rather than to an hour, because "90 minutes ago" is a
    // more useful sentence than "2 hours ago" — and months stop at a year,
    // because "12 months ago" is a thing nobody says out loud.
    let secs = age.as_secs();
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;

    // Every unit divides down, not up. Rounding up skipped the singular of
    // the smallest one entirely — 90 seconds came out "2 minutes ago" and
    // nothing was ever "1 minute ago" — and an age that rounds up is an age
    // that is wrong in the direction of sounding staler than it is.
    let (count, unit) = match secs {
        0..=59 => return SharedString::from("just now"),
        60..=5399 => (secs / MINUTE, "minute"),
        // Hours stop at a day rather than at two, so that the day after a
        // snapshot is "1 day ago" and not "24 hours ago" — which is a thing
        // nobody says when they mean yesterday, and which left "1 day ago"
        // unreachable in exactly the way "1 minute ago" was.
        5400..=86_399 => (secs / HOUR, "hour"),
        // Days run to six weeks rather than to two months, so that the first
        // month a snapshot reaches can actually be called one: starting months
        // at sixty days would make "1 month ago" a thing this never says.
        86_400..=3_887_999 => (secs / DAY, "day"),
        3_888_000..=31_535_999 => (secs / MONTH, "month"),
        _ => (secs / YEAR, "year"),
    };
    SharedString::from(format!("{} ago", format::plural(count as usize, unit)))
}

/// A snapshot named by the clock rather than by its age.
///
/// For the one place an age is not enough: a comparison names both of its
/// sides, and two snapshots taken in one sitting are both "just now". A
/// heading reading `just now → just now` over a list of changes that have a
/// direction is worse than no heading at all.
pub fn exactly(meta: &Meta) -> SharedString {
    SharedString::from(format::stamp(meta.taken, u64::from(format::now_unix())))
}

/// One line under a snapshot's name: what it holds and what it costs.
pub fn detail(kept: &store::Kept, units: format::Units) -> SharedString {
    SharedString::from(format!(
        "{} · {} · {}",
        format::bytes_in(kept.meta.bytes_allocated, units),
        format::plural(kept.meta.entries as usize, "entry"),
        format::bytes_in(kept.bytes, units),
    ))
}

/// What holding this snapshot against what is on screen would answer.
///
/// The button says which of the two it is, because "compare" alone leaves a
/// reader to work out what the other side is — and when the other side is
/// another snapshot, getting that wrong means reading every sign backwards.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Against {
    /// A scan of the same folder is on screen.
    Scan,
    /// Another snapshot of the same folder is open.
    Snapshot,
    /// Nothing to hold it against: a different folder, or itself.
    Nothing,
}

impl Against {
    fn label(self) -> Option<&'static str> {
        match self {
            Against::Scan => Some("What changed since then"),
            Against::Snapshot => Some("What changed between these"),
            Against::Nothing => None,
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Against::Snapshot => {
                "Hold this snapshot against the one on screen: what grew, what shrank, \
                 and what came and went between the two"
            }
            _ => {
                "Hold this snapshot against the scan on screen: what grew, what shrank, \
                 and what came and went since it was taken"
            }
        }
    }
}

/// The rail section.
///
/// Absent entirely when there are none: a heading over nothing teaches people
/// that part of the window is dead. The way to make the first one is in the
/// command palette and on the status bar, which is where somebody who has just
/// finished a scan is already looking.
pub fn rail(
    app: &KoshaApp,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> Vec<gpui_kit::AnyElement> {
    let kept = app.snapshots();
    // A store holding nothing but damaged files is not an empty store, and the
    // rail used to draw it as one — no heading, no rows, and three files in the
    // directory occupying their megabytes.
    if kept.is_empty() && app.snapshot_problems().is_empty() {
        return Vec::new();
    }

    let open = app.snapshot_source().map(|one| one.path.clone());
    let units = app.units();
    // Comparing needs a tree on screen to hold this one against, and needs
    // that tree to be of the same folder. A button that is offered and then
    // refuses is worse than one that is not offered.
    let here = app.tree_root();
    let showing_snapshot = app.snapshot_source().is_some();

    // Newest first, where the store lists oldest first: a directory listing
    // wants chronological order and a person wants the most recent thing at
    // the top.
    //
    // And snapshots of the folder on screen before all the others, by a stable
    // sort that keeps each group in date order. They are the only ones that can
    // be compared with anything right now, and a rail that mixed them in by
    // date buried them among records of folders nobody is looking at — below
    // the fold, on a machine with a few months of snapshots on it.
    let current_only = app.snapshots_current_only();
    let order = snapshot_order(kept, here.as_deref(), current_only);

    let mut rows = Vec::with_capacity(kept.len());
    rows.push(
        h_flex()
            .gap(px(4.0))
            .mb(px(6.0))
            .child(
                scope_button("snapshots-all", "All", !current_only, false, palette).on_click(
                    cx.listener(|this, _, window, cx| this.set_snapshot_scope(false, window, cx)),
                ),
            )
            .child(
                scope_button(
                    "snapshots-location",
                    "This location",
                    current_only,
                    here.is_none(),
                    palette,
                )
                .tooltip(|window, cx| {
                    tooltip("Snapshots of the current scan's drive or folder root")
                        .build(window, cx)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.tree_root().is_some() {
                        this.set_snapshot_scope(true, window, cx);
                    }
                })),
            )
            .into_any_element(),
    );
    if order.is_empty() && current_only {
        rows.push(
            ui::note(
                palette,
                "No snapshots kept for this location. Choose All to see other locations.",
            )
            .into_any_element(),
        );
    }
    for index in order {
        let one = &kept[index];
        let showing = open.as_deref() == Some(&one.path);
        let against = match here.as_deref() {
            Some(root) if store::same_root(root, &one.meta.root) && !showing => {
                if showing_snapshot {
                    Against::Snapshot
                } else {
                    Against::Scan
                }
            }
            _ => Against::Nothing,
        };
        rows.push(row(
            one,
            index,
            showing,
            app.armed(&crate::app::Armed::Snapshot(one.path.clone())),
            against,
            units,
            palette,
            cx,
        ));
    }

    // Last, under the ones that opened: a file that cannot be read is not a
    // row anybody can click, and it must not push the working snapshots down.
    // Said rather than skipped, because a snapshot missing from this rail reads
    // as one that was never taken.
    for (index, problem) in app.snapshot_problems().iter().enumerate() {
        rows.push(
            div()
                .id(("snapshot-problem", index))
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.caution)
                .child(problem.clone())
                .into_any_element(),
        );
    }
    rows
}

fn snapshot_order(
    kept: &[store::Kept],
    root: Option<&std::path::Path>,
    current_only: bool,
) -> Vec<usize> {
    let matches =
        |index: usize| root.is_some_and(|root| store::same_root(root, &kept[index].meta.root));
    let mut order: Vec<_> = (0..kept.len())
        .rev()
        .filter(|&index| !current_only || root.is_none() || matches(index))
        .collect();
    order.sort_by_key(|&index| !matches(index));
    order
}

fn scope_button(
    id: &'static str,
    label: &'static str,
    active: bool,
    disabled: bool,
    palette: &Palette,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    ui::chip(id)
        .border_1()
        .border_color(palette.hairline)
        .text_color(if active {
            palette.accent
        } else {
            palette.text_dim
        })
        .when(active, |view| view.bg(palette.accent.opacity(0.12)))
        .when(disabled, |view| view.opacity(0.5))
        .hover(|view| view.bg(palette.hover))
        .child(label)
}

#[allow(clippy::too_many_arguments)]
fn row(
    kept: &store::Kept,
    index: usize,
    showing: bool,
    arming: bool,
    against: Against,
    units: format::Units,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> gpui_kit::AnyElement {
    let path: PathBuf = kept.path.clone();
    let to_delete = path.clone();
    let to_compare = path.clone();
    let name = when(&kept.meta);
    // Beside the age rather than instead of it. The age is the fact somebody
    // is deciding on; the clock time is how they tell two of them apart, and
    // without it a pair taken minutes apart are two rows that both read "just
    // now" with nothing to say which is which.
    let clock = SharedString::from(format::stamp(
        kept.meta.taken,
        u64::from(format::now_unix()),
    ));
    // A label and a root are text out of a file, and a file can have been
    // written anywhere: a newline in a filename is legal on Linux and a label is
    // whatever was typed after `--label`. `printable` here for the same reason
    // every name in the treemap goes through it.
    let root =
        SharedString::from(format::printable(&kept.meta.root.to_string_lossy()).into_owned());
    let detail = detail(kept, units);
    let label = format::printable(&kept.meta.label).into_owned();
    let opens: SharedString = if showing {
        "This snapshot is what is on screen".into()
    } else {
        format!(
            "Open this snapshot of {}, taken {clock}. Nothing in it is the disk as it is now.",
            format::printable(&kept.meta.root.to_string_lossy())
        )
        .into()
    };

    v_flex()
        .id(("snapshot", index))
        .px(px(8.0))
        .py(px(6.0))
        .gap(px(1.0))
        .rounded(px(metrics::RADIUS_SM))
        .cursor_pointer()
        .when(showing, |this| this.bg(palette.selected))
        .hover(|this| this.bg(palette.hover))
        .tooltip(move |window, cx| tooltip(opens.clone()).build(window, cx))
        .child(
            h_flex()
                .w_full()
                .gap(px(6.0))
                .items_center()
                .child(
                    div()
                        .text_color(if showing {
                            palette.text
                        } else {
                            palette.text_dim
                        })
                        .child(name),
                )
                .child(
                    div()
                        .text_size(px(metrics::TEXT_XS))
                        .text_color(palette.text_faint)
                        .child(clock),
                )
                // Pushed to the right edge of the row rather than given a
                // column of its own, so a rail of snapshots reads as a list of
                // dates and not as a list of delete buttons.
                .child(div().flex_1())
                .child(
                    ui::remove(
                        ("delete-snapshot", index),
                        palette,
                        "Delete this snapshot. The folder it describes is not touched.",
                        "Delete",
                        arming,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // The row underneath opens the snapshot. Without
                        // this, the delete button would also open the thing
                        // it is about to remove.
                        cx.stop_propagation();
                        this.delete_snapshot(to_delete.clone(), cx);
                    })),
                ),
        )
        .when(!label.is_empty(), |this| {
            this.child(
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.accent)
                    .child(SharedString::from(label.clone())),
            )
        })
        .child(
            div()
                .text_size(px(metrics::TEXT_XS))
                .text_color(palette.text_faint)
                .child(root),
        )
        .child(
            div()
                .text_size(px(metrics::TEXT_XS))
                .text_color(palette.text_faint)
                .child(detail),
        )
        // On a line of its own, spelled out in words, and saying which two
        // things it would hold against each other.
        //
        // This was a `Δ` at the end of the row above: five pixels of a Greek
        // letter, in the faintest colour the palette has, with nothing to say
        // what it did. Somebody who had taken snapshots specifically in order
        // to compare them reported that there was no way to compare them, which
        // is the only review of an affordance that counts.
        .children(against.label().map(|text| {
            // The same shape as the rail's other offers — "Keep a snapshot",
            // "Find duplicate files" — because it is the same kind of thing.
            // Written out by hand it had come out a third of their height and
            // a size smaller, which left the one affordance this feature is
            // for looking like a caption.
            ui::button(("compare-snapshot", index), palette)
                .mt(px(4.0))
                .child(text)
                .tooltip(move |window, cx| tooltip(against.hint()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.compare_with_snapshot(to_compare.clone(), cx);
                }))
        }))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_snapshot(path.clone(), cx);
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn scope_filter_keeps_newest_first_and_restores_other_locations() {
        let record = |root: &str, at| {
            let mut meta = meta_at(at);
            meta.root = PathBuf::from(root);
            store::Kept {
                path: PathBuf::from(format!("{at}.kosha")),
                meta,
                bytes: 0,
            }
        };
        let kept = vec![record("/one", 1), record("/two", 2), record("/one", 3)];
        let root = std::path::Path::new("/one");
        assert_eq!(super::snapshot_order(&kept, Some(root), true), vec![2, 0]);
        assert_eq!(
            super::snapshot_order(&kept, Some(root), false),
            vec![2, 0, 1]
        );
        assert_eq!(super::snapshot_order(&kept, None, true), vec![2, 1, 0]);
    }
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn meta_at(secs: u64) -> Meta {
        Meta {
            taken: secs,
            taken_ms: 0,
            root: PathBuf::from("/x"),
            label: String::new(),
            entries: 0,
            dirs: 0,
            files: 0,
            bytes_allocated: 0,
            bytes_logical: 0,
            strategy: String::new(),
            image: String::new(),
            partial: false,
            depth_limited: false,
            cancelled: false,
            unreadable: 0,
            skipped_other_device: 0,
            hardlinks_deduped: 0,
            dedup_hardlinks: false,
            cross_filesystems: false,
            exact_allocated: None,
            max_depth: 0,
            duration_ms: 0,
            tool: String::new(),
            import: None,
        }
    }

    fn said(age_secs: u64) -> String {
        let taken = 1_000_000_000;
        ago(
            &meta_at(taken),
            UNIX_EPOCH + Duration::from_secs(taken + age_secs),
        )
        .to_string()
    }

    #[test]
    fn an_age_reads_as_a_person_would_say_it() {
        assert_eq!(said(0), "just now");
        assert_eq!(said(59), "just now");
        assert_eq!(said(60), "1 minute ago");
        assert_eq!(said(90), "1 minute ago");
        assert_eq!(said(60 * 59), "59 minutes ago");
        // Minutes hold past the hour: "61 minutes ago" says more than
        // "2 hours ago" would, and rounding up to two hours at 61 minutes is
        // how a clock ends up lying by an hour.
        assert_eq!(said(3600 + 60), "61 minutes ago");
        assert_eq!(said(5400), "1 hour ago");
        assert_eq!(said(86_400 * 2), "2 days ago");
        assert_eq!(said(86_400 * 40), "40 days ago");
        assert_eq!(said(86_400 * 50), "1 month ago");
        assert_eq!(said(86_400 * 200), "6 months ago");
        assert_eq!(said(86_400 * 400), "1 year ago");
    }

    /// Every unit has to be reachable in the singular. Ranges chosen without
    /// checking this are how an interface ends up counting "2 months, 3 months"
    /// and never once saying "1 month".
    #[test]
    fn every_unit_can_be_said_once() {
        let singular = [
            "1 minute ago",
            "1 hour ago",
            "1 day ago",
            "1 month ago",
            "1 year ago",
        ];
        let mut found = [false; 5];
        // Every minute of four years, which is cheap and leaves no gap for a
        // boundary to hide in.
        for minutes in 0..(4 * 365 * 24 * 60u64) {
            let text = said(minutes * 60);
            for (index, want) in singular.iter().enumerate() {
                if text == *want {
                    found[index] = true;
                }
            }
        }
        for (index, want) in singular.iter().enumerate() {
            assert!(found[index], "no age is ever described as `{want}`");
        }
    }

    /// Singulars, because "1 days ago" is the kind of thing that makes a person
    /// trust the rest of the window slightly less.
    #[test]
    fn one_of_anything_is_singular() {
        assert_eq!(said(3600 * 2), "2 hours ago");
        assert!(!said(86_400 * 2).contains("2 dayss"));
        assert_eq!(said(86_400 * 370), "1 year ago");
        assert_eq!(said(86_400 * 800), "2 years ago");
        assert_eq!(said(86_400), "1 day ago");
        assert_eq!(said(86_399), "23 hours ago");
    }

    /// A clock somewhere is wrong, which is not a thing to render as a negative
    /// number of days.
    #[test]
    fn a_snapshot_from_the_future_says_nothing_clever() {
        let meta = meta_at(2_000_000_000);
        let text = ago(&meta, UNIX_EPOCH + Duration::from_secs(1_000_000_000));
        assert_eq!(text, "at an unknown time");
    }
}
