// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime};

use gpui_kit::base::{h_flex, input::InputState, v_flex};
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use kosha_core::format;
use kosha_core::monitoring::{self, Attempt, Cadence, CheckResult, Location, Profile, Thresholds};
use kosha_core::snapshot::store::{self, MonitoringLease};

use super::KoshaApp;
use crate::{
    theme::{Palette, metrics},
    ui,
};

#[derive(Default)]
pub(super) struct View {
    rows: Vec<Row>,
    message: Option<String>,
    job: Option<Arc<AtomicBool>>,
    draft: Option<Draft>,
}

#[derive(Default)]
pub(super) struct ScheduledCheck {
    cancel: Arc<AtomicBool>,
    active: AtomicBool,
}

struct Draft {
    existing: Option<Profile>,
    root: Entity<InputState>,
    growth: Entity<InputState>,
    low_space: Entity<InputState>,
    cadence: Cadence,
}

struct Row {
    profile: Profile,
    status: String,
    alert: Option<String>,
    comparison: Option<[PathBuf; 2]>,
}

enum Operation {
    Refresh,
    Check(u64),
    Pause(u64, bool),
    Save(Option<u64>, PathBuf, Cadence, Thresholds),
    Remove(u64),
}

impl KoshaApp {
    pub(super) fn start_monitoring_schedule(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut interval = Duration::from_secs(5);
            loop {
                cx.background_executor().timer(interval).await;
                if this
                    .update(cx, |app, cx| app.poll_monitoring_schedule(cx))
                    .is_err()
                {
                    break;
                }
                interval = Duration::from_secs(60);
            }
        })
        .detach();
    }

    fn poll_monitoring_schedule(&mut self, cx: &mut Context<Self>) {
        if self.scheduled_monitoring.is_some()
            || self.is_scanning()
            || self.is_opening_drive()
            || self
                .monitoring
                .as_ref()
                .is_some_and(|view| view.job.is_some() || view.draft.is_some())
        {
            return;
        }
        let Some(dir) = store::monitoring_dir() else {
            return;
        };
        let job = Arc::new(ScheduledCheck::default());
        self.scheduled_monitoring = Some(Arc::clone(&job));
        let worker = Arc::clone(&job);
        let task = cx
            .background_executor()
            .spawn(async move { run_due(&dir, &worker) });
        cx.spawn(async move |this, cx| {
            // The worker marks activity only when a check is actually due.
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
            let result = task.await;
            let _ = this.update(cx, |app, cx| {
                app.scheduled_monitoring = None;
                match result {
                    Ok((checked, alerts)) => {
                        if alerts > 0 {
                            app.state.notice = Some(format!(
                                "{} in Monitoring.",
                                format::plural(alerts, "new storage alert")
                            ));
                        }
                        if checked
                            && app
                                .monitoring
                                .as_ref()
                                .is_some_and(|view| view.draft.is_none() && view.job.is_none())
                        {
                            app.monitoring_operation(Operation::Refresh, cx);
                        }
                    }
                    Err(error) if error.kind() != std::io::ErrorKind::WouldBlock => {
                        if let Some(view) =
                            app.monitoring.as_mut().filter(|view| view.job.is_none())
                        {
                            view.message =
                                Some(format!("Scheduled monitoring could not run: {error}"));
                        }
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn cancel_scheduled_monitoring(&self) {
        if let Some(job) = &self.scheduled_monitoring {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn monitoring_activity(&self) -> Option<super::ActivityRow> {
        let job = self.scheduled_monitoring.as_ref()?;
        if !job.active.load(Ordering::Relaxed) {
            return None;
        }
        let stopping = job.cancel.load(Ordering::Relaxed);
        Some(super::ActivityRow {
            id: "scheduled-monitoring".into(),
            title: "Storage monitoring",
            scope: "Scheduled checks".into(),
            status: if stopping { "Cancelling" } else { "Running" },
            cancel: (!stopping).then_some(crate::commands::Action::CancelScheduledMonitoring),
        })
    }
    pub(super) fn open_monitoring(&mut self, cx: &mut Context<Self>) {
        if self.monitoring.is_some() {
            return;
        }
        self.disarm();
        self.monitoring = Some(View::default());
        self.monitoring_operation(Operation::Refresh, cx);
    }

    fn draft_monitoring(
        &mut self,
        existing: Option<Profile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = existing
            .as_ref()
            .map(|p| p.location.root())
            .or_else(|| self.state.ready().map(|ready| ready.tree.root_path()))
            .map(|root| root.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (growth, low_space) = match existing.as_ref().map(|p| p.thresholds) {
            Some(Thresholds::Growth(bytes)) => (Some(bytes), None),
            Some(Thresholds::LowSpace(bytes)) => (None, Some(bytes)),
            Some(Thresholds::Both { growth, low_space }) => (Some(growth), Some(low_space)),
            None => (None, None),
        };
        let mut field = |placeholder: &'static str, value: String| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_value(value, window, cx);
                input
            })
        };
        let draft = Draft {
            cadence: existing.as_ref().map_or(Cadence::Daily, |p| p.cadence),
            existing,
            root: field("Absolute drive or folder path", root),
            growth: field(
                "Optional byte count",
                growth.map(|n| n.to_string()).unwrap_or_default(),
            ),
            low_space: field(
                "Optional byte count",
                low_space.map(|n| n.to_string()).unwrap_or_default(),
            ),
        };
        if let Some(view) = self.monitoring.as_mut() {
            view.draft = Some(draft);
            view.message = None;
            cx.notify();
        }
    }

    fn save_monitoring_draft(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.monitoring.as_mut() else {
            return;
        };
        let Some(draft) = view.draft.as_ref() else {
            return;
        };
        let root = PathBuf::from(draft.root.read(cx).value().to_string());
        let rules = parse_thresholds(
            &draft.growth.read(cx).value(),
            &draft.low_space.read(cx).value(),
        );
        match rules {
            Ok(rules) if draft.existing.is_some() || root.is_absolute() => {
                let operation = Operation::Save(
                    draft.existing.as_ref().map(|p| p.id),
                    root,
                    draft.cadence,
                    rules,
                );
                self.monitoring_operation(operation, cx);
            }
            Ok(_) => {
                view.message = Some("Enter the absolute path of a local drive or folder.".into());
                cx.notify();
            }
            Err(message) => {
                view.message = Some(message.into());
                cx.notify();
            }
        }
    }

    fn monitoring_operation(&mut self, operation: Operation, cx: &mut Context<Self>) {
        let Some(view) = self.monitoring.as_mut().filter(|view| view.job.is_none()) else {
            return;
        };
        let job = Arc::new(AtomicBool::new(false));
        view.job = Some(Arc::clone(&job));
        view.message = Some(if matches!(&operation, Operation::Check(_)) {
            "Checking storage…".into()
        } else {
            "Reading monitoring profiles…".into()
        });
        cx.notify();
        let cancel = Arc::clone(&job);
        let saving = matches!(operation, Operation::Save(..));
        let task = cx.background_executor().spawn(async move {
            let dir = store::monitoring_dir().ok_or_else(|| {
                std::io::Error::other("Monitoring data directory is unavailable.")
            })?;
            run(&dir, operation, cancel)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |app, cx| {
                let Some(view) = app.monitoring.as_mut().filter(|view| {
                    view.job
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &job))
                }) else {
                    return;
                };
                view.job = None;
                match result {
                    Ok((rows, message)) => {
                        view.rows = rows;
                        view.message = message;
                        if saving {
                            view.draft = None;
                        }
                    }
                    Err(error) => view.message = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn close_monitoring(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = self.monitoring.take().and_then(|view| view.job) {
            job.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }
}

fn run_due(dir: &Path, job: &ScheduledCheck) -> std::io::Result<(bool, usize)> {
    if !monitoring::list_profiles(dir)?
        .iter()
        .any(|profile| !profile.paused)
    {
        return Ok((false, 0));
    }
    let mut lease = MonitoringLease::try_acquire(dir)?;
    let now = SystemTime::now();
    let mut checked = false;
    let mut alerts = 0;
    for profile in monitoring::list_profiles(dir)? {
        if job.cancel.load(Ordering::Relaxed) {
            break;
        }
        if profile.paused
            || !profile.cadence.is_due(
                store::last_monitoring_attempt_in(dir, profile.id)?.map(|attempt| attempt.time),
                now,
            )
        {
            continue;
        }
        job.active.store(true, Ordering::Relaxed);
        checked = true;
        let result = monitoring::check(&mut lease, &profile, now, false, Arc::clone(&job.cancel))?;
        if let CheckResult::Observed { decision, .. } = result {
            alerts += usize::from(decision.growth_alert_bytes.is_some())
                + usize::from(decision.low_space_alert);
        }
    }
    Ok((checked, alerts))
}

fn run(
    dir: &Path,
    operation: Operation,
    cancel: Arc<AtomicBool>,
) -> std::io::Result<(Vec<Row>, Option<String>)> {
    let mut message = None;
    if !matches!(&operation, Operation::Refresh) {
        let mut lease = MonitoringLease::try_acquire(dir)?;
        let profile = |id| {
            monitoring::list_profiles(dir)?
                .into_iter()
                .find(|profile| profile.id == id)
                .ok_or_else(|| std::io::Error::other("This monitoring profile was removed."))
        };
        match operation {
            Operation::Check(id) => {
                let profile = profile(id)?;
                let result =
                    monitoring::check(&mut lease, &profile, SystemTime::now(), true, cancel)?;
                message = Some(
                    match result {
                        CheckResult::Observed { .. } => "Check completed.",
                        CheckResult::Paused => "Resume this profile before checking.",
                        CheckResult::Skipped(status) => attempt_message(status),
                        CheckResult::NotDue => "No check was due.",
                    }
                    .into(),
                );
            }
            Operation::Pause(id, paused) => {
                let profile = profile(id)?;
                lease.update_profile(id, profile.cadence, profile.thresholds, paused)?;
            }
            Operation::Save(Some(id), _, cadence, thresholds) => {
                lease.update_profile(id, cadence, thresholds, profile(id)?.paused)?;
            }
            Operation::Save(None, root, cadence, thresholds) => {
                let location = Location::pin(&root).ok_or_else(|| {
                    std::io::Error::other("Choose an accessible local drive or folder.")
                })?;
                lease.add_profile(location, cadence, thresholds)?;
            }
            Operation::Remove(id) => {
                lease.remove_profile(id)?;
                message = Some("Profile removed. Saved observations were kept.".into());
            }
            Operation::Refresh => unreachable!(),
        }
    }
    let mut rows = Vec::new();
    for profile in monitoring::list_profiles(dir)? {
        let entries = store::list_monitoring_in(dir, profile.id)?;
        let alerts = monitoring::alerts(dir, profile.id)?;
        let growth = alerts.growth;
        let path = |id| {
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.snapshot.path.clone())
        };
        let comparison = growth
            .as_ref()
            .and_then(|alert| Some([path(alert.before)?, path(alert.after)?]));
        let alert = match (growth, alerts.low_space_active) {
            (Some(growth), true) => Some(format!(
                "{} growth · Low free space",
                format::bytes(growth.bytes)
            )),
            (Some(growth), false) => {
                Some(format!("{} allocated growth", format::bytes(growth.bytes)))
            }
            (None, true) => Some("Low free space".into()),
            (None, false) => None,
        };
        let attempt = store::last_monitoring_attempt_in(dir, profile.id)?;
        let cadence = match profile.cadence {
            Cadence::Daily => "Daily",
            Cadence::Weekly => "Weekly",
        };
        let state = if profile.paused {
            "Paused"
        } else {
            attempt
                .as_ref()
                .map_or("No checks yet", |attempt| attempt_message(attempt.status))
        };
        let mut status = format!("{cadence} · {state}");
        if !profile.paused {
            let last = attempt.map(|attempt| attempt.time);
            if profile.cadence.is_due(last, SystemTime::now()) {
                status.push_str(" · Due");
            } else if let Some(next) = last
                .and_then(|time| time.checked_add(profile.cadence.interval()))
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            {
                status.push_str(&format!(
                    " · Next check {}",
                    format::stamp(next.as_secs(), u64::from(format::now_unix()))
                ));
            }
        }
        if let Some(latest) = entries.last() {
            status.push_str(&format!(
                " · Last saved {}",
                format::stamp(latest.snapshot.meta.taken, u64::from(format::now_unix()))
            ));
        }
        rows.push(Row {
            profile,
            status,
            alert,
            comparison,
        });
    }
    Ok((rows, message))
}

fn parse_thresholds(growth: &str, low_space: &str) -> Result<Thresholds, &'static str> {
    let bytes = |value: &str| {
        let value = value.trim();
        if value.is_empty() {
            Ok(None)
        } else {
            value
                .parse::<NonZeroU64>()
                .map(Some)
                .map_err(|_| "Enter a positive whole number of bytes.")
        }
    };
    match (bytes(growth)?, bytes(low_space)?) {
        (Some(growth), Some(low_space)) => Ok(Thresholds::Both { growth, low_space }),
        (Some(growth), None) => Ok(Thresholds::Growth(growth)),
        (None, Some(low_space)) => Ok(Thresholds::LowSpace(low_space)),
        (None, None) => Err("Enter at least one threshold."),
    }
}

fn attempt_message(status: Attempt) -> &'static str {
    match status {
        Attempt::Scanning => "Check started",
        Attempt::Unavailable => "Location unavailable",
        Attempt::Incomplete => "Incomplete scan",
        Attempt::Cancelled => "Check cancelled",
        Attempt::Completed => "Last check completed",
    }
}

fn render_draft(
    draft: &Draft,
    working: bool,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let mut cadence = h_flex().gap(px(6.));
    for (choice, label) in [(Cadence::Daily, "Daily"), (Cadence::Weekly, "Weekly")] {
        cadence = cadence.child(
            ui::button(("monitor-cadence", choice as u32), palette)
                .child(if draft.cadence == choice {
                    format!("✓ {label}")
                } else {
                    label.into()
                })
                .on_click(cx.listener(move |app, _, _, cx| {
                    if let Some(draft) = app
                        .monitoring
                        .as_mut()
                        .filter(|view| view.job.is_none())
                        .and_then(|view| view.draft.as_mut())
                    {
                        draft.cadence = choice;
                        cx.notify();
                    }
                })),
        );
    }
    v_flex()
        .p(px(12.))
        .gap(px(8.))
        .border_1()
        .border_color(palette.hairline)
        .child(div().text_size(metrics::scaled(metrics::TEXT_LG)).child(
            if draft.existing.is_some() {
                "Edit monitoring profile"
            } else {
                "Monitor a location"
            },
        ))
        .child(div().text_color(palette.text_dim).child("Location"))
        .child(if let Some(profile) = &draft.existing {
            div()
                .child(format::printable(&profile.location.root().to_string_lossy()).into_owned())
                .into_any_element()
        } else {
            ui::dialog_input("monitor-root", palette, &draft.root, cx).into_any_element()
        })
        .child(cadence)
        .child(
            div()
                .text_color(palette.text_dim)
                .child("Allocated growth threshold (bytes)"),
        )
        .child(ui::dialog_input(
            "monitor-growth",
            palette,
            &draft.growth,
            cx,
        ))
        .child(
            div()
                .text_color(palette.text_dim)
                .child("Low free space threshold (bytes)"),
        )
        .child(ui::dialog_input(
            "monitor-low-space",
            palette,
            &draft.low_space,
            cx,
        ))
        .child(
            h_flex()
                .gap(px(8.))
                .children((!working).then(|| {
                    ui::button("monitor-save", palette)
                        .child("Save profile")
                        .on_click(cx.listener(|app, _, _, cx| app.save_monitoring_draft(cx)))
                }))
                .children((!working).then(|| {
                    ui::button("monitor-cancel-edit", palette)
                        .child("Cancel")
                        .on_click(cx.listener(|app, _, _, cx| {
                            if let Some(view) = app.monitoring.as_mut() {
                                view.draft = None;
                                view.message = None;
                                cx.notify();
                            }
                        }))
                })),
        )
        .into_any_element()
}

pub(super) fn render(view: &View, palette: &Palette, cx: &mut Context<KoshaApp>) -> AnyElement {
    let working = view.job.is_some();
    let mut panel = v_flex()
        .id("monitoring-panel")
        .size_full()
        .overflow_y_scroll()
        .p(px(16.))
        .gap(px(12.))
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(metrics::scaled(metrics::TEXT_XL))
                        .child("Monitoring"),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .children((!working && view.draft.is_none()).then(|| {
                            ui::button("monitoring-add", palette)
                                .child("Add location…")
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.draft_monitoring(None, window, cx)
                                }))
                        }))
                        .child(
                            ui::button("monitoring-close", palette)
                                .child("Done")
                                .on_click(cx.listener(|app, _, _, cx| app.close_monitoring(cx))),
                        ),
                ),
        )
        .children(
            view.message
                .as_ref()
                .map(|message| div().text_color(palette.text_dim).child(message.clone())),
        );
    if let Some(draft) = &view.draft {
        return panel
            .child(render_draft(draft, working, palette, cx))
            .into_any_element();
    }
    if view.rows.is_empty() && view.message.is_none() {
        panel = panel.child(
            div()
                .text_color(palette.text_dim)
                .child("No monitored locations yet."),
        );
    }
    for row in &view.rows {
        let id = row.profile.id;
        let paused = row.profile.paused;
        let profile = row.profile.clone();
        let controls = h_flex()
            .gap(px(6.))
            .flex_wrap()
            .children((!working).then(|| {
                ui::button(("monitoring-pause", id), palette)
                    .child(if paused { "Resume" } else { "Pause" })
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.monitoring_operation(Operation::Pause(id, !paused), cx)
                    }))
            }))
            .children((!working && !paused).then(|| {
                ui::button(("monitoring-check", id), palette)
                    .child("Check now")
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.monitoring_operation(Operation::Check(id), cx)
                    }))
            }))
            .children((!working).then(|| {
                ui::button(("monitoring-edit", id), palette)
                    .child("Edit…")
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.draft_monitoring(Some(profile.clone()), window, cx)
                    }))
            }))
            .children((!working).then(|| {
                ui::button(("monitoring-remove", id), palette)
                    .child("Remove…")
                    .on_click(
                        cx.listener(move |_, _, window, cx: &mut Context<KoshaApp>| {
                            let prompt = window.prompt(
                                gpui_kit::PromptLevel::Warning,
                                "Stop monitoring this location?",
                                Some("Future checks stop. Saved observations are kept."),
                                &["Cancel", "Remove profile"],
                                cx,
                            );
                            cx.spawn(async move |this, cx| {
                                if prompt.await == Ok(1) {
                                    let _ = this.update(cx, |app, cx| {
                                        app.monitoring_operation(Operation::Remove(id), cx)
                                    });
                                }
                            })
                            .detach();
                        }),
                    )
            }))
            .children(row.comparison.clone().map(|paths| {
                ui::button(("monitoring-compare", id), palette)
                    .child("Compare changes")
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.close_monitoring(cx);
                        app.folder_comparison =
                            Some(crate::folder_comparison::View::from_snapshots(
                                paths.clone(),
                                "Monitoring observation",
                            ));
                        app.compare_folders(cx);
                    }))
            }));
        panel = panel.child(
            v_flex()
                .p(px(12.))
                .gap(px(7.))
                .border_1()
                .border_color(palette.hairline)
                .child(div().child(
                    format::printable(&row.profile.location.root().to_string_lossy()).into_owned(),
                ))
                .child(div().text_color(palette.text_dim).child(row.status.clone()))
                .children(
                    row.alert
                        .as_ref()
                        .map(|alert| div().text_color(palette.caution).child(alert.clone())),
                )
                .child(controls),
        );
    }
    panel.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_monitoring_lifecycle_keeps_observations_after_profile_removal() {
        let storage = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let execute = |operation| run(storage.path(), operation, Arc::default()).unwrap();
        assert!(execute(Operation::Refresh).0.is_empty());

        let limit = Thresholds::Growth(NonZeroU64::new(1024).unwrap());
        let (rows, _) = execute(Operation::Save(
            None,
            fixture.path().to_owned(),
            Cadence::Daily,
            limit,
        ));
        let id = rows[0].profile.id;
        assert!(rows[0].status.contains("No checks yet"));
        assert_eq!(
            execute(Operation::Check(id)).1.as_deref(),
            Some("Check completed.")
        );

        std::fs::write(fixture.path().join("new.bin"), vec![b'x'; 65536]).unwrap();
        let (rows, _) = execute(Operation::Check(id));
        assert!(
            rows[0]
                .alert
                .as_deref()
                .unwrap()
                .contains("allocated growth")
        );
        let [before, after] = rows[0].comparison.clone().unwrap();
        assert_ne!(before, after);
        assert!(rows[0].status.contains("Last saved"));

        let (rows, _) = execute(Operation::Pause(id, true));
        assert!(rows[0].status.contains("Paused"));
        assert_eq!(
            execute(Operation::Check(id)).1.as_deref(),
            Some("Resume this profile before checking.")
        );
        let (rows, _) = execute(Operation::Save(
            Some(id),
            fixture.path().to_owned(),
            Cadence::Weekly,
            limit,
        ));
        assert!(rows[0].status.starts_with("Weekly · Paused"));

        let (rows, message) = execute(Operation::Remove(id));
        assert!(rows.is_empty());
        assert_eq!(
            message.as_deref(),
            Some("Profile removed. Saved observations were kept.")
        );
        assert!(before.is_file() && after.is_file());
        assert!(
            matches!(run(storage.path(), Operation::Check(id), Arc::default()), Err(error) if error.to_string().contains("profile was removed"))
        );
    }

    #[test]
    fn schedule_checks_due_profiles_once_and_skips_paused_profiles() {
        let storage = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let dir = storage.path();
        let limit = Thresholds::Growth(NonZeroU64::new(1024).unwrap());
        let mut lease = MonitoringLease::try_acquire(dir).unwrap();
        let due = lease
            .add_profile(Location::pin(first.path()).unwrap(), Cadence::Daily, limit)
            .unwrap();
        let paused = lease
            .add_profile(Location::pin(second.path()).unwrap(), Cadence::Daily, limit)
            .unwrap();
        lease
            .update_profile(paused.id, Cadence::Daily, limit, true)
            .unwrap();
        drop(lease);
        let cancelled = ScheduledCheck::default();
        cancelled.cancel.store(true, Ordering::Relaxed);
        assert_eq!(run_due(dir, &cancelled).unwrap(), (false, 0));
        assert_eq!(run_due(dir, &ScheduledCheck::default()).unwrap(), (true, 0));
        assert_eq!(
            run_due(dir, &ScheduledCheck::default()).unwrap(),
            (false, 0)
        );
        assert_eq!(store::list_monitoring_in(dir, due.id).unwrap().len(), 1);
        assert!(
            store::list_monitoring_in(dir, paused.id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn desktop_threshold_fields_require_at_least_one_positive_value() {
        assert_eq!(
            parse_thresholds(" 128 ", "").unwrap(),
            Thresholds::Growth(NonZeroU64::new(128).unwrap())
        );
        assert!(matches!(
            parse_thresholds("1", "2"),
            Ok(Thresholds::Both { .. })
        ));
        for (growth, low_space) in [("", ""), ("0", "128"), ("-1", ""), ("", "1.5")] {
            assert!(parse_thresholds(growth, low_space).is_err());
        }
    }
}
