// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px, relative,
};
use kosha_core::snapshot::store::{self, HistoryEntry};
use kosha_core::volume_history::{FolderGrowth, ScanObservation, VolumeId, folder_growth};
use kosha_core::{DiskUsage, ScanOptions, Scanner, SizeTree, format};

use super::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

pub(super) struct TrackedCapture {
    dir: PathBuf,
    capture: ScanObservation,
    generation: store::HistoryTracking,
}

impl TrackedCapture {
    pub(super) fn start(root: &std::path::Path) -> Option<Self> {
        let dir = store::history_dir().filter(|dir| dir.is_dir())?;
        let capture = ScanObservation::start(root)?;
        let generation = store::history_tracking_in(&dir, capture.volume()).ok()??;
        Some(Self {
            dir,
            capture,
            generation,
        })
    }
}

pub(super) struct View {
    root: PathBuf,
    volume: Option<VolumeId>,
    entries: Vec<HistoryEntry>,
    interval: Option<[usize; 2]>,
    growth: Option<Result<Vec<FolderGrowth>, String>>,
    growth_ticket: Option<Arc<()>>,
    job: Option<Arc<AtomicBool>>,
    recording: bool,
    tracking: bool,
    message: Option<String>,
}

#[derive(Clone, Copy)]
enum Operation {
    Open,
    Record,
    Track(bool),
    Clear,
}

impl Drop for View {
    fn drop(&mut self) {
        if let Some(stop) = &self.job {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

impl KoshaApp {
    pub(super) fn save_tracked_history(
        &mut self,
        capture: TrackedCapture,
        tree: Arc<SizeTree>,
        load: u64,
        cx: &mut Context<Self>,
    ) {
        let task = cx.background_executor().spawn(async move {
            if let Some(observation) = capture.capture.finish(&tree) {
                store::save_tracked_history_in(&capture.dir, &observation, &capture.generation)?;
            }
            Ok::<_, std::io::Error>(())
        });
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |app, cx| {
                    if app.load_generation == load {
                        app.set_notice(format!("Could not record drive history: {error}"));
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn history_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.volume_history.is_some() || self.folder_comparison.is_some() {
            return None;
        }
        let ready = self.state.ready()?;
        let root = ready.tree.root_path();
        if !self.is_automatic_scan_root(root)
            || !matches!(
                ready.tree.stats().strategy,
                kosha_core::Strategy::DirectoryWalk | kosha_core::Strategy::MasterFileTable
            )
        {
            return None;
        }
        let root = root.to_path_buf();
        Some(
            super::clickable_in_title_bar(ui::button("volume-history", palette))
                .debug_selector(|| "volume-history".into())
                .child("History")
                .on_click(
                    cx.listener(move |app, _, _, cx| app.open_volume_history(root.clone(), cx)),
                )
                .into_any_element(),
        )
    }

    fn open_volume_history(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.volume_history = Some(View {
            root,
            volume: None,
            entries: Vec::new(),
            interval: None,
            growth: None,
            job: None,
            recording: false,
            tracking: false,
            message: None,
            growth_ticket: None,
        });
        self.refresh_history(Operation::Open, cx);
    }

    fn confirm_clear_history(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let Some(view) = self
            .volume_history
            .as_ref()
            .filter(|view| view.job.is_none())
        else {
            return;
        };
        let root = view.root.clone();
        let volume = view.volume.clone();
        let prompt = window.prompt(
            gpui_kit::PromptLevel::Warning,
            "Clear this drive’s history?",
            Some("This removes all stored history observations for this drive. Its files and separately saved snapshots are kept."),
            &["Cancel", "Clear history"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if prompt.await == Ok(1) {
                this.update(cx, |app, cx| {
                    if app
                        .volume_history
                        .as_ref()
                        .is_some_and(|view| view.root == root && view.volume == volume)
                    {
                        app.refresh_history(Operation::Clear, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn refresh_history(&mut self, operation: Operation, cx: &mut Context<Self>) {
        let Some(view) = self
            .volume_history
            .as_mut()
            .filter(|view| view.job.is_none())
        else {
            return;
        };
        let root = view.root.clone();
        let previous = view.volume.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        view.job = Some(Arc::clone(&stop));
        view.recording = matches!(operation, Operation::Record);
        view.message = Some(
            match operation {
                Operation::Open => "Opening drive history…",
                Operation::Record => "Recording drive scan…",
                Operation::Track(_) => "Updating tracking…",
                Operation::Clear => "Clearing drive history…",
            }
            .into(),
        );
        let task = cx.background_executor().spawn(async move {
            let dir =
                store::history_dir().ok_or("The application data directory is unavailable.")?;
            let volume = VolumeId::at_root(&root)
                .ok_or("This drive has no supported persistent volume identifier.")?;
            if previous.as_ref().is_some_and(|old| old != &volume) {
                return Err(
                    "The volume at this location changed. Reopen History to continue.".into(),
                );
            }
            if matches!(operation, Operation::Record) {
                let capture =
                    ScanObservation::start(&root).ok_or("The drive is no longer available.")?;
                let tree = Scanner::new(ScanOptions::default())
                    .with_cancel(Arc::clone(&worker_stop))
                    .scan(&root)
                    .map_err(|error| error.to_string())?;
                if worker_stop.load(Ordering::Relaxed) {
                    return Err("Recording cancelled.".into());
                }
                let observation = capture
                    .finish(&tree)
                    .filter(|observation| observation.volume() == &volume)
                    .ok_or("The scan could not be recorded for this volume.")?;
                if worker_stop.load(Ordering::Relaxed) {
                    return Err("Recording cancelled.".into());
                }
                store::save_history_in(&dir, &observation).map_err(|error| error.to_string())?;
            }
            match operation {
                Operation::Track(enabled) => store::set_history_tracking_in(&dir, &volume, enabled),
                Operation::Clear => store::clear_history_in(&dir, &volume),
                _ => Ok(()),
            }
            .map_err(|error| error.to_string())?;
            let tracking = store::history_tracking_in(&dir, &volume)
                .map_err(|error| error.to_string())?
                .is_some();
            let entries =
                store::list_history_in(&dir, &volume).map_err(|error| error.to_string())?;
            Ok::<_, String>((volume, entries, tracking))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |app, cx| {
                let Some(view) = app.volume_history.as_mut().filter(|view| {
                    view.job
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &stop))
                }) else {
                    return;
                };
                view.job = None;
                view.recording = false;
                match result {
                    Ok((volume, entries, tracking)) => {
                        view.volume = Some(volume);
                        view.tracking = tracking;
                        view.entries = entries;
                        view.interval = None;
                        view.growth = None;
                        view.growth_ticket = None;
                        view.message = None;
                        if view.entries.len() >= 2 {
                            let end = view.entries.len() - 1;
                            app.select_history_interval([end - 1, end], cx);
                        }
                    }
                    Err(error) => view.message = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn select_history_interval(&mut self, interval: [usize; 2], cx: &mut Context<Self>) {
        let Some(view) = self
            .volume_history
            .as_mut()
            .filter(|view| view.job.is_none())
        else {
            return;
        };
        let [start, end] = interval;
        if start >= end || end >= view.entries.len() {
            return;
        }
        let paths = interval.map(|index| view.entries[index].record.path.clone());
        view.interval = Some(interval);
        view.growth = None;
        let ticket = Arc::new(());
        view.growth_ticket = Some(Arc::clone(&ticket));
        let task = cx.background_executor().spawn(async move {
            let [before, after] = paths.each_ref().map(|path| store::open(path));
            let (_, before) = before.map_err(|error| error.to_string())?;
            let (_, after) = after.map_err(|error| error.to_string())?;
            folder_growth(&before, &after).ok_or_else(|| {
                "Folder growth needs two complete scans with compatible coverage.".into()
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |app, cx| {
                if let Some(view) = app.volume_history.as_mut().filter(|view| {
                    view.growth_ticket
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &ticket))
                }) {
                    view.growth_ticket = None;
                    view.growth = Some(result);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

pub(super) fn render(view: &View, palette: &Palette, cx: &mut Context<KoshaApp>) -> AnyElement {
    let ready = view.volume.is_some() && view.job.is_none();
    let interval = view.interval;
    let now = u64::from(format::now_unix());
    let stamps: Vec<_> = view
        .entries
        .iter()
        .map(|entry| format::stamp(entry.record.meta.taken, now))
        .collect();
    let axis = view
        .entries
        .iter()
        .filter_map(|entry| entry.usage.map(|usage| usage.total))
        .max()
        .unwrap_or(1)
        .max(1);
    let mut panel =
        v_flex()
            .id("drive-history-panel")
            .size_full()
            .overflow_y_scroll()
            .gap(px(14.))
            .p(px(16.))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_size(metrics::scaled(metrics::TEXT_XL))
                                    .child("Drive history"),
                            )
                            .child(div().text_color(palette.text_dim).child(
                                format::printable(&view.root.to_string_lossy()).into_owned(),
                            )),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .children(ready.then(|| {
                                ui::primary("history-record", palette, false)
                                    .child("Record scan")
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        app.refresh_history(Operation::Record, cx)
                                    }))
                            }))
                            .children(view.recording.then(|| {
                                ui::button("history-cancel", palette)
                                    .child("Cancel scan")
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(view) = app.volume_history.as_mut()
                                            && let Some(stop) = &view.job
                                        {
                                            stop.store(true, Ordering::Relaxed);
                                            view.message = Some("Stopping scan…".into());
                                            cx.notify();
                                        }
                                    }))
                            }))
                            .child(
                                ui::button("history-close", palette)
                                    .child("Close")
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        app.volume_history = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .children(
                view.message
                    .as_ref()
                    .map(|message| div().text_color(palette.text_dim).child(message.clone())),
            )
            .children(view.volume.is_some().then(|| {
                let enable = !view.tracking;
                h_flex()
                    .items_center()
                    .justify_between()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .items_center()
                            .flex_wrap()
                            .gap(px(8.))
                            .children(ready.then(|| {
                                ui::button("history-track", palette)
                                    .child(if enable {
                                        "Track this drive"
                                    } else {
                                        "Stop tracking"
                                    })
                                    .on_click(cx.listener(move |app, _, _, cx| {
                                        app.refresh_history(Operation::Track(enable), cx)
                                    }))
                            }))
                            .child(div().text_color(palette.text_dim).child(if enable {
                                "Record observations only when requested."
                            } else {
                                "Completed drive scans are added to history."
                            })),
                    )
                    .children((ready && !view.entries.is_empty()).then(|| {
                        ui::button("history-clear", palette)
                            .child("Clear history…")
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.confirm_clear_history(window, cx)
                            }))
                    }))
            }));
    if view.entries.is_empty() {
        if ready {
            panel = panel.child(div().text_color(palette.text_dim).child(
                "Record a scan to start. A second scan shows how free space and folders changed.",
            ));
        }
        return panel.into_any_element();
    }

    panel = panel.child(
        v_flex()
            .gap(px(6.))
            .child(
                div()
                    .text_size(metrics::scaled(metrics::TEXT_LG))
                    .child("Free space by scan"),
            )
            .children(view.entries.iter().enumerate().map(|(index, entry)| {
                let usage = entry.usage;
                let taken = entry.record.meta.taken;
                let stamp = &stamps[index];
                let when = if stamps.iter().filter(|other| *other == stamp).count() > 1 {
                    format!("{stamp} · #{}", index + 1)
                } else {
                    stamp.clone()
                };
                let time = format!(
                    "{} · {:03} ms",
                    format::local_timestamp(taken),
                    entry.record.meta.taken_ms
                );
                h_flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id(("history-time", index))
                            .w(px(130.))
                            .child(when)
                            .tooltip(move |window, cx| ui::tooltip(time.clone()).build(window, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(40.))
                            .h(px(8.))
                            .rounded(px(4.))
                            .bg(palette.hairline)
                            .children(usage.map(|usage| {
                                div()
                                    .h_full()
                                    .rounded(px(4.))
                                    .bg(palette.accent)
                                    .w(relative(
                                        (usage.free as f64 / axis as f64).clamp(0., 1.) as f32
                                    ))
                            })),
                    )
                    .child(
                        div()
                            .w(px(150.))
                            .text_color(palette.text_dim)
                            .child(free_label(usage, entry.partial)),
                    )
                    .children(interval.map(|[from, to]| {
                        h_flex()
                            .gap(px(4.))
                            .child(endpoint_button(
                                index,
                                from,
                                "Start",
                                ready && index < to,
                                palette,
                                cx,
                                move |app, cx| app.select_history_interval([index, to], cx),
                            ))
                            .child(endpoint_button(
                                index,
                                to,
                                "End",
                                ready && from < index,
                                palette,
                                cx,
                                move |app, cx| app.select_history_interval([from, index], cx),
                            ))
                    }))
            })),
    );
    if let Some([start, end]) = interval {
        let paths = [start, end].map(|index| view.entries[index].record.path.clone());
        let measure = view.entries[start].usage.zip(view.entries[end].usage);
        let detail = match measure {
            Some((a, b)) if a.total != b.total => "Capacity changed during this interval.".into(),
            Some((a, b)) => format!(
                "Free-space change: {}",
                signed_bytes(i128::from(b.free) - i128::from(a.free))
            ),
            None => "Free-space change unavailable.".into(),
        };
        panel = panel.child(
            v_flex()
                .gap(px(6.))
                .pt(px(10.))
                .border_t_1()
                .border_color(palette.hairline)
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            v_flex()
                                .child(
                                    div()
                                        .text_size(metrics::scaled(metrics::TEXT_LG))
                                        .child("Folder changes"),
                                )
                                .child(div().text_color(palette.text_dim).child(detail)),
                        )
                        .children(ready.then(|| {
                            ui::button("history-compare", palette)
                                .child("Open comparison")
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.folder_comparison =
                                        Some(crate::folder_comparison::View::from_snapshots(
                                            paths.clone(),
                                        ));
                                    app.compare_folders(cx);
                                }))
                        })),
                )
                .children(
                    match &view.growth {
                        None => Some("Calculating changes…"),
                        Some(Err(error)) => Some(error.as_str()),
                        Some(Ok(rows)) if rows.is_empty() => {
                            Some("No measured allocation changes in top-level folders.")
                        }
                        _ => None,
                    }
                    .map(|message| div().text_color(palette.text_dim).child(message.to_owned())),
                )
                .children(
                    view.growth
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .into_iter()
                        .flatten()
                        .take(8)
                        .map(|row| {
                            h_flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.))
                                .child(div().truncate().child(
                                    format::printable(&row.name.to_string_lossy()).into_owned(),
                                ))
                                .child(div().flex_shrink_0().child(signed_bytes(row.delta())))
                        }),
                ),
        );
    }
    panel.into_any_element()
}

fn endpoint_button(
    index: usize,
    current: usize,
    label: &'static str,
    enabled: bool,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
    action: impl Fn(&mut KoshaApp, &mut Context<KoshaApp>) + 'static,
) -> AnyElement {
    if index == current {
        return div()
            .w(px(52.))
            .text_color(palette.accent)
            .child(label)
            .into_any_element();
    }
    if !enabled {
        return div().w(px(52.)).into_any_element();
    }
    ui::button((label, index), palette)
        .w(px(52.))
        .child(label)
        .on_click(cx.listener(move |app, _, _, cx| action(app, cx)))
        .into_any_element()
}

fn free_label(usage: Option<DiskUsage>, partial: bool) -> String {
    let mut label = usage.map_or_else(
        || "Unavailable".to_owned(),
        |usage| format!("{} free", format::bytes(usage.free)),
    );
    if partial {
        label.push_str(" · Partial");
    }
    label
}

fn signed_bytes(delta: i128) -> String {
    let sign = if delta < 0 {
        "−"
    } else if delta > 0 {
        "+"
    } else {
        ""
    };
    format!("{sign}{}", format::bytes(delta.unsigned_abs() as u64))
}
