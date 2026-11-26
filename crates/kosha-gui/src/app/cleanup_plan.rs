// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::{h_flex, input::InputState, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Task, Window, div, px,
};
use kosha_analyze::{Insight, Suggested};
use kosha_core::{NodeId, SizeTree, Strategy, format};
use kosha_fsops::{
    Action,
    workflows::{self, Workflow},
};

use super::KoshaApp;
use crate::theme::{Palette, metrics};
use crate::ui;

#[derive(Default)]
pub(super) struct Workflows {
    saved: BTreeMap<String, Workflow>,
    draft: Option<WorkflowDraft>,
    message: Option<String>,
    task: Option<Task<()>>,
}

struct WorkflowDraft {
    original: Option<(String, Workflow)>,
    name: Entity<InputState>,
    root: Entity<InputState>,
    files: Vec<PathBuf>,
}

pub(super) fn render_workflows(
    view: &Workflows,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let mut panel = v_flex()
        .id("saved-cleanup-workflows")
        .w_full()
        .max_w(px(680.))
        .gap(px(12.))
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(metrics::scaled(metrics::TEXT_XL))
                        .child("Saved cleanup workflows"),
                )
                .child(
                    ui::button("close-workflows", palette)
                        .child("Done")
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.cleanup_workflows = None;
                            cx.notify();
                        })),
                ),
        )
        .children(
            view.message
                .as_ref()
                .map(|message| ui::note(palette, message.clone())),
        );
    if view.task.is_none() {
        if let Some(draft) = &view.draft {
            panel = panel.child(ui::note(
                palette,
                "Choose individual files inside one local folder. Each run requires confirmation.",
            ));
            if let Some((name, workflow)) = &draft.original {
                panel = panel
                    .child(div().child(name.clone()))
                    .child(div().child(workflow.location().root().display().to_string()));
            } else {
                panel = panel.child(ui::dialog_input("workflow-name", palette, &draft.name, cx));
                if draft.files.is_empty() {
                    panel =
                        panel.child(ui::dialog_input("workflow-root", palette, &draft.root, cx));
                } else {
                    panel = panel.child(div().child(draft.root.read(cx).value().to_string()));
                }
            }
            for (index, file) in draft.files.iter().enumerate() {
                panel = panel.child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .child(file.display().to_string()),
                        )
                        .child(
                            ui::button(("workflow-remove-file", index), palette)
                                .child("Remove")
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    if let Some(draft) = app
                                        .cleanup_workflows
                                        .as_mut()
                                        .and_then(|view| view.draft.as_mut())
                                        && index < draft.files.len()
                                    {
                                        draft.files.remove(index);
                                        cx.notify();
                                    }
                                })),
                        ),
                );
            }
            panel = panel.child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        ui::button("workflow-add-files", palette)
                            .child("Add files…")
                            .on_click(cx.listener(|app, _, _, cx| app.add_workflow_files(cx))),
                    )
                    .child(
                        ui::button("workflow-cancel-edit", palette)
                            .child("Cancel")
                            .on_click(cx.listener(|app, _, _, cx| {
                                if let Some(view) = app.cleanup_workflows.as_mut() {
                                    view.draft = None;
                                    view.message = None;
                                    cx.notify();
                                }
                            })),
                    )
                    .children((!draft.files.is_empty()).then(|| {
                        ui::button("workflow-save", palette)
                            .child("Save workflow")
                            .on_click(cx.listener(|app, _, _, cx| app.save_workflow(cx)))
                    })),
            );
        } else {
            panel = panel.child(ui::note(
                palette,
                "Review the same local files again after a fresh scan.",
            ));
            if view.saved.is_empty() {
                panel = panel.child(ui::note(palette, "No saved cleanup workflows yet."));
            }
            for (index, (name, workflow)) in view.saved.iter().enumerate() {
                let edit = name.clone();
                let review = name.clone();
                let definition = workflow.clone();
                panel = panel.child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .p(px(12.))
                        .rounded(px(metrics::RADIUS_SM))
                        .bg(palette.surface)
                        .child(
                            v_flex()
                                .min_w(px(0.))
                                .child(div().truncate().child(name.clone()))
                                .child(
                                    div()
                                        .truncate()
                                        .text_color(palette.text_dim)
                                        .child(workflow.location().root().display().to_string()),
                                )
                                .child(
                                    div()
                                        .text_color(palette.text_dim)
                                        .child(format::plural(workflow.targets().len(), "file")),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(
                                    ui::button(("workflow-edit", index), palette)
                                        .child("Edit…")
                                        .on_click(cx.listener(move |app, _, window, cx| {
                                            app.edit_workflow(Some(edit.clone()), window, cx);
                                        })),
                                )
                                .child(
                                    ui::button(("workflow-review", index), palette)
                                        .child("Review…")
                                        .on_click(cx.listener(move |app, _, _, cx| {
                                            app.review_workflow(&review, definition.clone(), cx);
                                        })),
                                ),
                        ),
                );
            }
            panel = panel.child(
                ui::button("workflow-create", palette)
                    .child("New workflow…")
                    .on_click(
                        cx.listener(|app, _, window, cx| app.edit_workflow(None, window, cx)),
                    ),
            );
        }
    }
    v_flex()
        .id("workflow-scroll")
        .size_full()
        .overflow_y_scroll()
        .items_center()
        .p(px(20.))
        .child(panel)
        .into_any_element()
}

impl KoshaApp {
    pub(super) fn open_cleanup_workflows(&mut self, cx: &mut Context<Self>) {
        if self.state.pending.is_some() || self.planning_action.is_some() {
            return;
        }
        self.disarm();
        self.cleanup_workflows = Some(Workflows::default());
        self.load_workflows(None, cx);
    }

    fn load_workflows(
        &mut self,
        save: Option<(String, PathBuf, Vec<PathBuf>, Option<Workflow>)>,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self
            .cleanup_workflows
            .as_mut()
            .filter(|view| view.task.is_none())
        else {
            return;
        };
        let saving = save.is_some();
        view.message = Some(
            if saving {
                "Checking selected files…"
            } else {
                "Loading workflows…"
            }
            .into(),
        );
        view.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let dir = workflows::directory().ok_or_else(|| {
                        std::io::Error::other("Cleanup workflow storage is unavailable.")
                    })?;
                    if let Some((name, root, paths, original)) = save {
                        let workflow = match &original {
                            Some(original) => original.with_targets(paths)?,
                            None => Workflow::new(&root, paths)?,
                        };
                        workflow.preview()?;
                        workflows::save(&dir, &name, workflow, original.as_ref())?;
                    }
                    workflows::list(&dir)
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                if let Some(view) = app.cleanup_workflows.as_mut() {
                    view.task = None;
                    match result {
                        Ok(saved) => {
                            view.saved = saved;
                            view.draft = None;
                            view.message = saving.then(|| "Workflow saved.".into());
                        }
                        Err(error) => view.message = Some(error.to_string()),
                    }
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn edit_workflow(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.cleanup_workflows.as_mut() else {
            return;
        };
        let original = name.and_then(|name| {
            view.saved
                .get(&name)
                .cloned()
                .map(|workflow| (name, workflow))
        });
        let root = original
            .as_ref()
            .map(|(_, workflow)| workflow.location().root())
            .or_else(|| {
                self.state
                    .ready()
                    .filter(|ready| !ready.source.is_past())
                    .map(|ready| ready.tree.root_path())
            });
        let mut field = |placeholder: &'static str, value: String| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_value(value, window, cx);
                input
            })
        };
        let name = field(
            "Workflow name",
            original
                .as_ref()
                .map(|(name, _)| name.clone())
                .unwrap_or_default(),
        );
        let files = original
            .as_ref()
            .map(|(_, workflow)| workflow.targets().to_vec())
            .unwrap_or_default();
        view.draft = Some(WorkflowDraft {
            name,
            root: field(
                "Absolute local folder path",
                root.map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            original,
            files,
        });
        view.message = None;
        cx.notify();
    }

    fn add_workflow_files(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self
            .cleanup_workflows
            .as_ref()
            .and_then(|view| view.draft.as_ref())
        else {
            return;
        };
        let root = PathBuf::from(draft.root.read(cx).value().to_string());
        let field = draft.name.clone();
        let prompt = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add files".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            let _ = this.update(cx, |app, cx| {
                let Some(paths) = app.file_picker_selection(selection, cx) else {
                    return;
                };
                let Some(view) = app.cleanup_workflows.as_mut() else {
                    return;
                };
                let Some(draft) = view.draft.as_mut().filter(|draft| draft.name == field) else {
                    return;
                };
                if std::path::Path::new(&*draft.root.read(cx).value()) != root {
                    view.message =
                        Some("The workflow folder changed. Choose the files again.".into());
                    cx.notify();
                    return;
                }
                let result: Result<Vec<_>, _> = paths
                    .iter()
                    .map(|path| path.strip_prefix(&root).map(|path| path.to_path_buf()))
                    .collect();
                match result {
                    Ok(paths) => {
                        for path in paths {
                            if !draft.files.contains(&path) {
                                draft.files.push(path);
                            }
                        }
                        view.message = None;
                    }
                    Err(_) => {
                        view.message =
                            Some("Choose files inside the workflow’s original folder.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_workflow(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self
            .cleanup_workflows
            .as_ref()
            .and_then(|view| view.draft.as_ref())
        else {
            return;
        };
        let (name, original) = match &draft.original {
            Some((name, workflow)) => (name.clone(), Some(workflow.clone())),
            None => (draft.name.read(cx).value().to_string(), None),
        };
        let root = PathBuf::from(draft.root.read(cx).value().to_string());
        self.load_workflows(Some((name, root, draft.files.clone(), original)), cx);
    }

    fn review_workflow(&mut self, name: &str, workflow: Workflow, cx: &mut Context<Self>) {
        if self.planning_action.is_some() || self.state.pending.is_some() {
            return;
        }
        if !self.state.ready().is_some_and(|ready| {
            !ready.source.is_past()
                && kosha_core::snapshot::store::same_root(
                    self.state.session.root(),
                    workflow.location().root(),
                )
        }) {
            if !workflow.location().is_current() {
                if let Some(view) = self.cleanup_workflows.as_mut() {
                    view.message =
                        Some("The original workflow location is unavailable or changed.".into());
                    cx.notify();
                }
                return;
            }
            self.cleanup_workflows = None;
            self.scan(workflow.location().root().to_path_buf(), cx);
            self.state.notice =
                Some("Open Saved cleanup workflows after the scan to review this workflow.".into());
            return;
        }
        if !self.state.mode().is_read_write() {
            if let Some(view) = self.cleanup_workflows.as_mut() {
                view.message =
                    Some("Switch to Read-write in the title bar before reviewing cleanup.".into());
                cx.notify();
            }
            return;
        }
        let Some(tree) = self.state.ready().map(|ready| Arc::clone(&ready.tree)) else {
            return;
        };
        self.cleanup_workflows = None;
        self.cleanup_plan_open = false;
        self.state.notice = Some("Preparing workflow preview… Escape cancels.".into());
        let name = name.to_owned();
        self.planning_action = Some(cx.spawn(async move |this, cx| {
            let checked = Arc::clone(&tree);
            let result = cx
                .background_executor()
                .spawn(async move {
                    let plan = workflow.preview()?;
                    let preview = plan.dry_run();
                    if preview.skipped != 0 {
                        return Err(std::io::Error::other(
                            "A target changed during preview. Try again.",
                        ));
                    }
                    Ok(super::PendingAction {
                        title: format!("Move to Trash · {name}"),
                        workflow: Some((name, workflow)),
                        plan,
                        preview,
                        tree: checked,
                    })
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.planning_action = None;
                app.state.notice = None;
                if app.state.mode().is_read_write()
                    && app.state.pending.is_none()
                    && !app.settings_open
                    && !app.help_open
                    && !app.command_open
                    && !app.owners_open
                    && app.state.ready().is_some_and(|ready| {
                        Arc::ptr_eq(&ready.tree, &tree) && !ready.source.is_past()
                    })
                {
                    match result {
                        Ok(pending) => app.state.pending = Some(pending),
                        Err(error) => app.state.notice = Some(error.to_string()),
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

pub(super) struct Draft {
    tree: Arc<SizeTree>,
    action: Action,
    findings: Vec<Finding>,
}

struct Finding {
    title: String,
    nodes: Vec<NodeId>,
}

impl Draft {
    pub(super) fn len(&self) -> usize {
        self.findings.len()
    }

    fn position(&self, insight: &Insight) -> Option<usize> {
        self.findings
            .iter()
            .position(|entry| entry.title == insight.title && entry.nodes == insight.nodes)
    }

    pub(super) fn contains(&self, insight: &Insight) -> bool {
        finding_action(insight) == Some(self.action) && self.position(insight).is_some()
    }
}

fn finding_action(insight: &Insight) -> Option<Action> {
    match insight.action {
        Suggested::Trash => Some(Action::Trash),
        Suggested::EmptyContents => Some(Action::EmptyContents),
        Suggested::Compress if kosha_core::supports_compression() => Some(Action::Compress),
        Suggested::ReviewOnly | Suggested::Compress => None,
    }
}

impl KoshaApp {
    pub(super) fn can_draft_cleanup(&self) -> bool {
        self.state.ready().is_some_and(|ready| {
            !ready.source.is_past()
                && !matches!(
                    ready.tree.stats().strategy,
                    Strategy::DiskImage | Strategy::Imported
                )
        })
    }

    pub(super) fn current_cleanup_draft(&self) -> Option<&Draft> {
        let ready = self.state.ready()?;
        self.cleanup_draft
            .as_ref()
            .filter(|draft| self.can_draft_cleanup() && Arc::ptr_eq(&draft.tree, &ready.tree))
    }

    pub(super) fn toggle_cleanup_finding(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.can_draft_cleanup()
            || self.planning_action.is_some()
            || self.state.pending.is_some()
        {
            return;
        }
        let Some(ready) = self.state.ready() else {
            return;
        };
        let Some(insight) = ready.insights.as_ref().and_then(|items| items.get(index)) else {
            return;
        };
        let Some(action) = finding_action(insight).filter(|_| !insight.nodes.is_empty()) else {
            return;
        };
        let tree = Arc::clone(&ready.tree);
        if self
            .cleanup_draft
            .as_ref()
            .is_some_and(|draft| !Arc::ptr_eq(&draft.tree, &tree))
        {
            self.cleanup_draft = None;
        }
        let draft = self.cleanup_draft.get_or_insert_with(|| Draft {
            tree,
            action,
            findings: Vec::new(),
        });
        if draft.action != action {
            self.state.notice = Some(format!(
                "Current action: {}. Review the plan or remove its findings before adding a different action.",
                draft.action.label()
            ));
        } else if let Some(position) = draft.position(insight) {
            self.remove_cleanup_finding(position, cx);
            return;
        } else {
            draft.findings.push(Finding {
                title: insight.title.clone(),
                nodes: insight.nodes.clone(),
            });
            self.state.notice = None;
        }
        cx.notify();
    }

    fn remove_cleanup_finding(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.planning_action.is_some() || self.state.pending.is_some() {
            return;
        }
        if let Some(draft) = self.cleanup_draft.as_mut()
            && index < draft.findings.len()
        {
            draft.findings.remove(index);
            if draft.findings.is_empty() {
                self.cleanup_draft = None;
                self.cleanup_plan_open = false;
            }
            self.state.notice = None;
            cx.notify();
        }
    }

    fn preview_cleanup_draft(&mut self, cx: &mut Context<Self>) {
        if !self.state.mode().is_read_write() {
            return;
        }
        let Some(draft) = self.current_cleanup_draft() else {
            return;
        };
        self.prepare_cleanup(
            Arc::clone(&draft.tree),
            draft
                .findings
                .iter()
                .flat_map(|finding| finding.nodes.iter().copied())
                .collect(),
            draft.action,
            format!("Cleanup plan · {}", draft.action.label()),
            cx,
        );
    }
}

pub(super) fn render(
    draft: &Draft,
    read_write: bool,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let mut findings = Vec::with_capacity(draft.len());
    for (index, finding) in draft.findings.iter().enumerate() {
        findings.push(
            h_flex()
                .justify_between()
                .gap(px(12.))
                .p(px(12.))
                .rounded(px(metrics::RADIUS_SM))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.hairline)
                .child(
                    v_flex()
                        .min_w(px(0.))
                        .gap(px(4.))
                        .child(div().truncate().child(finding.title.clone()))
                        .child(
                            div()
                                .text_color(palette.text_dim)
                                .child(format::plural(finding.nodes.len(), "target")),
                        ),
                )
                .child(
                    ui::button(("cleanup-plan-remove", index), palette)
                        .flex_none()
                        .child("Remove")
                        .on_click(
                            cx.listener(move |app, _, _, cx| app.remove_cleanup_finding(index, cx)),
                        ),
                ),
        );
    }
    v_flex()
        .id("cleanup-plan")
        .size_full()
        .overflow_y_scroll()
        .items_center()
        .p(px(20.))
        .child(
            v_flex()
                .w_full()
                .max_w(px(640.))
                .gap(px(14.))
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(div().text_size(metrics::scaled(metrics::TEXT_XL)).child("Cleanup plan"))
                        .child(ui::button("cleanup-plan-close", palette).child("Back to scan").on_click(
                            cx.listener(|app, _, _, cx| {
                                app.cleanup_plan_open = false;
                                cx.notify();
                            })
                        )),
                )
                .child(div().child(format!("{} · {}", draft.action.label(), format::plural(draft.len(), "finding"))))
                .children(findings)
                .child(ui::button("cleanup-plan-saved-workflows", palette)
                    .child("Saved workflows…")
                    .on_click(cx.listener(|app, _, _, cx| app.open_cleanup_workflows(cx))))
                .child(ui::note(palette, "The preview combines overlapping targets and shows the estimated size before you confirm."))
                .when(read_write, |panel| {
                    panel.child(ui::button("cleanup-plan-preview", palette)
                        .child("Preview cleanup…")
                        .on_click(cx.listener(|app, _, _, cx| app.preview_cleanup_draft(cx))))
                })
                .when(!read_write, |panel| {
                    panel.child(ui::note(palette, "Switch to Read-write in the title bar to preview cleanup."))
                }),
        )
        .into_any_element()
}
