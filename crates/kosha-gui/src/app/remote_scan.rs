// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use super::*;
use kosha_core::snapshot::RemoteRecord;
use kosha_remote::{PreparedTarget, key_input};
use kosha_remote::{ScanCancellation, ScanHandle, scan_cancellation};
use kosha_scan_wire::Progress as SshProgress;
use std::sync::Mutex;

pub(super) enum RemoteScan {
    Editing {
        host: Entity<InputState>,
        path: Entity<InputState>,
        agent_key: Option<Entity<InputState>>,
        error: Option<String>,
    },
    Preparing(Arc<()>),
    Review {
        target: PreparedTarget,
        folder: String,
        host: Entity<InputState>,
        path: Entity<InputState>,
        agent_key: Option<Entity<InputState>>,
        error: Option<String>,
    },
    Running {
        _cancel: ScanHandle,
        progress: Option<SshProgress>,
    },
}

impl KoshaApp {
    pub(super) fn open_remote_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.ssh_access {
            self.state.notice =
                Some("Enable SSH access in Settings to scan a remote folder.".into());
        } else if self.remote_scan.is_none() {
            let host =
                cx.new(|cx| InputState::new(window, cx).placeholder("Alias or user@host:port"));
            let path = cx.new(|cx| InputState::new(window, cx).placeholder("Remote folder path"));
            let agent_key = cfg!(any(unix, windows)).then(|| {
                cx.new(|cx| InputState::new(window, cx).placeholder("Absolute path to public key"))
            });
            host.read(cx).focus_handle(cx).focus(window, cx);
            self.remote_scan = Some(RemoteScan::Editing {
                host,
                path,
                agent_key,
                error: None,
            });
        }
        cx.notify();
    }

    fn cancel_remote_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remote_scan = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn handle_remote_scan_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(state) = &self.remote_scan else {
            return false;
        };
        let key = &event.keystroke;
        let submit = !event.is_held && !key.modifiers.modified();
        match (key.key.as_str(), state) {
            ("escape", _) => self.cancel_remote_scan(window, cx),
            ("tab", RemoteScan::Editing { host, path, .. }) => {
                let host = host.read(cx).focus_handle(cx);
                let path = path.read(cx).focus_handle(cx);
                if host.is_focused(window) {
                    path.focus(window, cx);
                } else {
                    host.focus(window, cx);
                }
            }
            (
                "tab",
                RemoteScan::Review {
                    agent_key: Some(input),
                    ..
                },
            ) => {
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            ("tab", _) => {}
            ("enter", RemoteScan::Editing { .. }) if submit => {
                self.prepare_remote_scan(window, cx);
            }
            ("enter", RemoteScan::Review { .. }) if submit => {
                self.start_remote_scan(window, cx);
            }
            _ => return false,
        }
        true
    }

    fn prepare_remote_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.ssh_access {
            self.remote_scan = None;
            return;
        }
        let Some(RemoteScan::Editing {
            host,
            path,
            agent_key,
            error,
        }) = &mut self.remote_scan
        else {
            return;
        };
        let input = host.read(cx).value().trim().to_owned();
        let folder = path.read(cx).value().to_string();
        if input.is_empty() || folder.trim().is_empty() {
            *error = Some("Enter a host and a remote folder path.".into());
            cx.notify();
            return;
        }
        let (host, path) = (host.clone(), path.clone());
        let agent_key = agent_key.clone();
        let ticket = Arc::new(());
        self.remote_scan = Some(RemoteScan::Preparing(ticket.clone()));
        window.focus(&self.focus, cx);
        self.state.notice = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let config = kosha_core::dirs::config_dir()
                .ok_or_else(|| anyhow::anyhow!("Kosha settings directory is unavailable"))?;
            Ok::<_, anyhow::Error>(kosha_remote::prepare_local_target(true, &input, &config)?)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !this.settings.ssh_access
                    || !matches!(&this.remote_scan, Some(RemoteScan::Preparing(active)) if Arc::ptr_eq(active, &ticket))
                {
                    return;
                }
                this.remote_scan = Some(match result {
                    Ok(target) => RemoteScan::Review { target, folder, host, path, agent_key, error: None },
                    Err(error) => {
                        RemoteScan::Editing {
                            host, path, agent_key,
                            error: Some(format!("SSH configuration could not be reviewed: {error}")),
                        }
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn start_remote_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.ssh_access {
            self.remote_scan = None;
            return;
        }
        let Some(RemoteScan::Review {
            agent_key, error, ..
        }) = &mut self.remote_scan
        else {
            return;
        };
        let agent_key = agent_key.as_ref().and_then(|input| {
            let value = input.read(cx).value();
            let value = value.trim();
            (!value.is_empty()).then(|| PathBuf::from(value))
        });
        if agent_key.as_ref().is_some_and(|path| !path.is_absolute()) {
            *error = Some("Enter an absolute path to an SSH public key.".into());
            cx.notify();
            return;
        }
        let Some(RemoteScan::Review {
            target,
            folder,
            host,
            path,
            agent_key: agent_input,
            ..
        }) = self.remote_scan.take()
        else {
            unreachable!();
        };
        self.disarm();
        self.state.pending = None;
        self.state.cancel_scan();
        if self.is_scanning() {
            self.state.phase = Phase::Idle;
        }
        self.cancel_search();
        self.load_generation += 1;
        self.comparison_generation += 1;
        self.comparison_busy = false;
        let generation = self.load_generation;
        let (cancel, cancelled) = scan_cancellation();
        self.remote_scan = Some(RemoteScan::Running {
            _cancel: cancel,
            progress: None,
        });
        self.state.notice = None;
        window.focus(&self.focus, cx);
        cx.notify();

        let latest = Arc::new(Mutex::new(None));
        let worker_progress = Arc::clone(&latest);
        let task = cx.background_executor().spawn(async move {
            run(target, folder, agent_key, cancelled, |progress| {
                *worker_progress.lock().unwrap() = Some(progress);
            })
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let current = *latest.lock().unwrap();
                let active = this
                    .update(cx, |this, cx| {
                        if this.load_generation != generation {
                            return false;
                        }
                        let Some(RemoteScan::Running { progress, .. }) = this.remote_scan.as_mut()
                        else {
                            return false;
                        };
                        if *progress != current {
                            *progress = current;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.load_generation != generation
                    || !this.settings.ssh_access
                    || !matches!(this.remote_scan, Some(RemoteScan::Running { .. }))
                {
                    return;
                }
                this.remote_scan = None;
                match result {
                    Ok((record, key_readable_by_others)) => {
                        let ready = Ready::from(record);
                        this.search_index = None;
                        this.state.session = kosha_fsops::Session::new(ready.tree.root_path());
                        this.state.phase = Phase::Ready(ready);
                        this.state.notice = key_readable_by_others.then(|| "The SSH private key is readable by group or others. Use chmod 600 to restrict access.".into());
                        this.apply_pending(cx);
                        this.refresh_insights(cx);
                    }
                    Err(error) => {
                        this.remote_scan = Some(RemoteScan::Editing {
                            host,
                            path,
                            agent_key: agent_input,
                            error: Some(format!("SSH scan failed: {error}")),
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_remote_scan(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.remote_scan.as_ref()?;
        let cancel = crate::ui::button("cancel-ssh-scan", palette)
            .child("Cancel")
            .on_click(cx.listener(|this, _, window, cx| this.cancel_remote_scan(window, cx)));
        let (title, body) = match state {
            RemoteScan::Editing {
                host, path, error, ..
            } => (
                "Scan a folder over SSH",
                v_flex()
                    .gap(px(8.0))
                    .child("SSH host")
                    .child(crate::ui::dialog_input("ssh-host", palette, host, cx))
                    .child("Remote folder")
                    .child(crate::ui::dialog_input("ssh-folder", palette, path, cx))
                    .child(
                        h_flex().justify_end().gap(px(6.0)).child(cancel).child(
                            crate::ui::primary("review-ssh-scan", palette, false)
                                .child("Review connection")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.prepare_remote_scan(window, cx)
                                })),
                        ),
                    )
                    .when_some(error.clone(), |form, error| {
                        form.child(div().w_full().whitespace_normal().child(error))
                    }),
            ),
            RemoteScan::Preparing(_) => (
                "Scan a folder over SSH",
                v_flex()
                    .gap(px(8.0))
                    .child("Reading SSH configuration…")
                    .child(h_flex().justify_end().child(cancel)),
            ),
            RemoteScan::Review {
                target,
                folder,
                agent_key,
                error,
                ..
            } => {
                let target = target.target();
                let mut details = v_flex()
                    .gap(px(8.0))
                    .child(format!(
                        "Host {} · Port {} · User {}",
                        target.host, target.port, target.user
                    ))
                    .child(
                        div()
                            .whitespace_normal()
                            .child(format!("Remote folder {folder}")),
                    )
                    .child(format!("SSH key files ({})", target.identity_files.len()));
                for (index, identity) in target.identity_files.iter().take(5).enumerate() {
                    let identity = identity.display().to_string();
                    details = details.child(
                        div()
                            .id(("ssh-key-path", index))
                            .truncate()
                            .child(identity.clone())
                            .tooltip(move |window, cx| {
                                crate::ui::tooltip(identity.clone()).build(window, cx)
                            }),
                    );
                }
                if target.identity_files.len() > 5 {
                    details = details.child(format!(
                        "{} more key files",
                        target.identity_files.len() - 5
                    ));
                }
                for (line, option) in target.ignored.iter().take(3) {
                    details = details.child(format!("Ignored SSH option {option} (line {line})"));
                }
                if target.ignored.len() > 3 {
                    details = details.child(format!(
                        "{} more ignored SSH options",
                        target.ignored.len() - 3
                    ));
                }
                if let Some(input) = agent_key {
                    details = details
                        .child("SSH agent public key (optional)")
                        .child(crate::ui::dialog_input("ssh-agent-key", palette, input, cx))
                        .child("Leave blank to use the configured private key files.");
                }
                (
                    "Review SSH connection",
                    details
                        .when_some(error.clone(), |form, error| {
                            form.child(div().w_full().whitespace_normal().child(error))
                        })
                        .child(
                            h_flex()
                                .justify_end()
                                .gap(px(6.0))
                                .child(cancel)
                                .child(
                                    crate::ui::button("edit-ssh-scan", palette)
                                        .child("Back")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if let Some(RemoteScan::Review {
                                                host,
                                                path,
                                                agent_key,
                                                ..
                                            }) = this.remote_scan.take()
                                            {
                                                host.read(cx).focus_handle(cx).focus(window, cx);
                                                this.remote_scan = Some(RemoteScan::Editing {
                                                    host,
                                                    path,
                                                    agent_key,
                                                    error: None,
                                                });
                                                this.state.notice = None;
                                                cx.notify();
                                            }
                                        })),
                                )
                                .child(
                                    crate::ui::primary("start-ssh-scan", palette, false)
                                        .child("Scan")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.start_remote_scan(window, cx)
                                        })),
                                ),
                        ),
                )
            }
            RemoteScan::Running { progress, .. } => {
                let label = progress.map_or_else(
                    || "Scanning over SSH…".into(),
                    |progress| {
                        format!(
                            "Scanning over SSH… {} files · {} folders",
                            format::count(progress.files),
                            format::count(progress.directories)
                        )
                    },
                );
                (
                    "Scan a folder over SSH",
                    v_flex()
                        .gap(px(8.0))
                        .child(label)
                        .child(h_flex().justify_end().child(cancel)),
                )
            }
        };
        Some(
            crate::ui::scrim("ssh-scan-scrim", palette)
                .items_center()
                .justify_center()
                .child(
                    crate::ui::dialog("ssh-scan-dialog", palette)
                        .debug_selector(|| "ssh-scan-dialog".into())
                        .w(px(560.0))
                        .max_w(relative(0.92))
                        .max_h(relative(0.9))
                        .child(div().text_size(px(metrics::TEXT_LG)).child(title))
                        .child(
                            div()
                                .id("ssh-scan-content")
                                .min_h(px(0.0))
                                .overflow_y_scroll()
                                .child(body),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "headless-ui-tests", debug_assertions))]
    #[test]
    fn headless_source_dialogs_center_and_cancel() {
        use gpui_kit::{TestAppContext, VisualTestContext};

        fn assert_centered(visual: &mut VisualTestContext, selector: &'static str) {
            visual.refresh().unwrap();
            visual.run_until_parked();
            let center = visual.debug_bounds(selector).unwrap().center();
            let viewport = visual.update(|window, _| window.viewport_size());
            assert!((center.x - viewport.width * 0.5).abs() <= px(1.0));
            assert!((center.y - viewport.height * 0.5).abs() <= px(1.0));
        }

        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = cx.add_window(|window, cx| {
            KoshaApp::new(
                window,
                crate::settings::Loaded {
                    settings: crate::settings::Settings {
                        ssh_access: true,
                        ..Default::default()
                    },
                    problem: None,
                },
                Start::default(),
                None,
                cx,
            )
        });
        let mut visual = VisualTestContext::from_window(app.into(), &cx);
        app.update(&mut cx, |app, window, cx| app.open_remote_scan(window, cx))
            .unwrap();
        assert_centered(&mut visual, "ssh-scan-dialog");
        visual.simulate_keystrokes("tab enter");
        app.update(&mut cx, |app, window, cx| {
            let Some(RemoteScan::Editing { path, error, .. }) = &app.remote_scan else {
                panic!("the dialog skipped validation");
            };
            assert!(path.read(cx).focus_handle(cx).is_focused(window));
            assert!(error.is_some());
        })
        .unwrap();
        visual.simulate_keystrokes("escape");
        let progress = Arc::new(ListingProgress {
            cancelled: AtomicBool::new(false),
            bytes: AtomicU64::new(0),
        });
        app.update(&mut cx, |app, window, cx| {
            assert!(app.remote_scan.is_none());
            assert!(app.focus.is_focused(window));
            app.listing_import = Some(ListingImport::Reading(Arc::clone(&progress)));
            cx.notify();
        })
        .unwrap();
        assert_centered(&mut visual, "listing-import-dialog");
        visual.simulate_keystrokes("escape");
        assert!(progress.cancelled.load(Ordering::Relaxed));
        app.update(&mut cx, |app, window, _| {
            assert!(app.listing_import.is_none());
            assert!(app.focus.is_focused(window));
        })
        .unwrap();

        let imported = kosha_import::import_auto(
            &b"[]"[..],
            &kosha_import::RcloneSource {
                source_name: "synthetic.json",
                label: "",
                root: "",
                tool: "",
                observed: None,
            },
            std::time::UNIX_EPOCH,
            &kosha_import::Limits::default(),
        )
        .unwrap();
        app.update(&mut cx, |app, _, cx| {
            app.listing_import = Some(ListingImport::Preview(Box::new(imported)));
            cx.notify();
        })
        .unwrap();
        assert_centered(&mut visual, "listing-import-dialog");
        visual.simulate_keystrokes("escape");
        app.update(&mut cx, |app, _, _| assert!(app.listing_import.is_none()))
            .unwrap();
    }

    #[test]
    fn cancelled_request_returns_before_connecting() {
        let (sender, cancelled) = scan_cancellation();
        drop(sender);
        let home = tempfile::tempdir().unwrap();
        let target =
            kosha_remote::prepare_target(true, "host.invalid", home.path(), "user", &[]).unwrap();
        let outcome = run(target, String::new(), None, cancelled, |_| unreachable!());
        assert_eq!(outcome.err().unwrap().to_string(), "cancelled");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn missing_agent_public_key_returns_before_connecting() {
        let (_sender, cancelled) = scan_cancellation();
        let home = tempfile::tempdir().unwrap();
        let target =
            kosha_remote::prepare_target(true, "host.invalid", home.path(), "user", &[]).unwrap();
        let error = run(
            target,
            "/fixture".into(),
            Some(home.path().join("missing.pub")),
            cancelled,
            |_| unreachable!(),
        )
        .err()
        .unwrap();
        assert!(matches!(
            error.downcast_ref::<kosha_remote::AuthenticationError>(),
            Some(kosha_remote::AuthenticationError::AgentKey(
                key_input::Error::Read(io::ErrorKind::NotFound)
            ))
        ));
    }
}

fn run(
    target: PreparedTarget,
    path: String,
    agent_key: Option<PathBuf>,
    cancelled: ScanCancellation,
    progress: impl FnMut(SshProgress),
) -> anyhow::Result<(RemoteRecord, bool)> {
    use anyhow::bail;

    cancelled.run(async {
        #[cfg(not(any(unix, windows)))]
        if agent_key.is_some() {
            bail!("this build does not support SSH agent authentication");
        }
        #[cfg(any(unix, windows))]
        let agent_key = agent_key.as_deref()
            .map(|path| kosha_remote::SelectedAgentKey::load(true, path))
            .transpose()?;
        let (target, mut session) = target.connect_default(true).await?;
        let request = kosha_scan_wire::Request { path, max_depth: None, owners: false, cross_filesystems: false, dedup_hardlinks: true };
        #[cfg(any(unix, windows))]
        if let Some(key) = agent_key {
            let session = key.authenticate(session, &target.user).await?;
            let record = kosha_remote::scan_session(true, &session, &request, progress).await?;
            return Ok((record, false));
        }
        let mut rejected = false;
        for identity in &target.identity_files {
            let loaded = match key_input::load(true, identity, None) {
                Ok(key) => key,
                Err(key_input::Error::Read(io::ErrorKind::NotFound)) => continue,
                Err(key_input::Error::PassphraseRequired) if cfg!(unix) => {
                    bail!(
                        "{} requires a passphrase. Unlock the key in your SSH agent and select its public key in the connection review, or run kosha remote from a terminal.",
                        identity.display()
                    );
                }
                Err(key_input::Error::PassphraseRequired) if cfg!(windows) => {
                    bail!(
                        "{} requires a passphrase. Unlock the key in the Windows OpenSSH agent and select its public key in the connection review.",
                        identity.display()
                    );
                }
                Err(error) => return Err(error.into()),
            };
            let readable_by_others = loaded.readable_by_others;
            let (returned, accepted) =
                kosha_remote::try_authenticate_key(session, &target.user, Arc::new(loaded.key)).await?;
            if accepted {
                let record = kosha_remote::scan_session(true, &returned, &request, progress).await?;
                return Ok((record, readable_by_others));
            }
            session = returned;
            rejected = true;
        }
        if rejected {
            bail!("SSH server refused the configured keys");
        }
        bail!("no configured SSH private key was found")
    })?
    .ok_or_else(|| anyhow::anyhow!("cancelled"))?
}
