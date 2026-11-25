// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::SystemTime;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px, uniform_list,
};
use kosha_core::snapshot::{self, Meta};
use kosha_core::snapshot_compare::{Comparison, Status, Summary};
use kosha_core::{ScanOptions, Scanner, SizeTree, format};
use serde_json::json;

use crate::app::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

#[derive(Clone)]
struct Source {
    path: PathBuf,
    snapshot: bool,
}

struct Finding {
    path: String,
    path_bytes_hex: Option<String>,
    status: Status,
    reasons: [Option<&'static str>; 2],
}

struct Report {
    sources: [Meta; 2],
    summary: Summary,
    findings: Arc<Vec<Finding>>,
    by_status: [Arc<Vec<usize>>; 4],
    verified: bool,
}

const FINDING_STATUSES: [Status; 4] = [
    Status::Changed,
    Status::LeftOnly,
    Status::RightOnly,
    Status::Indeterminate,
];
const MTIME_TOLERANCE: u32 = 2;

#[derive(Clone, Copy)]
enum ExportFormat {
    Text,
    Markdown,
    Jsonl,
}

impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Markdown => "md",
            Self::Jsonl => "jsonl",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Text => "Plain text",
            Self::Markdown => "Markdown",
            Self::Jsonl => "JSONL",
        }
    }
}

fn write_report(
    report: &Report,
    sources: [&Source; 2],
    filter: Option<usize>,
    style: ExportFormat,
    mut out: impl Write,
) -> io::Result<usize> {
    let filter = filter.map(|index| FINDING_STATUSES[index]);
    let filter_label = filter.map_or("all", Status::label);
    let summary = &report.summary;
    let rows = report
        .findings
        .iter()
        .filter(|row| filter.is_none_or(|status| status == row.status));
    let counts = [
        ("changed", summary.changed),
        ("left-only", summary.left_only),
        ("right-only", summary.right_only),
        ("indeterminate", summary.indeterminate),
        ("metadata-match", summary.matched),
        ("content-match", summary.content_matched),
    ];
    let markdown = matches!(style, ExportFormat::Markdown);
    let human = |value: &str| {
        if markdown {
            crate::comparison_export::text(value)
        } else {
            format::printable(value).into_owned()
        }
    };
    let mut written = 0;
    if matches!(style, ExportFormat::Jsonl) {
        let source = |index: usize| {
            let source = sources[index];
            let meta = &report.sources[index];
            json!({
                "kind": if source.snapshot { "snapshot" } else { "live-folder" },
                "path": source.path.to_string_lossy(),
                "root": meta.root.to_string_lossy(),
                "taken_unix": meta.taken,
                "taken_ms": meta.taken_ms,
                "incomplete": summary.incomplete[index],
            })
        };
        let mut line = |record| -> io::Result<()> {
            serde_json::to_writer(&mut out, &record).map_err(io::Error::other)?;
            writeln!(out)
        };
        line(json!({
            "type": "header", "schema": "kosha.folder-comparison-report.v1",
            "scope": "filtered-findings", "filter": filter_label,
            "mtime_tolerance_seconds": MTIME_TOLERANCE, "verify_contents": report.verified,
            "left": source(0), "right": source(1),
        }))?;
        for row in rows {
            let mut record = json!({
                "type": "entry", "path": row.path, "status": row.status.label(),
                "reasons": row.reasons.into_iter().flatten().collect::<Vec<_>>(),
            });
            if let Some(raw) = &row.path_bytes_hex {
                record["path_bytes_hex"] = json!(raw);
            }
            line(record)?;
            written += 1;
        }
        line(json!({
            "type": "summary", "exported_findings": written,
            "counts": counts.into_iter().map(|(label, count)| (label.to_owned(), json!(count)))
                .collect::<serde_json::Map<_, _>>(),
        }))?;
    } else {
        writeln!(
            out,
            "{}Folder comparison\n",
            if markdown { "# " } else { "" }
        )?;
        for (index, side) in ["Left", "Right"].into_iter().enumerate() {
            let source = sources[index];
            let meta = &report.sources[index];
            let kind = if source.snapshot {
                "snapshot"
            } else {
                "live folder"
            };
            writeln!(
                out,
                "{side} ({kind}): {}  ",
                human(&source.path.to_string_lossy())
            )?;
            if source.snapshot {
                writeln!(
                    out,
                    "Recorded root: {}  ",
                    human(&meta.root.to_string_lossy())
                )?;
            }
            writeln!(
                out,
                "Observation (Unix seconds): {}.{:03}  ",
                meta.taken, meta.taken_ms
            )?;
            if summary.incomplete[index] {
                writeln!(out, "Warning: {side} source has incomplete coverage.  ")?;
            }
            writeln!(out)?;
        }
        writeln!(
            out,
            "Modification-time tolerance: {MTIME_TOLERANCE} seconds  "
        )?;
        writeln!(
            out,
            "Content verification: {}  ",
            if report.verified { "enabled" } else { "off" }
        )?;
        writeln!(out, "Findings filter: {filter_label}\n")?;
        for (label, count) in counts {
            writeln!(out, "- {label}: {count}")?;
        }
        writeln!(out)?;
        if markdown {
            writeln!(out, "| Status | Path | Reason |\n| --- | --- | --- |")?;
        }
        for row in rows {
            let label = row.status.label();
            let path = human(&row.path);
            let reasons = row
                .reasons
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
            if markdown {
                writeln!(out, "| {label} | {path} | {reasons} |")?;
            } else {
                writeln!(out, "{label}\t{path}\t{reasons}")?;
            }
            written += 1;
        }
        if written == 0 {
            writeln!(out, "\nNo findings in this report.")?;
        }
    }
    Ok(written)
}

#[derive(Default)]
pub(crate) struct View {
    sources: [Option<Source>; 2],
    picking: Option<Arc<()>>,
    stop: Option<Arc<AtomicBool>>,
    report: Option<Arc<Report>>,
    exporting: Option<Arc<()>>,
    filter: Option<usize>,
    verify: bool,
    message: Option<String>,
}

impl View {
    pub(crate) fn new(ready: Option<&crate::state::Ready>) -> Self {
        let left = ready.and_then(|ready| match &ready.source {
            crate::state::Source::Snapshot(record) => Some(Source {
                path: record.path.clone(),
                snapshot: true,
            }),
            crate::state::Source::Disk
                if matches!(
                    ready.tree.stats().strategy,
                    kosha_core::Strategy::DirectoryWalk | kosha_core::Strategy::MasterFileTable
                ) =>
            {
                Some(Source {
                    path: ready.tree.root_path().to_path_buf(),
                    snapshot: false,
                })
            }
            _ => None,
        });
        Self {
            sources: [left, None],
            ..Self::default()
        }
    }

    pub(crate) fn stopping(&self) -> Option<bool> {
        self.stop.as_ref().map(|stop| stop.load(Ordering::Relaxed))
    }
}

impl KoshaApp {
    fn export_folder_comparison(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
        let Some(view) = self
            .folder_comparison
            .as_mut()
            .filter(|view| view.exporting.is_none() && view.picking.is_none())
        else {
            return;
        };
        let (Some(report), [Some(left), Some(right)]) = (view.report.clone(), view.sources.clone())
        else {
            return;
        };
        let filter = view.filter;
        let ticket = Arc::new(());
        view.exporting = Some(Arc::clone(&ticket));
        let directory = crate::places::home().unwrap_or_else(|| PathBuf::from("."));
        let suggested = format!("kosha-folder-comparison.{}", format.extension());
        let prompt = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            let Ok(path) = this.update(cx, |app, cx| app.file_picker_selection(selection, cx))
            else {
                return;
            };
            let message = if let Some(path) = path {
                let target = path.clone();
                let written = cx
                    .background_spawn(async move {
                        kosha_core::export::atomic_write(&target, |out| {
                            write_report(&report, [&left, &right], filter, format, out)
                        })
                    })
                    .await;
                Some(match written {
                    Ok(rows) => format!(
                        "Exported {} to {}",
                        format::plural(rows, "finding"),
                        path.display()
                    ),
                    Err(error) => {
                        format!("Could not export comparison to {}: {error}", path.display())
                    }
                })
            } else {
                None
            };
            this.update(cx, |app, cx| {
                if let Some(view) = app.folder_comparison.as_mut().filter(|view| {
                    view.exporting
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &ticket))
                }) {
                    view.exporting = None;
                    if let Some(message) = message {
                        view.message = Some(message);
                    }
                } else if let Some(message) = message {
                    app.set_notice(message);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn close_folder_comparison(&mut self, cx: &mut Context<Self>) {
        if self
            .folder_comparison
            .as_ref()
            .is_some_and(|view| view.stop.is_some())
        {
            self.cancel_folder_comparison();
        } else {
            self.folder_comparison = None;
        }
        cx.notify();
    }

    pub(crate) fn cancel_folder_comparison(&mut self) {
        if let Some(stop) = self
            .folder_comparison
            .as_ref()
            .and_then(|view| view.stop.as_ref())
        {
            stop.store(true, Ordering::Relaxed);
        }
    }

    fn pick_comparison_source(&mut self, side: usize, snapshot: bool, cx: &mut Context<Self>) {
        let Some(view) = self
            .folder_comparison
            .as_mut()
            .filter(|view| view.stop.is_none() && view.picking.is_none())
        else {
            return;
        };
        let ticket = Arc::new(());
        view.picking = Some(Arc::clone(&ticket));
        let prompt = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: snapshot,
            directories: !snapshot,
            multiple: false,
            prompt: Some("Select".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            this.update(cx, |this, cx| {
                if !this
                    .folder_comparison
                    .as_ref()
                    .and_then(|view| view.picking.as_ref())
                    .is_some_and(|active| Arc::ptr_eq(active, &ticket))
                {
                    return;
                }
                let selected = this
                    .file_picker_selection(selection, cx)
                    .and_then(|paths| paths.into_iter().next());
                let view = this
                    .folder_comparison
                    .as_mut()
                    .expect("picker belongs to this view");
                view.picking = None;
                if let Some(path) = selected {
                    view.sources[side] = Some(Source { path, snapshot });
                    if snapshot {
                        view.verify = false;
                    }
                    view.report = None;
                    view.message = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn compare_folders(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self
            .folder_comparison
            .as_mut()
            .filter(|view| view.stop.is_none() && view.picking.is_none())
        else {
            return;
        };
        let [Some(left), Some(right)] = view.sources.clone() else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        view.stop = Some(Arc::clone(&stop));
        view.report = None;
        view.filter = None;
        view.message = None;
        let worker_stop = Arc::clone(&stop);
        let verify = view.verify;
        let task = cx
            .background_executor()
            .spawn(async move { compare([left, right], verify, &worker_stop) });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                let Some(view) = this.folder_comparison.as_mut().filter(|view| {
                    view.stop
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &stop))
                }) else {
                    return;
                };
                view.stop = None;
                if stop.load(Ordering::Relaxed) {
                    view.message = Some("Comparison cancelled.".into());
                } else {
                    match outcome {
                        Ok(report) => view.report = Some(Arc::new(report)),
                        Err(error) => view.message = Some(error),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

fn read(source: &Source, stop: &Arc<AtomicBool>) -> Result<(Meta, SizeTree), String> {
    if stop.load(Ordering::Relaxed) {
        return Err("Comparison cancelled.".into());
    }
    let result = if source.snapshot {
        snapshot::store::open(&source.path).map_err(|error| error.to_string())
    } else {
        let started = SystemTime::now();
        Scanner::new(ScanOptions::default())
            .with_cancel(Arc::clone(stop))
            .scan(&source.path)
            .map(|tree| (Meta::of(&tree, started, ""), tree))
            .map_err(|error| error.to_string())
    };
    result.map_err(|error| {
        format!(
            "{}: {error}",
            format::printable(&source.path.to_string_lossy())
        )
    })
}

fn compare(sources: [Source; 2], verify: bool, stop: &Arc<AtomicBool>) -> Result<Report, String> {
    if verify && (!cfg!(any(unix, windows)) || sources.iter().any(|source| source.snapshot)) {
        return Err(
            "Content verification requires two live folders on Windows, macOS, or Linux.".into(),
        );
    }
    let (left, a) = read(&sources[0], stop)?;
    let (right, b) = read(&sources[1], stop)?;
    if stop.load(Ordering::Relaxed) {
        return Err("Comparison cancelled.".into());
    }
    let comparison = Comparison::new((&left, &a), (&right, &b), MTIME_TOLERANCE)?;
    #[cfg(any(unix, windows))]
    let comparison = if verify {
        comparison
            .with_content_verification([a.root_path(), b.root_path()], stop)
            .map_err(|error| {
                format!("Could not open both folders for content verification: {error}")
            })?
    } else {
        comparison
    };
    let mut findings = Vec::new();
    let mut by_status: [Vec<usize>; 4] = Default::default();
    let summary = comparison.try_visit(|row| {
        if stop.load(Ordering::Relaxed) {
            return Err("Comparison cancelled.");
        }
        if let Some(index) = FINDING_STATUSES
            .iter()
            .position(|status| *status == row.status)
        {
            by_status[index].push(findings.len());
            let path = String::from_utf8_lossy(row.path);
            let path_bytes_hex = matches!(path, std::borrow::Cow::Owned(_)).then(|| {
                use std::fmt::Write as _;
                let mut hex = String::with_capacity(row.path.len() * 2);
                for byte in row.path {
                    let _ = write!(hex, "{byte:02x}");
                }
                hex
            });
            findings.push(Finding {
                path: path.into_owned(),
                path_bytes_hex,
                status: row.status,
                reasons: row.reasons,
            });
        }
        Ok(())
    })?;
    Ok(Report {
        sources: [left, right],
        summary,
        findings: Arc::new(findings),
        by_status: by_status.map(Arc::new),
        verified: verify,
    })
}

pub(crate) fn render(view: &View, palette: &Palette, cx: &mut Context<KoshaApp>) -> AnyElement {
    let editable = view.stop.is_none() && view.picking.is_none() && view.exporting.is_none();
    let selected = view.sources.iter().all(Option::is_some);
    let next_step = match view.sources.each_ref().map(Option::is_some) {
        [false, false] => Some("Choose a source on each side to start a comparison."),
        [true, false] => Some("Choose the right source to compare with the left."),
        [false, true] => Some("Choose the left source to compare with the right."),
        [true, true] => None,
    };
    let can_verify = cfg!(any(unix, windows))
        && view
            .sources
            .iter()
            .all(|source| source.as_ref().is_some_and(|source| !source.snapshot));
    v_flex().id("folder-comparison").debug_selector(|| "folder-comparison".into()).size_full().gap(px(10.)).p(px(14.))
        .child(h_flex().items_center().justify_between().child(div().text_size(metrics::scaled(metrics::TEXT_XL)).text_color(palette.text).child("Compare folders"))
            .children(match view.stopping() {
                Some(true) => None,
                stopping => Some(ui::button("close-folder-comparison", palette).debug_selector(|| "close-folder-comparison".into())
                    .child(if stopping.is_some() { "Cancel" } else { "Close" })
                    .on_click(cx.listener(|app, _, _, cx| app.close_folder_comparison(cx)))),
            }))
        .child(h_flex().items_stretch().gap(px(8.)).children(["Left source", "Right source"].into_iter().enumerate().map(|(side, label)| {
            let observation = view.sources[side].as_ref().map(|source| {
                let kind = if source.snapshot { "Snapshot" } else { "Live folder" };
                view.report.as_ref().map_or_else(|| kind.to_owned(), |report| {
                    let stamp = format::stamp(report.sources[side].taken, u64::from(format::now_unix()));
                    format!("{kind} · {stamp}")
                })
            });
            let mut card = v_flex().flex_1().min_w(px(0.)).gap(px(4.)).p(px(10.)).rounded(px(metrics::RADIUS_SM)).bg(palette.surface).border_1().border_color(palette.hairline)
                .child(h_flex().justify_between().flex_wrap().gap(px(6.)).text_size(metrics::scaled(metrics::TEXT_XS))
                    .text_color(palette.text_dim).child(label).children(observation.map(|value| div().child(value))));
            if let Some(source) = &view.sources[side] {
                let path = source.path.to_string_lossy();
                let name = source.path.file_name().map_or_else(|| path.clone(), |name| name.to_string_lossy());
                card = card
                    .child(div().id(("comparison-source-name", side)).debug_selector(move || if side == 0 { "comparison-left-selected".into() } else { "comparison-right-selected".into() }).truncate().text_color(palette.text).child(format::printable(&name).into_owned()))
                    .child(div().truncate().text_size(metrics::scaled(metrics::TEXT_SM)).text_color(palette.text_dim).child(format::printable(&path).into_owned()));
            } else {
                card = card.child(div().text_color(palette.text_dim).child("Choose a folder or saved snapshot"));
            }
            if let Some(report) = &view.report && view.sources[side].as_ref().is_some_and(|source| source.snapshot) {
                let root = report.sources[side].root.to_string_lossy();
                card = card.child(div().truncate().text_size(metrics::scaled(metrics::TEXT_XS)).text_color(palette.text_dim).child(format!("Recorded root: {}", format::printable(&root))));
            }
            if editable {
                card = card.child(div().flex_1()).child(h_flex().flex_wrap().gap(px(6.)).children([(false, "Folder…"), (true, "Snapshot…")].into_iter().map(|(snapshot, label)| {
                    ui::button((if snapshot { "pick-snapshot" } else { "pick-folder" }, side), palette).child(label)
                        .on_click(cx.listener(move |app, _, _, cx| app.pick_comparison_source(side, snapshot, cx)))
                })));
            }
            card
        })))
        .children((editable && selected).then(|| h_flex().items_center().flex_wrap().gap(px(8.))
            .child(ui::primary("run-folder-comparison", palette, false).child("Compare")
                .on_click(cx.listener(|app, _, _, cx| app.compare_folders(cx))))
            .child(ui::button("swap-compare-sides", palette).child("Swap sides").on_click(cx.listener(|app, _, _, cx| {
                if let Some(view) = &mut app.folder_comparison {
                    view.sources.swap(0, 1);
                    view.report = None;
                    view.message = None;
                    cx.notify();
                }
            })))
            .children(can_verify.then(|| ui::button("verify-comparison-content", palette)
            .bg(if view.verify { palette.accent.opacity(0.12) } else { palette.canvas })
            .text_color(if view.verify { palette.accent } else { palette.text_dim })
            .child(if view.verify { "Verify file contents · On" } else { "Verify file contents · Off" })
            .tooltip(|window, cx| ui::tooltip("Reads and compares equal-size files at matching paths. This can take longer.").build(window, cx))
            .on_click(cx.listener(|app, _, _, cx| {
                if let Some(view) = &mut app.folder_comparison {
                    view.verify = !view.verify;
                    view.report = None;
                    cx.notify();
                }
            }))))))
        .children(next_step.map(|step| div().text_color(palette.text_dim).child(step)))
        .children(view.message.clone().map(|message| div().text_color(palette.caution).child(message)))
        .children(view.report.as_ref().map(|report| render_report(report, view.filter, view.exporting.is_some(), palette, cx)))
        .into_any_element()
}

fn render_report(
    report: &Report,
    filter: Option<usize>,
    exporting: bool,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let summary = &report.summary;
    let findings = Arc::clone(&report.findings);
    let palette = *palette;
    let text = palette.text;
    let dim = palette.text_dim;
    let indices = filter.map(|index| Arc::clone(&report.by_status[index]));
    let count = indices
        .as_ref()
        .map_or(findings.len(), |indices| indices.len());
    let list_id = filter.map_or(0, |index| index + 1);
    let filters = ["Changed", "Only on left", "Only on right", "Indeterminate"];
    let metadata_matches = format::plural(summary.matched as usize, "metadata match");
    v_flex()
        .flex_1()
        .min_h(px(0.))
        .gap(px(6.))
        .mt(px(4.0))
        .pt(px(10.0))
        .border_t_1()
        .border_color(palette.hairline)
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .flex_wrap()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(metrics::scaled(metrics::TEXT_LG))
                        .text_color(text)
                        .child("Findings"),
                )
                .child(
                    div()
                        .text_size(metrics::scaled(metrics::TEXT_SM))
                        .text_color(dim)
                        .child(if report.verified {
                            format!(
                                "{metadata_matches} · {}",
                                format::plural(
                                    summary.content_matched as usize,
                                    "verified content match"
                                )
                            )
                        } else {
                            format!("{metadata_matches} · Contents unchecked")
                        }),
                ),
        )
        .child(
            h_flex()
                .items_center()
                .flex_wrap()
                .gap(px(4.))
                .text_color(dim)
                .child(if exporting {
                    "Exporting findings…"
                } else {
                    "Export findings"
                })
                .children(
                    (!exporting)
                        .then(|| {
                            [
                                ExportFormat::Text,
                                ExportFormat::Markdown,
                                ExportFormat::Jsonl,
                            ]
                            .map(|format| {
                                ui::button(("export-folder-comparison", format as usize), &palette)
                                    .child(format.label())
                                    .on_click(cx.listener(move |app, _, _, cx| {
                                        app.export_folder_comparison(format, cx)
                                    }))
                            })
                        })
                        .into_iter()
                        .flatten(),
                ),
        )
        .children(
            ["Left", "Right"]
                .into_iter()
                .zip(summary.incomplete)
                .filter(|(_, incomplete)| *incomplete)
                .map(|(side, _)| {
                    div()
                        .text_color(palette.caution)
                        .child(format!("{side} source has incomplete coverage."))
                }),
        )
        .child(
            h_flex().flex_wrap().gap(px(4.)).children(
                std::iter::once((None, "All findings", findings.len()))
                    .chain(
                        filters.into_iter().enumerate().map(|(index, name)| {
                            (Some(index), name, report.by_status[index].len())
                        }),
                    )
                    .map(|(choice, name, total)| {
                        let selected = filter == choice;
                        ui::chip((
                            "folder-comparison-filter",
                            choice.map_or(0, |index| index + 1),
                        ))
                        .bg(if selected {
                            palette.accent.opacity(0.12)
                        } else {
                            palette.canvas
                        })
                        .text_color(if selected { palette.accent } else { dim })
                        .child(format!("{name} ({total})"))
                        .on_click(cx.listener(move |app, _, _, cx| {
                            if let Some(view) = &mut app.folder_comparison {
                                view.filter = choice;
                                cx.notify();
                            }
                        }))
                    }),
            ),
        )
        .children((count == 0).then(|| {
            div().text_color(dim).child(if filter.is_some() {
                "No findings in this category."
            } else {
                "No differences or indeterminate paths found."
            })
        }))
        .child(
            uniform_list(
                ("folder-comparison-rows", list_id),
                count,
                move |range, _, _| {
                    range
                        .map(|index| {
                            let index = indices.as_ref().map_or(index, |indices| indices[index]);
                            let row = &findings[index];
                            let (status, tone) = match row.status {
                                Status::LeftOnly => ("Only on left", dim),
                                Status::RightOnly => ("Only on right", dim),
                                Status::Changed => ("Changed", palette.accent),
                                Status::Indeterminate => ("Indeterminate", palette.caution),
                                Status::MetadataMatch => ("Metadata matches", dim),
                                Status::ContentMatch => ("Content matches", dim),
                            };
                            let reasons = row
                                .reasons
                                .into_iter()
                                .flatten()
                                .map(|reason| match reason {
                                    "mtime" => "Modified time differs",
                                    "apparent-size" => "Size differs",
                                    "type" => "Entry type differs",
                                    "size-unavailable" => "Size unavailable",
                                    "mtime-unavailable" => "Modification time unavailable",
                                    "other-snapshot-incomplete" => {
                                        "Other source has incomplete coverage"
                                    }
                                    "duplicate-path" => "Ambiguous path",
                                    "content-differs" => "File contents differ",
                                    "content-matches" => "File contents match",
                                    "content-unavailable" => "File contents could not be verified",
                                    _ => "Metadata unavailable",
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            h_flex()
                                .w_full()
                                .h(metrics::scaled(metrics::ROW_HEIGHT + 16.0))
                                .items_center()
                                .border_b_1()
                                .border_color(palette.hairline)
                                .gap(px(8.))
                                .child(div().w(px(125.)).text_color(tone).child(status))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .truncate()
                                                .text_color(text)
                                                .child(format::printable(&row.path).into_owned()),
                                        )
                                        .children((!reasons.is_empty()).then(|| {
                                            div()
                                                .truncate()
                                                .text_size(metrics::scaled(metrics::TEXT_SM))
                                                .text_color(dim)
                                                .child(reasons)
                                        })),
                                )
                                .into_any_element()
                        })
                        .collect()
                },
            )
            .flex_1(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_report_keeps_only_findings_and_honours_cancellation() {
        let home = tempfile::tempdir().unwrap();
        let left = home.path().join("left");
        let right = home.path().join("right");
        for path in [&left, &right] {
            std::fs::create_dir(path).unwrap();
            std::fs::create_dir(path.join("matching-directory")).unwrap();
        }
        std::fs::write(left.join("only-left"), b"metadata only").unwrap();
        let sources = [left, right].map(|path| Source {
            path,
            snapshot: false,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let report = compare(sources.clone(), false, &stop).unwrap();
        assert_eq!(report.summary.matched, 1);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].status, Status::LeftOnly);
        assert_eq!(report.findings[0].path, "only-left");
        assert_eq!(report.by_status[1].as_slice(), &[0]);
        assert!(report.by_status[3].is_empty());
        for style in [
            ExportFormat::Text,
            ExportFormat::Markdown,
            ExportFormat::Jsonl,
        ] {
            let mut output = Vec::new();
            let inputs = [&sources[0], &sources[1]];
            assert_eq!(
                write_report(&report, inputs, Some(1), style, &mut output).unwrap(),
                1
            );
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("only-left"));
            assert!(!output.contains("matching-directory"));
            if matches!(style, ExportFormat::Jsonl) {
                let records = output
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(records[0]["filter"], "left-only");
                assert_eq!(records[1]["path"], "only-left");
                assert_eq!(records[2]["exported_findings"], 1);
                assert_eq!(records[2]["counts"]["metadata-match"], 1);
            }
            assert_eq!(
                write_report(&report, inputs, Some(2), style, Vec::new()).unwrap(),
                0
            );
        }
        stop.store(true, Ordering::Relaxed);
        assert!(
            matches!(compare(sources, false, &stop), Err(error) if error == "Comparison cancelled.")
        );
    }
}
