// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The root view: orchestration and layout.
//!
//! Two rules govern everything here.
//!
//! **Nothing blocks.** Scanning and analysis run on the background executor and
//! report back through the entity. The window is draggable, resizable and
//! interactive from the first frame, before any data exists.
//!
//! **The render path does no work that scales with the tree.** Rows are sorted
//! once when the focus changes, not once per frame; the table is virtualised so
//! only visible rows are built; and the treemap drops sub-pixel tiles rather
//! than emitting thousands of elements nobody can see.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::ui::tooltip;
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Theme, ThemeMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Animation, AnimationExt, AnyElement, App, AppContext, ClickEvent, Context, CursorStyle, Entity,
    FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, UniformListScrollHandle, Window, div,
    ease_in_out, px, relative, uniform_list,
};

use kosha_analyze::{Confidence, Insight, Suggested};
use kosha_core::{NodeId, Progress, Scanner, SizeTree, format};
use kosha_fsops::{Action, ActionPlan, AuditLog};

use crate::columns::Column;
use crate::commands;
use crate::settings::{Settings, ThemePreference};
use crate::state::{Duplicates, KeyIntent, PendingAction, Phase, Ready, Search, SortKey, State};
use crate::theme::{Category, Palette, metrics};
use crate::treemap::{self, Rect};

/// How fragmented the selected entry is, once somebody has asked.
///
/// The one figure quick look cannot get from the arena: it costs an ioctl per
/// file, so it is measured only for the entry being looked at, and only while
/// the panel is open.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fragments {
    /// The query is in flight. Shown as such rather than as a blank row, so a
    /// slow disk reads as slow rather than as broken.
    Measuring,
    /// Measured. The media type is what decides whether the number is worth
    /// commenting on — `None` when the device would not say.
    Known(kosha_core::Fragmentation, Option<kosha_core::MediaType>),
    /// Asked and refused: a file the user cannot open, a filesystem with no
    /// answer, or a platform that does not do this. The row is dropped rather
    /// than showing a zero, which would read as "in one piece".
    Unavailable,
}

/// A column edge held by the pointer.
///
/// The width is computed from where the drag *started* rather than
/// accumulated from each move, so a pointer that runs past the minimum and
/// comes back lands where it should. Accumulating deltas instead means the
/// clamp eats the overshoot and the column never catches up with the cursor.
#[derive(Clone, Copy, Debug)]
struct ColumnDrag {
    column: Column,
    /// Pointer x when the drag began, in window coordinates.
    origin: f32,
    /// The column's width at that moment.
    width: f32,
}

pub struct KoshaApp {
    state: State,
    /// Discards background results after another location is requested.
    load_generation: u64,
    comparison_generation: u64,
    comparison_busy: bool,
    scan_timing: crate::scan_timing::ScanTiming,
    /// One Arc to the last finished drive scan closes the save/load race
    /// without retaining every drive's full arena in memory.
    last_drive_record: Option<crate::drive_history::DriveRecord>,
    pending_drive_records: crate::drive_history::PendingRecords,
    focus: FocusHandle,
    rows_scroll: UniformListScrollHandle,
    /// Shared by the findings viewport and its draggable scrollbar across frames.
    insights_scroll: gpui_kit::ScrollHandle,
    sidebar_scroll: gpui_kit::ScrollHandle,
    help_scroll: gpui_kit::ScrollHandle,
    settings_scroll: gpui_kit::ScrollHandle,
    settings_search: Entity<InputState>,
    /// Size of the treemap area as of the last frame, so hit testing and layout
    /// agree. Updated from the rendered bounds.
    treemap_bounds: Rect,
    hovered: Option<NodeId>,
    /// The filter box. A real text input rather than a hand-rolled key handler,
    /// because people paste paths into filters and expect a cursor to work.
    filter: Entity<InputState>,
    search_bar: Entity<crate::search_bar::SearchBar>,
    search_recipes: Result<kosha_search::RecipeBook, String>,
    installed_recipe_packs: Vec<kosha_search::RecipePackMetadata>,
    changing_recipe_packs: bool,
    marketplace_catalog: Option<Arc<kosha_marketplace::catalog::Catalog>>,
    syncing_marketplace: bool,
    search_generation: u64,
    search_cancel: Option<Arc<AtomicBool>>,
    search_index: Option<Arc<kosha_search::SearchIndex>>,
    /// Whether the filter row is showing. It stays visible while it holds text,
    /// so a filtered view never looks like an empty directory.
    filter_open: bool,
    settings: Settings,
    /// Why the settings file on disk could not be read, when it could not.
    ///
    /// Kept, not just reported once, because it has to gate every later save.
    /// A file somebody hand-edited into something invalid still holds their
    /// saved searches, their pinned folders and their thresholds; writing
    /// defaults over it destroys all of that to record a preference they did
    /// not even change.
    settings_unusable: Option<String>,
    settings_open: bool,
    /// Whether quick look is showing. It follows the selection rather than
    /// pinning one entry, so there is nothing else to remember.
    details_open: bool,
    /// How fragmented the entry quick look is describing is, and which entry
    /// that answer belongs to.
    ///
    /// Kept on the view rather than inside `details::Details`, which is
    /// deliberately pure arithmetic over the arena with no I/O in it — that is
    /// what makes `Space` instant and the whole of it testable without a
    /// window. This is the one figure on the panel that has to be *asked* for,
    /// so it arrives separately and late, and the panel reads "measuring…"
    /// until it does.
    ///
    /// The path is carried with the answer so a measurement that finishes after
    /// the selection has moved on is discarded rather than shown against the
    /// wrong file.
    fragmentation: Option<(PathBuf, Fragments)>,
    /// The column edge being dragged, if any.
    ///
    /// Held on the view rather than on the header element because a drag
    /// routinely leaves the 6-pixel strip it started on — that is what makes it
    /// a drag — so the moves have to be read at the root, where the pointer
    /// still is.
    column_drag: Option<ColumnDrag>,
    /// A query from the command line, applied once the first scan finishes.
    /// Taken rather than kept, so a later rescan does not silently reapply it.
    pending_query: Option<String>,
    /// Set by `--duplicates`, cleared the first time a scan finishes.
    pending_duplicates: bool,
    /// Set by `--since`, cleared the first time a scan finishes. The snapshot
    /// to hold that scan against.
    pending_since: Option<PathBuf>,
    /// The command palette's own search box. Its own, because it is modal and
    /// the query box behind it must keep whatever was being typed there.
    command_search: Entity<InputState>,
    command_open: bool,
    /// Which row is armed for Enter. Clamped against the match count where it
    /// is used rather than tracked as the list changes: the list is rebuilt on
    /// every keystroke, and a stored index into a list that no longer exists is
    /// how a palette runs the wrong command.
    command_selected: usize,
    /// Keeps the armed row on screen. The palette lists forty-odd commands and
    /// shows nine, so without this the arrow keys walk the selection straight
    /// out of view: nothing is highlighted, and Enter runs something the user
    /// cannot see. The table has guarded against exactly this since it grew a
    /// keyboard; the palette did not.
    command_scroll: gpui_kit::ScrollHandle,
    /// The sidebar's locations, resolved once.
    ///
    /// Finding them means an `is_dir` per candidate and a free-space call per
    /// volume. Doing that per frame — which is what `render_sidebar` used to
    /// do — is filesystem I/O at the refresh rate, on the thread that must
    /// never wait for the disk.
    places: Vec<crate::places::Place>,
    /// The WSL distributions, on Windows. Empty everywhere else, and empty on
    /// a Windows machine without WSL, which is what keeps the heading from
    /// appearing over nothing.
    distributions: Vec<crate::places::Distribution>,
    volumes: Vec<crate::places::Volume>,
    /// Pinned folders, with whether each still exists.
    ///
    /// Cached alongside the rest of the rail because checking existence is a
    /// filesystem call, and a pinned folder can go away — an unmounted drive, a
    /// project directory that was moved. A row that fails the moment it is
    /// clicked is worse than one that says up front that it is gone.
    scopes: Vec<(crate::places::Place, bool)>,
    /// The snapshots kept, oldest first.
    ///
    /// Cached for the same reason the volumes are: the rail is drawn every
    /// frame and listing them is a directory read plus a small read per file.
    /// Refreshed when one is taken or deleted, which are the only two moments
    /// the list can change from inside this window.
    kept_snapshots: Vec<kosha_core::snapshot::store::Kept>,
    /// The files in that directory that are not snapshots this build can open,
    /// each already worded as a line.
    ///
    /// **A damaged snapshot used to vanish from the rail**, which reads exactly
    /// like one that was never taken, while the file sat there occupying its
    /// megabytes. The listing is right to skip it — one bad file must not hide
    /// the nine good ones — and `store::list`'s own note said the window was
    /// where somebody would be told. Nothing carried the news this far.
    snapshot_problems: Vec<SharedString>,
    /// The one remove button that has been pressed once and is waiting to be
    /// pressed again.
    ///
    /// One, not one per kind: arming any of them disarms whichever was armed,
    /// so there is never a second button on screen asking to be pressed. It was
    /// a `PathBuf` and only the snapshots could use it — which is most of why
    /// only the snapshots asked.
    arming: Option<Armed>,
    /// The finished palette, rebuilt when the settings or the theme change.
    ///
    /// Cached rather than derived per frame: finding the desktop accent reads
    /// the registry on Windows and talks to a portal on Linux, and both the
    /// contrast search and the tint are loops. None of that belongs in a
    /// function that runs at the refresh rate.
    palette: Palette,
    mono: SharedString,
    /// The help panel's own search box.
    help_search: Entity<InputState>,
    help_open: bool,
    /// The "who is using the disk" panel. Its own flag rather than a variant of
    /// one overlay enum, matching how settings and help are each their own.
    owners_open: bool,
    /// Recomputed when the query text changes, not per frame: suggesting means
    /// parsing plus a catalogue search, and the render path does no work that
    /// could have been done once.
    suggestions: Vec<kosha_query::Suggestion>,
}

/// Which remove button has been pressed once and is waiting for the second.
///
/// One value rather than one flag per list, so arming any of them disarms
/// whichever was armed. The field it lives in was a `PathBuf`, which only a
/// snapshot could fill — and that is most of why only the snapshot's button
/// asked, and the two in the settings panel fired on the first click behind an
/// identical mark.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Armed {
    Snapshot(PathBuf),
    Pin(PathBuf),
    /// By the query itself, which is what identifies a saved search: two can
    /// share a name, and the name is the query until somebody edits the file.
    Query(String),
}

impl KoshaApp {
    /// `loaded` comes from the caller rather than being read here, because
    /// `open` has to know the saved window geometry before it can open a
    /// window at all. Reading the file twice would work and would also be two
    /// chances to disagree about what is in it.
    pub fn new(
        window: &mut Window,
        loaded: crate::settings::Loaded,
        start: Start,
        theme: Option<ThemeMode>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let search_bar = cx.new(|cx| crate::search_bar::SearchBar::new(window, cx));
        let search_recipes = crate::search_recipes::load();
        if let Ok(book) = &search_recipes {
            search_bar.update(cx, |bar, _| bar.set_recipes(book));
        }
        let filter = search_bar.read(cx).input();
        cx.subscribe(
            &search_bar,
            |this: &mut KoshaApp, _, event, cx| match event {
                crate::search_bar::SearchEvent::Changed(text) => {
                    this.query_text_changed(text.clone(), cx)
                }
                crate::search_bar::SearchEvent::Refresh => {
                    if let Some(ready) = this.state.ready_mut() {
                        ready.now = format::now_unix();
                    }
                    let text = this.filter.read(cx).value().to_string();
                    this.query_text_changed(text, cx);
                }
                crate::search_bar::SearchEvent::Rescan => {
                    if let Some(ready) = this.state.ready() {
                        let root = ready.tree.root_path().to_path_buf();
                        this.scan(root, cx);
                    }
                }
            },
        )
        .detach();

        let help_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("big, old, video, folder, contains…")
        });
        // The panel re-searches as you type, over a list of a few dozen.
        cx.subscribe(
            &help_search,
            |this: &mut KoshaApp, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.help_scroll.set_offset(Default::default());
                    cx.notify();
                }
            },
        )
        .detach();

        let settings_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search settings…"));
        cx.subscribe(
            &settings_search,
            |this: &mut KoshaApp, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.settings_scroll.set_offset(Default::default());
                    cx.notify();
                }
            },
        )
        .detach();

        let command_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type a command: rescan, duplicates, dark\u{2026}")
        });
        // Every keystroke re-ranks a few dozen commands, and the selection goes
        // back to the top: the best match for what has been typed so far is the
        // one Enter should run.
        cx.subscribe(
            &command_search,
            |this: &mut KoshaApp, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.command_selected = 0;
                    cx.notify();
                }
            },
        )
        .detach();

        let scopes = crate::places::scopes(&loaded.settings.saved_scopes);
        // Applied here rather than in `main`, because the saved preference is
        // not known until the settings are read and the command-line flag has
        // to beat it. `startup_theme` is the whole of that rule, and it is
        // tested; this is the one line that acts on it.
        if let Some(mode) = crate::settings::startup_theme(theme, loaded.settings.theme) {
            Theme::change(mode, Some(window), cx);
        }

        // Read after the theme is settled, so a dark session does not get the
        // light theme's font.
        let mono = cx.theme().mono_font_family.clone();
        let palette = Palette::of(cx).with_accent(
            desktop_accent(&loaded.settings, cx),
            loaded.settings.tint_chrome,
        );
        let mut state = State::new();
        state.scan_options = loaded.settings.scan_options();
        // A settings file we could not parse is reported rather than silently
        // replaced, and every save refuses until the user fixes or removes it.
        let settings_unusable = loaded.problem.clone();
        state.notice = loaded.problem;

        if let Some(text) = &start.query {
            search_bar.update(cx, |bar, cx| bar.set_dsl(text.clone(), window, cx));
        }

        let startup_snapshots = kosha_core::snapshot::store::list_all();

        KoshaApp {
            state,
            load_generation: 0,
            comparison_generation: 0,
            comparison_busy: false,
            scan_timing: Default::default(),
            last_drive_record: None,
            pending_drive_records: Default::default(),
            focus,
            // Read once at startup, which is the other moment the list can
            // have changed: another window, or the command line, may have
            // taken one since this one last looked.
            kept_snapshots: startup_snapshots.kept,
            snapshot_problems: problem_lines(&startup_snapshots.unreadable),
            arming: None,
            rows_scroll: UniformListScrollHandle::new(),
            insights_scroll: gpui_kit::ScrollHandle::new(),
            sidebar_scroll: gpui_kit::ScrollHandle::new(),
            help_scroll: gpui_kit::ScrollHandle::new(),
            settings_scroll: gpui_kit::ScrollHandle::new(),
            settings_search,
            treemap_bounds: Rect::new(0.0, 0.0, 800.0, 360.0),
            hovered: None,
            filter,
            search_bar,
            search_recipes,
            installed_recipe_packs: kosha_marketplace::installed().unwrap_or_default(),
            changing_recipe_packs: false,
            marketplace_catalog: kosha_marketplace::catalog::cached().ok().map(Arc::new),
            syncing_marketplace: false,
            search_generation: 0,
            search_cancel: None,
            search_index: None,
            filter_open: start.query.is_some(),
            settings: loaded.settings,
            settings_unusable,
            settings_open: false,
            details_open: false,
            fragmentation: None,
            column_drag: None,
            pending_query: start.query,
            pending_duplicates: start.duplicates,
            pending_since: start.since,
            command_search,
            command_open: false,
            command_selected: 0,
            command_scroll: gpui_kit::ScrollHandle::new(),
            places: crate::places::personal(),
            distributions: crate::places::distributions(),
            volumes: crate::places::volumes(),
            scopes,
            palette,
            mono,
            help_search,
            help_open: false,
            owners_open: false,
            suggestions: Vec::new(),
        }
    }

    // ------------------------------------------------------------ query help

    fn cancel_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        if let Some(cancel) = self.search_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Translate cheaply here, then debounce and evaluate on a worker. Every
    /// publication checks revision, load generation, tree identity and focus.
    fn query_text_changed(&mut self, text: String, cx: &mut Context<Self>) {
        self.cancel_search();
        if !text.trim().is_empty() {
            self.filter_open = true;
        }
        self.suggestions = if self.search_bar.read(cx).mode() == kosha_search::SearchMode::Dsl {
            kosha_query::suggest(&text, 3)
        } else {
            Vec::new()
        };
        let Some(ready) = self.state.ready_mut() else {
            cx.notify();
            return;
        };
        // A newly entered relative phrase refers to the clock when it is
        // entered, even if the loaded scan has been open since yesterday.
        // The worker captures this instant; it never reads a moving clock.
        let now = format::now_unix();
        ready.now = now;
        let local_date = format::local_date(now).unwrap_or_else(|| {
            let (y, m, d) = format::civil_from_days(i64::from(now) / 86400);
            [y as i32, m as i32, d as i32]
        });
        let context = kosha_search::SearchContext::new(now, local_date);
        let mode = self.search_bar.read(cx).mode();
        let resolver = |[y, m, d]: [i32; 3]| {
            format::local_midnight(i64::from(y), m as u32, d as u32).or_else(|| {
                u64::try_from(format::days_from_civil(i64::from(y), m as u32, d as u32) * 86400)
                    .ok()
            })
        };
        let translation = match &self.search_recipes {
            Ok(book) => book.translate_with_resolver(&text, context, mode, resolver),
            Err(problem)
                if mode == kosha_search::SearchMode::Natural && !text.trim().is_empty() =>
            {
                kosha_search::Translation {
                    dsl: None,
                    query: None,
                    entities: Vec::new(),
                    mode,
                    diagnostics: vec![kosha_search::Diagnostic {
                        span: 0..text.len(),
                        message: format!("Cannot load custom recipes: {problem}"),
                        hint: "Fix the recipe file and restart, or switch to DSL or Name mode."
                            .into(),
                    }],
                }
            }
            Err(_) => kosha_search::translate_with_resolver(&text, context, mode, resolver),
        };
        let query = translation.query.clone();
        let dsl = translation.dsl.clone();
        let error = translation.diagnostics.first().map(|d| d.message.clone());
        self.search_bar
            .update(cx, |bar, cx| bar.interpret(translation, cx));
        if text.trim().is_empty() {
            if let Some(ready) = self.state.ready_mut() {
                ready.clear_filter();
            }
            cx.notify();
            return;
        }
        let (Some(query), Some(dsl)) = (query, dsl) else {
            if let Some(ready) = self.state.ready_mut() {
                ready.reject_search(
                    text,
                    error.unwrap_or_else(|| "Enter a complete search.".into()),
                );
            }
            cx.notify();
            return;
        };
        let ready = self.state.ready().expect("tree checked above");
        let tree = Arc::clone(&ready.tree);
        let sort = ready.sort;
        let index = self
            .search_index
            .as_ref()
            .filter(|index| Arc::ptr_eq(index.tree(), &tree))
            .cloned();
        // Release a previous scan's index instead of retaining a second arena.
        if index.is_none() {
            self.search_index = None;
        }
        let needs_creation = crate::search_bar::needs_creation(query.expression());
        let past = ready.source.is_past();
        let incomplete = tree.is_incomplete();
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_cancel = Some(Arc::clone(&cancel));
        self.state
            .ready_mut()
            .expect("tree checked above")
            .begin_search(dsl.clone());
        let ticket = crate::state::SearchTicket::new(
            self.state.ready().expect("tree checked above"),
            self.search_generation,
            self.load_generation,
        );
        cx.notify();

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(180)).await;
            if cancel.load(std::sync::atomic::Ordering::Relaxed) { return; }
            let worker_tree = Arc::clone(&tree);
            let worker_cancel = Arc::clone(&cancel);
            let outcome = cx.background_spawn(async move {
                let index = match index {
                    Some(index) => index,
                    None => Arc::new(kosha_search::SearchIndex::new_cancellable(worker_tree, &worker_cancel)?),
                };
                let results = index.run_sorted(&query, now, 2000, &worker_cancel,
                    |a, b| sort.compare(index.tree(), a, b))?;
                let mut warnings = Vec::new();
                let coverage = index.creation_coverage();
                let creation_unavailable = needs_creation && coverage.total > 0 && coverage.known == 0;
                if needs_creation {
                    if creation_unavailable {
                        warnings.push(if past {
                            format!("Creation dates unavailable: this saved scan recorded none for its {} entries. Scan again to collect available creation dates and rerun this search.", format::count(coverage.total as u64))
                        } else { "Creation dates unavailable in this scan. This source or scan method did not record them; modification dates are a separate field.".into() });
                    } else if coverage.known < coverage.total {
                        warnings.push(format!("Creation dates recorded for {} of {} entries; unknown dates cannot match creation-date conditions.", format::count(coverage.known as u64), format::count(coverage.total as u64)));
                    }
                }
                if incomplete { warnings.push("This scan is incomplete; search results cover recorded entries only.".into()); }
                Some((index, results, warnings.join(" "), creation_unavailable))
            }).await;
            if cancel.load(std::sync::atomic::Ordering::Relaxed) { return; }
            let Some((index, results, warning, creation_unavailable)) = outcome else { return; };
            this.update(cx, |this, cx| {
                let Some(ready) = this.state.ready_mut() else { return; };
                if !ticket.accepts(ready, this.search_generation, this.load_generation) { return; }
                ready.finish_search(results);
                if !ready.rows.is_empty() {
                    this.rows_scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                this.search_index = Some(index);
                this.search_cancel = None;
                this.search_bar.update(cx, |bar, cx| {
                    bar.warning = (!warning.is_empty()).then_some(warning);
                    bar.creation_unavailable = creation_unavailable;
                    bar.can_rescan_for_creation = creation_unavailable && past;
                    cx.notify();
                });
                cx.notify();
            }).ok();
        }).detach();
    }

    /// Push the model's query text back into the box.
    ///
    /// There are necessarily two copies of the query: the text the input owns
    /// and edits, and the string `Ready` filters by. They diverge whenever
    /// something other than typing changes the search — navigating clears it,
    /// and the box went on showing a query that was no longer in effect.
    ///
    /// Rather than remembering to clear the box at each of the six places that
    /// can navigate, everything reconciles here, as the last step. Two copies
    /// of a string stay equal because one of them is always written from the
    /// other, not because every author remembered to.
    fn sync_query_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self
            .state
            .ready()
            .map(|ready| ready.filter.clone())
            .unwrap_or_default();

        self.cancel_search();
        if wanted.is_empty() {
            self.search_bar.update(cx, |bar, cx| bar.clear(window, cx));
            self.suggestions.clear();
            self.filter_open = false;
        } else {
            self.search_bar
                .update(cx, |bar, cx| bar.set_dsl(wanted.clone(), window, cx));
            self.query_text_changed(wanted, cx);
        }
    }

    /// Move the view to a directory, leaving any search behind.
    ///
    /// The one way to navigate. Every caller goes through it so the box, the
    /// suggestions and the rows cannot disagree about what is being shown.
    fn navigate_to(&mut self, id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ready) = self.state.ready_mut() {
            ready.focus_on(id);
        }
        self.sync_query_input(window, cx);
        cx.notify();
    }

    /// Whether a text box currently has the keyboard.
    ///
    /// While one does, its own editing keys win: Enter, Backspace and the
    /// arrows belong to the text, not to the table. Without this check, typing
    /// a query and pressing Enter navigated the tree — which cleared the search
    /// and changed the results underneath the person typing.
    fn editing_text(&self, window: &Window, cx: &App) -> bool {
        self.filter.read(cx).focus_handle(cx).is_focused(window)
            || self
                .help_search
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            || self
                .settings_search
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    /// What is saved, for the panel that lists it.
    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn settings_search(&self) -> &Entity<InputState> {
        &self.settings_search
    }
    pub(crate) fn settings_scroll(&self) -> &gpui_kit::ScrollHandle {
        &self.settings_scroll
    }
    pub(crate) fn help_scroll(&self) -> &gpui_kit::ScrollHandle {
        &self.help_scroll
    }
    pub(crate) fn clear_settings_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.settings_search
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        self.settings_scroll.set_offset(Default::default());
        cx.notify();
    }

    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        self.disarm();
        if self.settings_open {
            self.settings_search
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    pub(crate) fn suggestions(&self) -> &[kosha_query::Suggestion] {
        &self.suggestions
    }

    pub(crate) fn help_search(&self) -> &Entity<InputState> {
        &self.help_search
    }

    pub(crate) fn recipe_book(&self) -> Option<&kosha_search::RecipeBook> {
        self.search_recipes.as_ref().ok()
    }

    pub(crate) fn help_search_text(&self, cx: &App) -> String {
        self.help_search.read(cx).value().to_string()
    }

    pub(crate) fn mono(&self) -> SharedString {
        self.mono.clone()
    }

    /// Work out the accent, from the desktop or from us.
    ///
    /// Runs when the settings change, when the theme changes, and once at
    /// startup — the three moments the answer can differ. Windows publishes a
    /// light and a dark variant, so the mode is part of the question.
    fn rebuild_palette(&mut self, cx: &App) {
        self.palette = Palette::of(cx).with_accent(
            desktop_accent(&self.settings, cx),
            self.settings.tint_chrome,
        );
    }

    /// Re-read the locations and how full each volume is.
    ///
    /// Called when a scan finishes and after anything that frees space, since
    /// those are the moments the numbers change. Not on a timer: a bar that
    /// twitches on its own draws the eye to the one part of the window that had
    /// nothing new to say.
    fn refresh_places(&mut self) {
        self.places = crate::places::personal();
        self.distributions = crate::places::distributions();
        self.volumes = crate::places::volumes();
        self.rebuild_scopes();
    }

    /// Re-read the snapshot directory.
    ///
    /// Cheap — a directory read and a few hundred bytes per snapshot, because
    /// the header is outside the compressed body — but not free, so it happens
    /// when the list can have changed rather than per frame.
    pub(crate) fn refresh_snapshots(&mut self) {
        let listing = kosha_core::snapshot::store::list_all();
        self.kept_snapshots = listing.kept;
        self.snapshot_problems = problem_lines(&listing.unreadable);
    }

    /// The files in the store that could not be read, one line each.
    pub(crate) fn snapshot_problems(&self) -> &[SharedString] {
        &self.snapshot_problems
    }

    /// What is kept, oldest first.
    pub(crate) fn snapshots(&self) -> &[kosha_core::snapshot::store::Kept] {
        &self.kept_snapshots
    }

    pub(crate) fn snapshots_current_only(&self) -> bool {
        self.settings.snapshots_current_only && self.tree_root().is_some()
    }

    pub(crate) fn set_snapshot_scope(
        &mut self,
        current_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.snapshots_current_only = current_only;
        self.settings_changed(window, cx);
    }

    /// The newest snapshot that could be held against what is on screen.
    ///
    /// Same folder, and not the snapshot already being looked at — holding one
    /// against itself is a list of nothing, which is a true answer to a
    /// question nobody asked.
    pub(crate) fn latest_comparable(&self) -> Option<PathBuf> {
        let root = self.tree_root()?;
        let open = self.snapshot_source().map(|one| one.path.clone());
        self.kept_snapshots
            .iter()
            .rev()
            .find(|kept| kept.meta.root == root && Some(&kept.path) != open.as_ref())
            .map(|kept| kept.path.clone())
    }

    /// The root of whatever tree is on screen, if any.
    pub(crate) fn tree_root(&self) -> Option<PathBuf> {
        self.state
            .ready()
            .map(|ready| ready.tree.root_path().to_path_buf())
    }

    /// Whether this is the button that has been pressed once already.
    pub(crate) fn armed(&self, what: &Armed) -> bool {
        self.arming.as_ref() == Some(what)
    }

    /// First press arms, second acts; `true` means this press is the second.
    ///
    /// Arming one disarms any other, so there is never more than one button on
    /// screen asking to be pressed again. A second click on the same button is
    /// a cheaper confirmation than a modal, which would put a dialog in front
    /// of somebody tidying a list.
    fn press_again(&mut self, what: Armed, warning: &str, cx: &mut Context<Self>) -> bool {
        if self.armed(&what) {
            self.arming = None;
            return true;
        }
        self.arming = Some(what);
        self.state.notice = Some(warning.into());
        cx.notify();
        false
    }

    /// Forget a saved search, once it has been asked about.
    ///
    /// The settings panel's route only. The star beside the query box is a
    /// toggle — press it again and the search comes straight back — so asking
    /// there would be asking about something already undoable in one click.
    pub(crate) fn forget_saved_query(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.press_again(
            Armed::Query(query.to_string()),
            "Press again to forget that search. You would have to type it out again.",
            cx,
        ) {
            return;
        }
        self.forget_query(query, window, cx);
    }

    /// Unpin a folder, once it has been asked about.
    pub(crate) fn unpin_from_settings(
        &mut self,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.press_again(
            Armed::Pin(path.to_path_buf()),
            "Press again to take that folder off the sidebar. Nothing on disk is touched.",
            cx,
        ) {
            return;
        }
        self.unpin_scope(path, window, cx);
    }

    /// First press arms, second deletes.
    ///
    /// Deleting a snapshot is the one of the three that cannot be undone by
    /// doing the thing again: the disk it described has moved on, so the record
    /// is the only copy of that moment there will ever be. The other two ask as
    /// well, because the button that asks and the button that does not looked
    /// exactly alike.
    pub(crate) fn delete_snapshot(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.press_again(
            Armed::Snapshot(path.clone()),
            "Press again to delete that snapshot. It cannot come back.",
            cx,
        ) {
            return;
        }

        match kosha_core::snapshot::store::remove(&path) {
            Ok(()) => {
                self.state.notice = Some("Snapshot deleted.".into());
                // If it was the one on screen, what is on screen is now a tree
                // with no file behind it. Saying so beats leaving a rail row
                // highlighted that no longer exists.
                if self.snapshot_source().is_some_and(|one| one.path == path)
                    && let Some(ready) = self.state.ready_mut()
                {
                    ready.source = crate::state::Source::Disk;
                    self.state.notice = Some(
                        "Snapshot deleted. What is on screen is the tree it held, and nothing now holds it."
                            .into(),
                    );
                }
            }
            Err(error) => {
                self.state.notice = Some(format!("Could not delete that snapshot: {error}"));
            }
        }
        self.refresh_snapshots();
        cx.notify();
    }

    /// Hold a snapshot against whatever is on screen.
    ///
    /// One rule, and it covers both of the things somebody wants: with a scan
    /// on screen this says "what changed since then", and with another
    /// snapshot on screen it says "what changed between these two". The reader
    /// picks the other side by clicking it in the rail, which is where they
    /// were already looking.
    ///
    /// **The older tree is always the baseline**, whichever one was clicked.
    /// "What changed" has a direction, and taking the reader's click as the
    /// baseline would silently invert every sign on screen when they happened
    /// to open the newer one first.
    pub(crate) fn compare_with_snapshot(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.disarm();

        let Some(ready) = self.state.ready() else {
            self.state.notice =
                Some("Scan something first, then compare it with a snapshot.".into());
            cx.notify();
            return;
        };
        let here = Arc::clone(&ready.tree);
        // When the thing on screen is itself a snapshot, it has a date. When it
        // is a scan, it is now — which is later than any snapshot can be.
        // To the millisecond, because two snapshots of a folder that scans
        // quickly can be taken in one second and the older of them is still
        // the baseline.
        let here_taken = ready
            .source
            .snapshot()
            .map(|one| one.meta.at_ms())
            .unwrap_or(u64::MAX);
        let here_label: SharedString = match ready.source.snapshot() {
            Some(one) => crate::snapshots::when(&one.meta),
            None => SharedString::from("this scan"),
        };
        // What to call it if the age alone will not do. Empty for a scan,
        // which is already the only thing on screen called "this scan".
        let here_exact: SharedString = match ready.source.snapshot() {
            Some(one) => crate::snapshots::exactly(&one.meta),
            None => SharedString::default(),
        };
        let here_root = ready.tree.root_path().to_path_buf();
        let floor = self
            .settings
            .min_reclaimable_mb
            .saturating_mul(1024 * 1024)
            .max(1);
        let opts = kosha_core::compare::Options {
            min_delta: floor,
            directories: true,
            files: true,
        };
        let generation = self.load_generation;
        self.comparison_generation += 1;
        let comparison_generation = self.comparison_generation;
        self.comparison_busy = true;

        self.state.notice = Some("Comparing stored trees… no new scan is being run.".into());
        cx.notify();

        let task = cx.background_executor().spawn(async move {
            let (meta, there) = kosha_core::snapshot::store::open(&path)?;
            let started = std::time::Instant::now();
            let (older, newer) = if meta.at_ms() <= here_taken {
                (&there, &*here)
            } else {
                (&*here, &there)
            };
            let baseline = kosha_core::compare::Baseline::of_tree(older);
            let report = kosha_core::compare::compare(&baseline, newer, &opts);
            Ok::<_, kosha_core::snapshot::ReadError>((
                meta,
                there,
                here,
                here_taken,
                here_root,
                report,
                started.elapsed(),
            ))
        });

        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                if this.load_generation != generation
                    || this.comparison_generation != comparison_generation
                {
                    return;
                }
                this.comparison_busy = false;
                match outcome {
                    Ok((meta, there, here, here_taken, here_root, report, elapsed)) => this
                        .comparison_ready(
                            meta,
                            there,
                            here,
                            here_taken,
                            (here_label, here_exact),
                            here_root,
                            floor,
                            report,
                            elapsed,
                            cx,
                        ),
                    Err(error) => {
                        this.state.notice = Some(format!("Could not read that snapshot: {error}"));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn comparison_ready(
        &mut self,
        meta: kosha_core::snapshot::Meta,
        there: SizeTree,
        here: Arc<SizeTree>,
        here_taken: u64,
        (here_label, here_exact): (SharedString, SharedString),
        here_root: PathBuf,
        floor: u64,
        report: kosha_core::compare::Report,
        elapsed: Duration,
        cx: &mut Context<Self>,
    ) {
        // **Two roots are comparable exactly as far as they overlap.** This used
        // to refuse the pair outright — "two different folders cannot be
        // compared" — which is not what [`kosha_core::compare`] says: paths are
        // relative to each side's own root precisely so that a directory can be
        // held against the backup that is supposed to mirror it, and that
        // comparison is the reason the module exists. Refusing it here also left
        // the command line and the window answering the same question two
        // different ways, and the command line was the one in step with the
        // contract.
        //
        // So it is said rather than refused, and the thing said is what the
        // report rests on: how much of the record is here at all. Nothing in
        // common is still visible — as a sentence over the list, instead of a
        // list of deletions that never happened.
        let elsewhere = (meta.root != here_root).then(|| meta.root.clone());

        // **Two sides that read the same are two sides nobody can tell apart.**
        // Both of these are ages, and ages are coarse on purpose — a pair taken
        // in one sitting are both "just now", and a pair from last spring are
        // both "5 months ago". The header would then say `just now → just now`
        // over a list of changes with a direction, which is precisely the
        // reading a person has to get right. So when the wording collides, both
        // sides fall back to the clock, and only then.
        let mut there_label = crate::snapshots::when(&meta);
        let mut here_label = here_label;
        if there_label == here_label {
            there_label = crate::snapshots::exactly(&meta);
            if !here_exact.is_empty() {
                here_label = here_exact;
            }
        }

        // Older first, whichever one the reader clicked.
        let (baseline_tree, newer, against, now) = if meta.at_ms() <= here_taken {
            (&there, &*here, there_label, here_label)
        } else {
            (&*here, &there, here_label, there_label)
        };

        let changes = report.changes.len();
        self.state.notice = Some(if changes == 0 {
            format!("Nothing changed by more than {}.", self.bytes(floor))
        } else {
            format!("{} since {against}.", format::plural(changes, "change"))
        });

        // Said in the header, not in the notice: the notice is one line at the
        // foot of the window and it truncates, and in the first run of this the
        // sentence that got cut was the one that mattered.
        let mut footing = String::new();
        // **A tree that did not see everything invents the difference**, and
        // which way it invents it depends on which side it is. The record
        // reports what it missed as created; the scan reports what it missed
        // as gone. The command line said the first and neither said the
        // second, and this window said neither. Measured on a complete record
        // held against a scan of the same untouched tree stopped at a depth
        // ceiling: every file below it listed as gone.
        //
        // Asked of the trees rather than of the snapshot's header, because
        // either side here can be the record — this is also where two
        // snapshots are compared with each other.
        let missed = |partial: bool, depth_limited: bool| {
            partial.then_some(if depth_limited {
                "stopped at a depth ceiling"
            } else {
                "could not read everything"
            })
        };
        if let Some(why) = missed(
            baseline_tree.stats().partial,
            baseline_tree.stats().depth_limited,
        ) {
            footing.push_str(&format!(
                "That record {why}, so whatever it did not reach reads as new here"
            ));
        }
        if let Some(why) = missed(newer.stats().partial, newer.stats().depth_limited) {
            if !footing.is_empty() {
                footing.push_str(" \u{2014} ");
            }
            footing.push_str(&format!(
                "This one {why}, so whatever it did not reach reads as gone here"
            ));
        }
        // Two counting rules are two different questions, and on Windows the
        // rule belongs to the strategy: an elevated `--mft` scan folds a
        // hardlinked file's bytes into one name and the ordinary walk does
        // not. Held against each other they disagree about a tree nothing has
        // touched — measured at 32 MiB across four files that never changed.
        if baseline_tree.stats().dedup_hardlinks != newer.stats().dedup_hardlinks {
            if !footing.is_empty() {
                footing.push_str(" \u{2014} ");
            }
            footing.push_str(if baseline_tree.stats().dedup_hardlinks {
                "That record counted a hardlinked file's bytes once and this scan counts \
                 them under every name, so anything hardlinked differs for that alone"
            } else {
                "That record counted a hardlinked file's bytes under every name and this \
                 scan counts them once, so anything hardlinked differs for that alone"
            });
        }
        if baseline_tree.stats().cross_filesystems != newer.stats().cross_filesystems {
            if !footing.is_empty() {
                footing.push_str(" \u{2014} ");
            }
            footing.push_str(if baseline_tree.stats().cross_filesystems {
                "That record descended into other filesystems and this scan did not, \
                 so whatever is mounted inside this folder differs for that alone"
            } else {
                "That record stopped at other filesystems and this scan descended \
                 into them, so whatever is mounted inside differs for that alone"
            });
        }
        // The third rule of the same kind. On Windows an ordinary file's
        // on-disk size is estimated from the cluster size unless the exact
        // figure was asked for, and for a file small enough to live inside
        // the Master File Table the estimate is 4096 where the answer is 8 —
        // so a record taken one way against a scan taken the other reports
        // every small file as having changed. `None` is a record written
        // before this was carried: it did not say, so nothing is said.
        if let Some(then) = meta
            .exact_allocated
            .map(|_| baseline_tree.stats().exact_allocated)
            && then != newer.stats().exact_allocated
        {
            if !footing.is_empty() {
                footing.push_str(" \u{2014} ");
            }
            footing.push_str(if then {
                "That record measured every on-disk size and this scan estimates them, \
                 so a file small enough to sit inside the Master File Table differs \
                 for that alone"
            } else {
                "That record estimated every on-disk size and this scan measures them, \
                 so a file small enough to sit inside the Master File Table differs \
                 for that alone"
            });
        }
        if let Some(root) = &elsewhere {
            if !footing.is_empty() {
                footing.push_str(" \u{2014} ");
            }
            footing.push_str(&format!(
                "That record is of {}",
                format::printable(&root.to_string_lossy())
            ));
        }
        if report.mostly_missing() {
            if !footing.is_empty() {
                footing.push_str(" \u{2014} ");
            }
            footing.push_str(&format!(
                "only {} of {} in it are here, so most of this is the difference \
                 between two folders rather than a change",
                format::count(report.matched as u64),
                format::plural(report.baseline_entries, "path"),
            ));
        }
        // A CSV export taken with `--export-min` has no header to say it is
        // incomplete, and a comparison against one reads everything it left
        // out as created. The command line says the same thing beside its
        // own list; see `Report::looks_filtered_by_size`, where the shape of
        // the observation is argued.
        if report.looks_filtered_by_size() {
            if !footing.is_empty() {
                footing.push_str(" \u{2014} ");
            }
            footing.push_str(&format!(
                "{} here {} in the record, and {} smaller than anything in it, \
                 which is what a record exported with a size floor looks like",
                format::plural(report.added, "entry"),
                if report.added == 1 {
                    "is not"
                } else {
                    "are not"
                },
                if report.added == 1 {
                    "it is"
                } else {
                    "they are"
                },
            ));
        }

        if let Some(ready) = self.state.ready_mut() {
            ready.show_comparison(crate::state::Comparison {
                against_root: baseline_tree.root_path().to_path_buf(),
                now_root: newer.root_path().to_path_buf(),
                against_taken_ms: Some(meta.at_ms().min(here_taken)),
                now_taken_ms: (here_taken != u64::MAX).then_some(meta.at_ms().max(here_taken)),
                elapsed,
                against,
                now,
                changes: Arc::new(report.changes),
                net: report.net,
                below_floor: report.below_floor,
                floor,
                show_dirs: true,
                show_files: true,
                footing: (!footing.is_empty()).then(|| SharedString::from(footing)),
                ..Default::default()
            });
        }
        cx.notify();
    }

    /// Show or hide the directory rows of a comparison, and the same for
    /// files.
    ///
    /// The rule that the last one cannot go off lives here rather than in the
    /// two click handlers, because it is one rule about a pair and writing it
    /// twice is how the pair ends up disagreeing: an empty list under two
    /// switches that both look live is a view that reads as broken.
    pub(crate) fn toggle_comparison_kind(&mut self, directories: bool, cx: &mut Context<Self>) {
        let Some(one) = self
            .state
            .ready_mut()
            .and_then(|ready| ready.comparison.as_mut())
        else {
            return;
        };
        if directories {
            if one.show_files {
                one.show_dirs = !one.show_dirs;
            }
        } else if one.show_dirs {
            one.show_files = !one.show_files;
        }
        one.rebuild_visible();
        cx.notify();
    }

    pub(crate) fn filter_comparison(
        &mut self,
        kind: Option<kosha_core::compare::Kind>,
        cx: &mut Context<Self>,
    ) {
        if let Some(comparison) = self
            .state
            .ready_mut()
            .and_then(|ready| ready.comparison.as_mut())
        {
            comparison.change_kind = kind;
            comparison.rebuild_visible();
            cx.notify();
        }
    }

    /// Put the comparison away and go back to the tree.
    pub(crate) fn close_comparison(&mut self, cx: &mut Context<Self>) {
        self.comparison_generation += 1;
        self.comparison_busy = false;
        if let Some(ready) = self.state.ready_mut() {
            ready.clear_comparison();
        }
        cx.notify();
    }

    /// Forget whichever remove button is armed.
    ///
    /// Called when anything else happens, so a button left armed does not sit
    /// there waiting to be pressed by a click somebody has forgotten the reason
    /// for.
    pub(crate) fn disarm(&mut self) {
        self.arming = None;
    }

    /// Re-derive the pinned rows from the settings.
    ///
    /// Called whenever the settings change as well as on a rescan, because
    /// pinning is a settings change and the rail has to show it at once.
    fn rebuild_scopes(&mut self) {
        self.scopes = crate::places::scopes(&self.settings.saved_scopes);
    }

    // ------------------------------------------------------ command palette

    pub(crate) fn command_search(&self) -> &Entity<InputState> {
        &self.command_search
    }

    pub(crate) fn command_search_text(&self, cx: &App) -> String {
        self.command_search.read(cx).value().to_string()
    }

    pub(crate) fn command_scroll(&self) -> &gpui_kit::ScrollHandle {
        &self.command_scroll
    }

    pub(crate) fn command_selected(&self) -> usize {
        self.command_selected
    }

    /// What the palette can offer right now.
    pub(crate) fn commands(&self, cx: &App) -> Vec<commands::Command> {
        let ready = self.state.ready();
        // The query as the model has it, not as the box has it: the palette's
        // own search box holds focus while it is open, and what the palette
        // offers to save must be the search that is actually running.
        let query = self.saveable_query().unwrap_or_default();
        // The directory being looked at, not the scan root: somebody four
        // levels into a project means to pin the level they are standing on.
        let folder = ready.map(|ready| ready.tree.path_of(ready.focus));
        let mut commands = commands::all(
            &commands::Situation {
                have_tree: ready.is_some(),
                can_go_up: ready.is_some_and(|ready| ready.focus != ready.tree.root()),
                viewing_snapshot: self.viewing_snapshot(),
                automatic_snapshot_date: self
                    .snapshot_source()
                    .filter(|one| one.automatic)
                    .map(|one| crate::snapshots::exactly(&one.meta).to_string()),
                snapshots_kept: self.kept_snapshots.len(),
                comparable: self.latest_comparable(),
                showing_comparison: ready.is_some_and(|ready| ready.comparison.is_some()),
                root: ready
                    .map(|ready| ready.tree.root_path().display().to_string())
                    .unwrap_or_default(),
                showing_duplicates: ready.is_some_and(|ready| ready.duplicates.is_some()),
                finding_duplicates: ready.is_some_and(Ready::is_hunting),
                read_write: self.state.mode().is_read_write(),
                elevated: kosha_core::elevated(),
                can_elevate: crate::elevate::can_offer(),
                dark: cx.theme().mode.is_dark(),
                query_is_saved: self.settings.has_saved_query(&query),
                showing_rows: ready.is_some_and(crate::state::Ready::showing_results),
                folder_pinned: folder
                    .as_ref()
                    .is_some_and(|path| self.settings.has_saved_scope(path)),
                folder,
                query,
                columns: self.settings.columns.clone(),
                has_selection: ready.is_some_and(|ready| ready.selected.is_some()),
                details_open: self.details_open,
            },
            &self.places,
            &self.distributions,
            &self.volumes,
            &self.settings.saved_queries,
        );
        if !self.changing_recipe_packs {
            commands.extend(
                self.installed_recipe_packs
                    .iter()
                    .map(commands::remove_recipe_pack),
            );
        }
        if !self.changing_recipe_packs
            && let Some(catalog) = &self.marketplace_catalog
        {
            commands.extend(catalog.search("").filter_map(|entry| {
                let pack = &entry.metadata;
                (!self
                    .installed_recipe_packs
                    .iter()
                    .any(|installed| installed.qualified_id() == pack.qualified_id()))
                .then(|| commands::marketplace_entry(entry))
            }));
        }
        commands
    }

    /// The rows the palette is currently showing, best match first.
    fn command_matches(&self, cx: &App) -> (Vec<commands::Command>, Vec<usize>) {
        let commands = self.commands(cx);
        let needle = self.command_search_text(cx);
        let matches = commands::search(&commands, &needle, commands::LIMIT);
        (commands, matches)
    }

    /// Keep the query that is running, and say so.
    ///
    /// Saving writes the settings file, which can fail — a full disk, a config
    /// directory that is not writable, a settings file we refused to overwrite
    /// because we could not parse it. `settings_changed` surfaces that, so a
    /// save that did not happen is never reported as one that did.
    pub(crate) fn save_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.saveable_query().as_deref() != Some(query.trim()) {
            return;
        }
        if !self.settings.save_query(query) {
            return;
        }
        self.state.notice = Some(format!("Saved · {query}"));
        self.settings_changed(window, cx);
    }

    /// Unpin a folder from the sidebar.
    pub(crate) fn unpin_scope(
        &mut self,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.forget_scope(path);
        self.state.notice = Some(format!("Unpinned {}", path.display()));
        self.settings_changed(window, cx);
    }

    pub(crate) fn forget_query(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.forget_query(query.trim());
        self.state.notice = Some(format!("Forgotten · {}", query.trim()));
        self.settings_changed(window, cx);
    }

    /// The search the star acts on: what the model is filtering by.
    fn saveable_query(&self) -> Option<String> {
        let ready = self.state.ready()?;
        if !matches!(ready.search, Search::Results { .. }) {
            return None;
        }
        let query = ready.filter.trim().to_string();
        (!query.is_empty()).then_some(query)
    }

    /// Ask where to put an export, then write it off the UI thread.
    ///
    /// Exports what the table is showing: the results of a search when there
    /// is one, the whole tree when there is not. The palette row says which,
    /// so the file cannot be a surprise, and the notice afterwards repeats it.
    ///
    /// The dialog comes first and the writing second, on the background pool.
    /// A whole-volume export is millions of rows, and a window that freezes
    /// while it writes looks broken rather than busy.
    pub(crate) fn export(
        &mut self,
        format: kosha_core::export::Format,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ready) = self.state.ready() else {
            return;
        };
        if matches!(ready.search, Search::Pending | Search::Invalid { .. }) {
            self.state.notice =
                Some("Wait for a valid completed search before exporting its results.".into());
            cx.notify();
            return;
        }
        let tree = Arc::clone(&ready.tree);
        // Cloned now, not later: by the time the dialog closes the user may
        // have typed a different query, and the file must be the one the
        // command they chose described.
        let rows = ready.showing_results().then(|| ready.rows.clone());
        // The table retains only its first 2,000 matches. Capture the exact
        // compiled DSL and clock, then reevaluate against the same immutable
        // tree on the export worker so every match is written.
        let query = if matches!(ready.search, Search::Results { .. }) {
            match kosha_query::Query::parse(&ready.filter) {
                Ok(query) => Some(query),
                Err(error) => {
                    self.state.notice = Some(format!("Cannot export this search: {error}"));
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        let now = ready.now;
        let absolute_paths = rows.is_some();

        let suggested = format!(
            "{}-kosha.{}",
            tree.root_path()
                .file_name()
                .map_or_else(|| "scan".to_string(), |n| n.to_string_lossy().to_string()),
            format.extension(),
        );
        let directory = crate::places::home().unwrap_or_else(|| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&directory, Some(&suggested));

        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            let Ok(Some(path)) =
                this.update(cx, |this, cx| this.file_picker_selection(selection, cx))
            else {
                return;
            };

            let options = kosha_core::export::Options {
                format,
                absolute_paths,
                ..Default::default()
            };
            let target = path.clone();
            let written = cx
                .background_spawn(async move {
                    let rows = export_rows(&tree, query.as_ref(), rows, now);
                    // Written beside the destination and renamed into place:
                    // the picker's path is very often an existing baseline,
                    // and an export that did not finish must not replace it
                    // with a shorter file that still reads as a whole one.
                    kosha_core::export::to_path(&tree, rows.as_deref(), &options, &target)
                })
                .await;

            this.update(cx, |this, cx| {
                this.state.notice = Some(match written {
                    Ok(written) => {
                        let mut summary = format!(
                            "Exported {} to {}",
                            format::plural(written.rows, "entry"),
                            path.display(),
                        );
                        // Named rather than buried: a row whose path is not
                        // real text is one a comparison against the filesystem
                        // will not match, and the file marks them for exactly
                        // that reason.
                        if written.lossy > 0 {
                            summary.push_str(&format!(
                                " · {} marked lossy",
                                format::plural(written.lossy, "name")
                            ));
                        }
                        summary
                    }
                    // Said plainly, with the path. A failed write that reports
                    // success is how somebody discovers an empty file a month
                    // later, when the thing it was going to be compared
                    // against is gone.
                    Err(error) => format!("Could not write {}: {error}", path.display()),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn export_comparison(
        &mut self,
        format: crate::comparison_export::Format,
        cx: &mut Context<Self>,
    ) {
        let Some(comparison) = self
            .state
            .ready()
            .and_then(|ready| ready.comparison.clone())
        else {
            return;
        };
        // Freeze the report and filters before opening the native save dialog.
        let directory = crate::places::home().unwrap_or_else(|| PathBuf::from("."));
        let suggested = format!("kosha-comparison.{}", format.extension());
        let prompt = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            let Ok(Some(path)) =
                this.update(cx, |this, cx| this.file_picker_selection(selection, cx))
            else {
                return;
            };
            let target = path.clone();
            let written = cx
                .background_spawn(async move {
                    kosha_core::export::atomic_write(&target, |out| {
                        crate::comparison_export::write(&comparison, format, out)
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.state.notice = Some(match written {
                    Ok(rows) => {
                        format!("Exported {} comparison changes to {}", rows, path.display())
                    }
                    Err(error) => {
                        format!("Could not export comparison to {}: {error}", path.display())
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_open = true;
        self.command_selected = 0;
        // Back to the top, or a palette reopened after scrolling shows the
        // middle of the list with the first row armed somewhere above it.
        self.command_scroll.scroll_to_item(0);
        // Emptied on every open. A palette that reopens showing the last thing
        // somebody typed makes the commonest gesture — open, type, Enter —
        // start by clearing text nobody wanted.
        self.command_search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.command_search
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Move the armed row, clamped to the matches actually on screen.
    fn move_command(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.command_matches(cx).1.len();
        if count == 0 {
            self.command_selected = 0;
            return;
        }
        let current = self.command_selected.min(count - 1) as isize;
        self.command_selected = current.saturating_add(by).clamp(0, count as isize - 1) as usize;
        self.command_scroll.scroll_to_item(self.command_selected);
        cx.notify();
    }

    /// Run whichever row Enter is armed on.
    fn run_selected_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (commands, matches) = self.command_matches(cx);
        let armed = self.command_selected.min(matches.len().saturating_sub(1));
        let Some(&index) = matches.get(armed) else {
            return;
        };
        self.run_command(commands[index].action.clone(), window, cx);
    }

    pub(crate) fn run_command(
        &mut self,
        action: commands::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Closed first, so an action that opens something else — the search
        // box, the help panel — is not immediately covered by the palette it
        // came from, and so focus ends up where the action put it.
        self.close_palette(window, cx);

        match action {
            commands::Action::Scan(path) => self.scan(path, cx),
            commands::Action::ChooseFolder => self.choose_folder(cx),
            commands::Action::ChooseImage => self.choose_image(cx),
            commands::Action::InstallRecipePack => self.choose_recipe_pack(cx),
            commands::Action::SyncMarketplace => self.sync_marketplace(cx),
            commands::Action::MarketplaceUnavailable(reason) => {
                self.state.notice = Some(reason);
            }
            commands::Action::DownloadRecipePack(id) => {
                if let Some(catalog) = self.marketplace_catalog.clone() {
                    self.change_recipe_packs(
                        move || {
                            catalog
                                .install(&id)
                                .map(|pack| format!("Installed {}", pack.title))
                        },
                        cx,
                    );
                }
            }
            commands::Action::RemoveRecipePack { id, title } => {
                self.confirm_remove_recipe_pack(id, title, window, cx);
            }
            commands::Action::Rescan => self.rescan(cx),
            commands::Action::GoUp => {
                if let Some(ready) = self.state.ready_mut() {
                    ready.ascend();
                }
                self.sync_query_input(window, cx);
            }
            commands::Action::OpenSearch => self.open_filter(window, cx),
            commands::Action::RunQuery(query) => self.apply_query(query.to_string(), window, cx),
            commands::Action::RunSaved(query) => self.apply_query(query, window, cx),
            commands::Action::SaveQuery(query) => self.save_query(&query, window, cx),
            commands::Action::ForgetQuery(query) => self.forget_query(&query, window, cx),
            commands::Action::OpenHelp => self.open_help(window, cx),
            commands::Action::FindDuplicates => self.find_duplicates(cx),
            commands::Action::TakeSnapshot => self.take_snapshot(cx),
            commands::Action::CompareWithLatest(path) => self.compare_with_snapshot(path, cx),
            commands::Action::CloseComparison => self.close_comparison(cx),
            commands::Action::ScanAgainForReal(path) => self.scan(path, cx),
            commands::Action::StopHunt => {
                if let Some(ready) = self.state.ready() {
                    ready.stop_hunt();
                }
                self.state.notice = Some("Stopping the duplicate hunt\u{2026}".into());
                cx.notify();
            }
            commands::Action::StopDuplicates => {
                if let Some(ready) = self.state.ready_mut() {
                    ready.clear_duplicates();
                }
            }
            commands::Action::ToggleSettings => {
                self.toggle_settings(window, cx);
            }
            commands::Action::ShowOwners => self.open_owners(cx),
            commands::Action::ToggleTheme => {
                self.settings.theme = if cx.theme().mode.is_dark() {
                    ThemePreference::Light
                } else {
                    ThemePreference::Dark
                };
                self.settings_changed(window, cx);
            }
            commands::Action::Export(format) => self.export(format, window, cx),
            commands::Action::ExportComparison(format) => self.export_comparison(format, cx),
            commands::Action::PinScope(path) => {
                if self.settings.save_scope(&path) {
                    self.state.notice = Some(format!("Pinned {}", path.display()));
                    self.settings_changed(window, cx);
                }
            }
            commands::Action::UnpinScope(path) => {
                self.settings.forget_scope(&path);
                self.state.notice = Some(format!("Unpinned {}", path.display()));
                self.settings_changed(window, cx);
            }
            commands::Action::MoveColumn(column, direction) => {
                let moved = match direction {
                    commands::Direction::Left => self.settings.columns.move_left(column),
                    commands::Direction::Right => self.settings.columns.move_right(column),
                };
                if moved {
                    self.persist_settings();
                }
            }
            commands::Action::SetColumnHidden(column, hidden) => {
                if self.settings.columns.set_hidden(column, hidden) {
                    self.persist_settings();
                }
            }
            commands::Action::ResetColumns => {
                self.settings.columns.reset();
                self.persist_settings();
            }
            commands::Action::ShowAuditLog => self.show_audit_log(cx),
            commands::Action::ToggleDetails => self.toggle_details(window, cx),
            commands::Action::ToggleMode => self.toggle_mode(cx),
            commands::Action::RestartAsAdministrator => {
                // The geometry is saved first: the new window should open where
                // this one is, and it is about to be a different process with no
                // way to ask.
                self.remember_geometry(window);
                self.persist_settings();
                match crate::elevate::restart_as_administrator() {
                    // Two kosha windows both claiming to be the session is
                    // worse than either of them alone.
                    Ok(()) => cx.quit(),
                    Err(message) => self.state.notice = Some(message),
                }
            }
            commands::Action::Quit => {
                self.remember_geometry(window);
                cx.quit();
            }
        }
        cx.notify();
    }

    /// Put a suggested or documented query into the box and run it.
    ///
    /// The only thing here that changes what the user typed, and it happens
    /// because they clicked. The input keeps its own undo history, so their
    /// original text is one Ctrl+Z away.
    pub(crate) fn apply_query(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter_open = true;
        self.search_bar
            .update(cx, |bar, cx| bar.set_dsl(query.clone(), window, cx));
        self.query_text_changed(query, cx);
    }

    /// Hunt for duplicate contents across the scanned tree.
    ///
    /// The one thing here whose cost is measured in bytes rather than entries:
    /// it reads file data. So it runs on the background pool, only when asked,
    /// and the interface stays live while it does — a scan of a media library
    /// can take a while, and freezing would make it look broken rather than
    /// busy.
    /// Hash the files in the tree on screen and group the ones that match.
    ///
    /// **Never against a snapshot.** A snapshot records how big every file was
    /// and where it sat; it does not record a single byte of any of them, so
    /// the only way to answer "are these two identical" is to read them off the
    /// disk as it is now. The answer would be about today and would appear
    /// under a chip saying this is the past, with paths in it that may not
    /// exist any more — a window contradicting itself about which day it is
    /// showing.
    fn find_duplicates(&mut self, cx: &mut Context<Self>) {
        let Some(ready) = self.state.ready() else {
            return;
        };
        if let Some(why) =
            crate::state::no_bytes_to_hunt(ready.source.is_past(), ready.tree.stats().strategy)
        {
            self.state.notice = Some(why.into());
            cx.notify();
            return;
        }
        if ready.is_hunting() {
            return;
        }
        let tree = Arc::clone(&ready.tree);
        let cancel = Arc::new(AtomicBool::new(false));
        // Bounded and dropping, like the scanner's: a slow frame must never be
        // able to hold up the hashing.
        let (tx, rx) = crossbeam_channel::bounded(64);
        if let Some(ready) = self.state.ready_mut() {
            ready.hunt = Some(crate::state::Hunting {
                progress: None,
                cancel: Arc::clone(&cancel),
            });
        }
        cx.notify();

        let task = cx.background_executor().spawn(async move {
            kosha_analyze::duplicates::Hunt::new(
                kosha_analyze::duplicates::DuplicateOptions::default(),
            )
            .with_cancel(cancel)
            .with_progress(tx)
            .run(&tree)
        });

        // The same sixteen-times-a-second poll the scan uses, and for the same
        // reason: waking the window per group would put the render thread in
        // the way of the reads.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let latest = rx.try_iter().last();
                let keep_going = this
                    .update(cx, |this, cx| {
                        let Some(hunt) = this.state.ready_mut().and_then(|r| r.hunt.as_mut())
                        else {
                            return false;
                        };
                        if let Some(update) = latest {
                            hunt.progress = Some(update);
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();

        let units = self.units();
        cx.spawn(async move |this, cx| {
            let report = task.await;
            this.update(cx, |this, cx| {
                let stopped = report.stopped;
                let found = Duplicates::from_report(report);
                let mut summary = if stopped && found.groups == 0 {
                    // Nothing found *and* stopped early is not "there is
                    // nothing here" — it is "we did not look". Saying the
                    // first would be the most misleading sentence this window
                    // could print.
                    "Stopped. Nothing was confirmed before it was asked to stop.".to_string()
                } else if stopped {
                    format!(
                        "Stopped early — {} confirmed so far, {} recoverable. There may be more.",
                        format::plural(found.groups, "duplicate set"),
                        this.bytes(found.reclaimable),
                    )
                } else if found.groups == 0 {
                    format!(
                        "No duplicate contents among {}.",
                        format::plural(found.examined, "candidate")
                    )
                } else {
                    format!(
                        "{} recoverable across {}, from {} read",
                        this.bytes(found.reclaimable),
                        format::plural(found.groups, "duplicate set"),
                        this.bytes(found.bytes_read),
                    )
                };
                // Excluded files are stated rather than quietly dropped: one we
                // could not read is not one to propose deleting a copy of.
                if found.not_local > 0 {
                    // A refusal rather than a failure: reading a cloud
                    // placeholder downloads it.
                    summary.push_str(&format!(
                        " · {} stored in the cloud, skipped rather than downloaded",
                        format::plural(found.not_local, "file"),
                    ));
                }
                if found.unreadable > 0 {
                    summary.push_str(&format!(
                        " · {} could not be read and were excluded",
                        format::count(found.unreadable as u64),
                    ));
                }
                if found.below_min > 0 {
                    // The one exclusion that is a setting rather than a fact
                    // about the file, so it names the threshold: a folder of
                    // identical 200 KB photographs otherwise comes back
                    // "nothing found" and looks like the hunt failed.
                    summary.push_str(&format!(
                        " · {} under {} not considered",
                        format::plural(found.below_min, "file"),
                        format::bytes_in(found.min_size, units),
                    ));
                }
                if let Some(ready) = this.state.ready_mut() {
                    ready.hunt = None;
                    // A stopped hunt still shows what it did confirm: a
                    // match is a match whenever it was found, the view acts on
                    // nothing, and throwing away minutes of hashing because
                    // somebody wanted the rest of it to stop would be unkind.
                    // The summary above is what says the list is incomplete.
                    if found.groups > 0 {
                        ready.show_duplicates(found);
                    }
                }
                this.state.notice = Some(summary);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_help(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.help_open = true;
        self.help_search.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(crate) fn close_help(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.help_open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(crate) fn open_owners(&mut self, cx: &mut Context<Self>) {
        self.owners_open = true;
        cx.notify();
    }

    pub(crate) fn close_owners(&mut self, cx: &mut Context<Self>) {
        self.owners_open = false;
        cx.notify();
    }

    /// Turn owner collection on and scan again, which is the only way to an
    /// answer the current tree does not carry.
    ///
    /// The setting is saved, not applied for one run: somebody who has asked
    /// this question once is asking it of a machine where it has an answer, and
    /// a scan that quietly stopped recording owners next time would send them
    /// back here to wonder why. Settings has the switch to turn it off again.
    pub(crate) fn rescan_with_owners(&mut self, cx: &mut Context<Self>) {
        self.settings.collect_owners = true;
        self.state.scan_options = self.settings.scan_options();
        if let Err(error) = crate::settings::save(&self.settings) {
            self.state.notice = Some(format!("Could not save settings: {error}"));
        }
        self.owners_open = false;
        self.rescan(cx);
    }

    /// The finished tree, for the panels that read across the whole of it.
    pub(crate) fn tree(&self) -> Option<&kosha_core::SizeTree> {
        self.state.ready().map(|ready| &*ready.tree)
    }

    /// Whether a scan is running right now.
    ///
    /// Distinct from "no tree": a panel opened mid-scan has nothing to show
    /// *yet*, which is a different sentence from one that has nothing to show
    /// because nobody has scanned anything.
    pub(crate) fn is_scanning(&self) -> bool {
        matches!(self.state.phase, crate::state::Phase::Scanning { .. })
    }

    /// How this session renders sizes. Threaded through every call site rather
    /// than defaulted, so a user who picked GB never sees a stray GiB.
    pub(crate) fn units(&self) -> kosha_core::format::Units {
        self.settings.units()
    }

    pub(crate) fn bytes(&self, n: u64) -> String {
        format::bytes_in(n, self.units())
    }

    /// Write the settings out, without re-applying anything.
    ///
    /// The counterpart to [`Self::settings_changed`], for preferences that are
    /// already visible by the time they are saved. A column width is one:
    /// rebuilding the palette and re-resolving the sidebar at the end of every
    /// drag would be work for a change that has already been drawn.
    /// Write down where the window is, so the next launch can put it back.
    ///
    /// Called on the way out rather than as the window moves: a resize is a
    /// drag, which is hundreds of events, and rewriting the settings file on
    /// each of them would be a great deal of disk for a number nobody reads
    /// until the next launch.
    ///
    /// The cost of that choice is that a killed process forgets its position.
    /// That is the right trade — it is the case where the user was not
    /// finishing anything — and it is why both ordinary exits are covered:
    /// closing the window, and quitting from the palette.
    fn remember_geometry(&mut self, window: &Window) {
        let now = Some(crate::geometry::Geometry::of(window.window_bounds()));
        if self.settings.window == now {
            return;
        }
        self.settings.window = now;
        self.persist_settings();
    }

    /// Write the settings out, unless the file on disk is one we could not
    /// read.
    ///
    /// That refusal is the whole point. A settings file is a file people are
    /// invited to open and edit, so it can be hand-edited into something
    /// invalid — and it still holds their saved searches, their pinned folders
    /// and their thresholds. Saving over it replaces every one of those with a
    /// default, and the user finds out later, with nothing to recover from.
    ///
    /// The doc on `Loaded::problem` had promised this since it was written.
    /// Nothing implemented it, and a test proved the cost: a file holding one
    /// unparseable value and one saved search came back with the search gone.
    fn persist_settings(&mut self) {
        if let Some(problem) = &self.settings_unusable {
            self.state.notice = Some(format!(
                "Not saving: {problem}. Fix or delete that file and restart."
            ));
            return;
        }
        if let Err(error) = crate::settings::save(&self.settings) {
            self.state.notice = Some(format!("Could not save settings: {error}"));
        }
    }

    /// Persist the settings and apply anything that takes effect immediately.
    ///
    /// A failed save is surfaced rather than swallowed: silently forgetting a
    /// preference every time the app restarts is the kind of bug users never
    /// manage to report precisely.
    fn settings_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.scan_options = self.settings.scan_options();

        match self.settings.theme {
            ThemePreference::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            ThemePreference::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
            // Following the OS again would mean re-reading the system
            // appearance, which GPUI applies at startup; until the next launch
            // the current mode stands.
            ThemePreference::System => {}
        }

        self.rebuild_scopes();
        // The theme may just have changed, and Windows publishes a different
        // accent shade for each mode.
        self.rebuild_palette(cx);

        if let Err(error) = crate::settings::save(&self.settings) {
            self.state.notice = Some(format!("Could not save settings: {error}"));
        }
        cx.notify();
    }

    /// Show the filter row and put the cursor in it.
    fn open_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_open = true;
        self.filter.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Clear the filter and hide the row.
    fn close_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_search();
        self.filter_open = false;
        self.suggestions.clear();
        self.search_bar.update(cx, |bar, cx| bar.clear(window, cx));
        if let Some(ready) = self.state.ready_mut() {
            // Not `set_filter("")`: that clears the duplicate hunt only when
            // the *new* filter is non-empty, so closing the row from the
            // duplicate view cleared an already-empty box and left everything
            // else exactly as it was. See `Ready::close_query_view`.
            ready.close_query_view();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    // ---------------------------------------------------------------- scanning

    /// True while the tree on screen came out of a file rather than a disk.
    pub(crate) fn viewing_snapshot(&self) -> bool {
        self.state
            .ready()
            .is_some_and(|ready| ready.source.is_past())
    }

    /// The snapshot on screen, if one is.
    pub(crate) fn snapshot_source(&self) -> Option<&crate::state::SnapshotSource> {
        self.state.ready().and_then(|ready| ready.source.snapshot())
    }

    /// Open a snapshot as the tree on screen.
    ///
    /// Reading one is fast — a quarter of a second for a million entries,
    /// against three to rescan — but it is still a file to inflate, and a
    /// window that froze while doing it would be the one place in this
    /// application that blocks. So it goes to the background pool like
    /// everything else.
    pub(crate) fn open_snapshot(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.disarm();
        self.state.cancel_scan();
        self.cancel_search();
        self.search_index = None;
        self.load_generation += 1;
        self.comparison_generation += 1;
        self.comparison_busy = false;
        let generation = self.load_generation;
        if self.is_scanning() {
            self.state.phase = Phase::Idle;
        }
        self.state.notice = Some(format!("Opening {}\u{2026}", path.display()));
        cx.notify();

        let reading = path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { kosha_core::snapshot::store::open(&reading) });

        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match outcome {
                    Ok((meta, tree)) => this.snapshot_opened(path, meta, Arc::new(tree), false, cx),
                    Err(error) => {
                        // A damaged or unreadable snapshot is not a failed scan:
                        // there is no folder to blame and nothing to retry. It says
                        // what happened and leaves whatever was on screen alone.
                        this.state.notice = Some(format!("{}: {error}", path.display()));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn snapshot_opened(
        &mut self,
        path: PathBuf,
        meta: kosha_core::snapshot::Meta,
        tree: Arc<SizeTree>,
        automatic: bool,
        cx: &mut Context<Self>,
    ) {
        let taken = crate::snapshots::when(&meta);
        // The session follows the snapshot's root so that the breadcrumb and
        // the window title say what is being looked at. It stays read-only:
        // `Session::new` starts that way, and a snapshot cannot be acted on
        // whatever the mode says.
        self.state.session = kosha_fsops::Session::new(meta.root.clone());
        self.state.phase = Phase::Ready(crate::state::Ready::from_source(
            Arc::clone(&tree),
            crate::state::Source::Snapshot(Box::new(crate::state::SnapshotSource {
                path,
                meta,
                automatic,
            })),
        ));
        self.state.notice = (!automatic).then(|| {
            format!("Showing a snapshot taken {taken}. Nothing here is the disk as it is now.")
        });
        self.apply_pending(cx);
        cx.notify();

        // Findings are about a tree, not about a disk, so they are still worth
        // having: "this build cache was 12 GB in March" is exactly the kind of
        // thing somebody opens a snapshot to find out. What they cannot do is
        // act on one, and `begin_action` is where that is refused.
        let context = self.settings.analysis_context();
        let packs = kosha_analyze::pack::load_all();
        let rules = packs.rules;
        let generation = self.load_generation;
        let task = cx
            .background_executor()
            .spawn(async move { kosha_analyze::findings_with(&tree, &context, &rules) });
        cx.spawn(async move |this, cx| {
            let insights = task.await;
            this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                if let Some(ready) = this.state.ready_mut() {
                    ready.insights = Some(Arc::new(insights));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Keep the tree on screen as a snapshot.
    pub(crate) fn take_snapshot(&mut self, cx: &mut Context<Self>) {
        // Explicit records are already kept. An automatic record can be kept
        // in the sidebar, preserving its original observation date and header.
        if self.snapshot_source().is_some_and(|one| !one.automatic) {
            self.state.notice = Some("This is already a snapshot.".into());
            cx.notify();
            return;
        }
        let Some(ready) = self.state.ready() else {
            self.state.notice = Some("Scan something first.".into());
            cx.notify();
            return;
        };

        let tree = Arc::clone(&ready.tree);
        let historical = ready.source.snapshot().map(|one| one.meta.clone());
        let original_date = historical.as_ref().map(crate::snapshots::exactly);
        if let Some(date) = &original_date {
            self.state.notice = Some(format!(
                "Keeping saved scan from {date}; its original scan date will be preserved…"
            ));
            cx.notify();
        }
        let taken = std::time::SystemTime::now();
        let task = cx.background_executor().spawn(async move {
            let saved = match historical {
                Some(mut meta) => {
                    meta.label = "Kept drive scan".into();
                    kosha_core::snapshot::store::save_record(&tree, &meta)
                }
                None => kosha_core::snapshot::store::save(&tree, "", taken),
            };
            saved.map(|path| {
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                (path, bytes)
            })
        });

        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok((path, bytes)) => {
                        this.state.notice = Some(match &original_date {
                            Some(date) => format!("Snapshot kept from {date}; original scan date preserved. It is in the sidebar."),
                            None => format!("Snapshot kept, {}. It is in the sidebar.", this.bytes(bytes)),
                        });
                        let _ = path;
                    }
                    Err(error) => {
                        this.state.notice = Some(format!("Could not keep a snapshot: {error}"));
                    }
                }
                this.refresh_snapshots();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Kick off a scan. Returns immediately; the UI keeps painting.
    pub fn scan(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let options = self.state.scan_options;
        self.scan_with(path, options, cx);
    }

    /// Scan a location the fast way, for this one scan.
    ///
    /// Bound to holding Shift while starting a scan, which is how somebody who
    /// leaves the settings off still reaches the fast path for the one place
    /// they want it — and how somebody checks two strategies against each other
    /// without opening settings twice.
    ///
    /// Both are asked for rather than one, because which of them applies is a
    /// property of the *path*: a drive has a Master File Table and a WSL
    /// distribution has a disk image, and each declines where it is not the
    /// right one. So the modifier means "read the filesystem's own structures"
    /// rather than naming a strategy the user would have to know to choose
    /// between.
    ///
    /// It only ever turns them *on*. A modifier that silently took a scan's
    /// accuracy away would be a worse trade than one extra click.
    pub fn scan_the_fast_way(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let options = kosha_core::ScanOptions {
            use_mft: true,
            use_image: true,
            ..self.state.scan_options
        };
        self.scan_with(path, options, cx);
    }

    fn scan_with(
        &mut self,
        path: PathBuf,
        options: kosha_core::ScanOptions,
        cx: &mut Context<Self>,
    ) {
        let reference_entries = self
            .state
            .ready()
            .filter(|r| {
                !r.tree.is_incomplete()
                    && kosha_core::snapshot::store::same_root(r.tree.root_path(), &path)
                    && r.tree.stats().cross_filesystems == options.cross_filesystems
            })
            .map(|r| r.tree.stats().entries)
            .or_else(|| {
                self.last_drive_record
                    .as_ref()
                    .filter(|r| {
                        r.matches(&path) && r.meta.cross_filesystems == options.cross_filesystems
                    })
                    .map(|r| r.meta.entries)
            })
            .or_else(|| {
                self.kept_snapshots
                    .iter()
                    .rev()
                    .find(|r| {
                        kosha_core::snapshot::store::same_root(&r.meta.root, &path)
                            && !r.meta.cancelled
                            && !r.meta.depth_limited
                            && r.meta.cross_filesystems == options.cross_filesystems
                    })
                    .map(|r| r.meta.entries)
            })
            .filter(|_| options.max_depth.is_none());
        self.disarm();
        self.state.cancel_scan();
        self.cancel_search();
        self.search_index = None;
        self.load_generation += 1;
        self.comparison_generation += 1;
        self.comparison_busy = false;
        let generation = self.load_generation;
        let cancel = Arc::new(AtomicBool::new(false));
        // A bounded channel that drops when full: a slow frame must never be
        // able to throttle the scanner.
        let (tx, rx) = crossbeam_channel::bounded(64);

        self.state.session = kosha_fsops::Session::new(path.clone());
        self.state.notice = None;
        self.state.phase = Phase::Scanning {
            path: path.clone(),
            progress: None,
            cancel: Arc::clone(&cancel),
        };

        let scan_path = path.clone();
        let completion_cancel = Arc::clone(&cancel);
        self.scan_timing = crate::scan_timing::ScanTiming::with_total(reference_entries);
        let (total_tx, total_rx) = crossbeam_channel::bounded(1);
        let started = std::time::Instant::now();
        let task = cx.background_executor().spawn(async move {
            if reference_entries.is_none()
                && options.max_depth.is_none()
                && let Some(dir) = kosha_core::snapshot::store::automatic_dir()
            {
                let root = kosha_core::path::canonical(&scan_path);
                let record = kosha_core::snapshot::store::automatic_path_in(&dir, &root);
                if let Ok(meta) = kosha_core::snapshot::store::read_meta_at(&record)
                    && kosha_core::snapshot::store::same_root(&meta.root, &root)
                    && !meta.cancelled
                    && !meta.depth_limited
                    && meta.cross_filesystems == options.cross_filesystems
                {
                    let _ = total_tx.try_send(meta.entries);
                }
            }
            Scanner::new(options)
                .with_cancel(cancel)
                .with_progress(tx)
                .scan(&scan_path)
        });

        // Poll progress on a timer rather than waking the UI per directory.
        // Sixteen updates a second is more than anyone can read, and it keeps
        // the render thread out of the scanner's way.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let latest = rx.try_iter().last();
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.load_generation != generation {
                            return false;
                        }
                        if let Ok(total) = total_rx.try_recv() {
                            this.scan_timing.set_reference_total(total);
                        }
                        if let Phase::Scanning { progress, .. } = &mut this.state.phase {
                            if let Some(update) = latest {
                                *progress = Some(update);
                            }
                            if let Some(update) = *progress {
                                this.scan_timing.observe(started.elapsed(), update);
                            }
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match outcome {
                    Ok(mut tree) => {
                        if completion_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            tree.mark_cancelled();
                        }
                        this.scan_finished(Arc::new(tree), cx);
                    }
                    // Cancelling is something the user did, not something that went
                    // wrong, and a failure screen saying "the scan was cancelled"
                    // reads as a bug in the thing they just asked for. The walk
                    // never reaches this — it stops at a level boundary and hands
                    // back the valid prefix it has — but the MFT strategy has
                    // nothing to show until the whole table is read, so pressing
                    // Escape during one lands here.
                    Err(kosha_core::ScanError::Cancelled) => {
                        this.state.phase = Phase::Idle;
                        this.state.notice = Some("Scan cancelled.".into());
                        cx.notify();
                    }
                    Err(error) => {
                        this.state.phase = Phase::Failed {
                            path,
                            // The reason without the path, because the screen
                            // below draws the path on a line of its own. See
                            // `ScanError::reason`.
                            message: error.reason(),
                            hint: failure_hint(&error),
                        };
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn file_picker_selection<T>(
        &mut self,
        selection: Result<anyhow::Result<Option<T>>, impl Into<anyhow::Error>>,
        cx: &mut Context<Self>,
    ) -> Option<T> {
        match selection.map_err(Into::into).and_then(|result| result) {
            Ok(path) => path,
            Err(error) => {
                self.state.notice = Some(format!("Could not open the file picker: {error}"));
                cx.notify();
                None
            }
        }
    }

    /// Ask for a folder and scan it.
    ///
    /// The sidebar lists home, downloads, documents, the desktop, the volumes
    /// and whatever has been pinned — which is most of what anybody scans and
    /// not all of it. Without this, a folder outside that list could only be
    /// reached by starting kosha again with it on the command line, which is
    /// not something a window should ask of anybody.
    pub fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Scan".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            this.update(cx, |this, cx| {
                if let Some(path) = this
                    .file_picker_selection(selection, cx)
                    .and_then(|paths| paths.into_iter().next())
                {
                    this.scan(path, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn sync_marketplace(&mut self, cx: &mut Context<Self>) {
        if self.syncing_marketplace {
            return;
        }
        self.syncing_marketplace = true;
        self.state.notice = Some("Syncing the marketplace…".into());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async { kosha_marketplace::catalog::sync() })
                .await;
            this.update(cx, |app, cx| {
                app.syncing_marketplace = false;
                app.state.notice = Some(match result {
                    Ok(catalog) => {
                        let count = catalog.search("").count();
                        app.marketplace_catalog = Some(Arc::new(catalog));
                        format!("Marketplace updated. {count} packs support this OS.")
                    }
                    Err(error) => format!("Could not sync the marketplace: {error}"),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_recipe_pack(&mut self, cx: &mut Context<Self>) {
        if self.changing_recipe_packs {
            return;
        }
        let prompt = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Install".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            let Ok(Some(path)) = this.update(cx, |app, cx| {
                app.file_picker_selection(selection, cx)
                    .and_then(|paths| paths.into_iter().next())
            }) else {
                return;
            };
            this.update(cx, |app, cx| {
                app.change_recipe_packs(
                    move || {
                        kosha_marketplace::install_local(&path)
                            .map(|pack| format!("Installed {}", pack.title))
                    },
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    fn confirm_remove_recipe_pack(
        &mut self,
        id: String,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.changing_recipe_packs {
            return;
        }
        let question = format!("Remove {title} ({id})?");
        let prompt = window.prompt(
            gpui_kit::PromptLevel::Warning,
            &question,
            Some("Personal recipes are kept."),
            &["Cancel", "Remove"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if prompt.await == Ok(1) {
                this.update(cx, |app, cx| {
                    app.change_recipe_packs(
                        move || {
                            kosha_marketplace::uninstall(&id).map(|()| format!("Removed {title}"))
                        },
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }

    fn change_recipe_packs(
        &mut self,
        operation: impl FnOnce() -> Result<String, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.changing_recipe_packs {
            return;
        }
        self.changing_recipe_packs = true;
        cx.spawn(async move |this, cx| {
            let (result, installed, recipes) = cx
                .background_spawn(async move {
                    let result = operation();
                    (
                        result,
                        kosha_marketplace::installed(),
                        kosha_marketplace::load(),
                    )
                })
                .await;
            this.update(cx, |app, cx| {
                app.changing_recipe_packs = false;
                app.installed_recipe_packs = installed.unwrap_or_default();
                let empty = kosha_search::RecipeBook::default();
                app.search_bar.update(cx, |bar, _| {
                    bar.set_recipes(recipes.as_ref().unwrap_or(&empty));
                });
                let notice = match result {
                    Ok(message) if recipes.is_err() => {
                        format!("{message}. Fix the recipe configuration to use installed packs.")
                    }
                    Ok(message) => message,
                    Err(error) => format!("Could not change recipe packs: {error}"),
                };
                app.search_recipes = recipes;
                let text = app.filter.read(cx).value().to_string();
                app.query_text_changed(text, cx);
                app.state.notice = Some(notice);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ask for a disk image and read it.
    ///
    /// **Choosing one here is the request**, so this scan reads images whether
    /// or not the setting is on — the same rule as the Shift modifier, and for
    /// the same reason: it only ever turns the thing *on*, and somebody who
    /// picked a file out of a dialog headed "Scan a disk image" has said what
    /// they want more plainly than a setting could.
    ///
    /// The MFT strategy is deliberately not turned on with it. That one reads
    /// a *volume* on the running machine, and an image is not one.
    pub fn choose_image(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Read".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = prompt.await;
            this.update(cx, |this, cx| {
                if let Some(path) = this
                    .file_picker_selection(selection, cx)
                    .and_then(|paths| paths.into_iter().next())
                {
                    let options = crate::state::image_scan_options(this.state.scan_options);
                    this.scan_with(path, options, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Start a scan from a click, honouring the modifier that asks for speed.
    ///
    /// One place rather than four, because four sidebar rows start scans and a
    /// modifier that works on three of them is a modifier nobody trusts.
    fn start_scan(&mut self, path: PathBuf, event: &ClickEvent, cx: &mut Context<Self>) {
        if event.modifiers().shift {
            self.scan_the_fast_way(path, cx);
        } else if self.is_volume_root(&path) {
            self.open_drive(path, cx);
        } else {
            self.scan(path, cx);
        }
    }

    fn is_volume_root(&self, path: &std::path::Path) -> bool {
        self.volumes
            .iter()
            .any(|volume| kosha_core::snapshot::store::same_root(&volume.place.path, path))
    }

    /// Browsing a drive prefers its last record. Explicit scan commands and
    /// Shift-click bypass this path, and an unreadable cache is never a wall.
    fn open_drive(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.disarm();
        self.state.cancel_scan();
        if self.is_scanning() {
            self.state.phase = Phase::Idle;
        }
        self.cancel_search();
        self.search_index = None;
        self.load_generation += 1;
        self.comparison_generation += 1;
        self.comparison_busy = false;
        let generation = self.load_generation;
        if let Some(record) = self
            .last_drive_record
            .as_ref()
            .filter(|r| r.matches(&root))
            .or_else(|| self.pending_drive_records.get(&root))
        {
            let (path, meta, tree) = (
                record.path.clone(),
                record.meta.clone(),
                Arc::clone(&record.tree),
            );
            self.snapshot_opened(path, meta, tree, true, cx);
            return;
        }
        self.state.notice = Some("Opening last drive scan…".into());
        cx.notify();
        let reading = root.clone();
        let task = cx
            .background_executor()
            .spawn(async move { kosha_core::snapshot::store::open_automatic(&reading) });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match outcome {
                    Ok((path, meta, tree)) => {
                        this.snapshot_opened(path, meta, Arc::new(tree), true, cx)
                    }
                    Err(_) => this.scan(root, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Keep the fragmentation figure pointed at whatever quick look is showing.
    ///
    /// Called once per frame from `render`, which is the only place every route
    /// to a new selection passes through — the selection moves from the table,
    /// the treemap, a search result, an insight's "show me", a rescan and the
    /// keyboard, and a hook on each of those is a hook somebody will forget.
    /// The work here is one path comparison unless the answer is actually
    /// stale.
    fn track_fragmentation(&mut self, cx: &mut Context<Self>) {
        if !self.details_open || !kosha_core::supports_fragmentation() {
            self.fragmentation = None;
            return;
        }
        let Some(ready) = self.state.ready() else {
            self.fragmentation = None;
            return;
        };
        let Some(selected) = ready.selected else {
            self.fragmentation = None;
            return;
        };

        let path = ready.tree.path_of(selected);
        if self
            .fragmentation
            .as_ref()
            .is_some_and(|(at, _)| *at == path)
        {
            return; // Already measured, or already being measured.
        }
        // Marked before the task starts, so the next frame sees a match and
        // does not queue the same question again.
        self.fragmentation = Some((path.clone(), Fragments::Measuring));

        let task = cx.background_executor().spawn(async move {
            // One open and one ioctl each. Off the UI thread because a file on
            // a sleeping disk can take a moment, and a frame must never wait
            // for a spindle.
            let found = kosha_core::fragmentation(&path);
            let media = kosha_core::media_type(&path);
            (path, found, media)
        });

        cx.spawn(async move |this, cx| {
            let (path, found, media) = task.await;
            this.update(cx, |this, cx| {
                // The selection may have moved while the disk was thinking. An
                // answer about a file nobody is looking at any more is dropped
                // rather than shown against whatever is selected now.
                if this
                    .fragmentation
                    .as_ref()
                    .is_none_or(|(at, _)| *at != path)
                {
                    return;
                }
                let state = match found {
                    Ok(found) => Fragments::Known(found, media),
                    Err(_) => Fragments::Unavailable,
                };
                this.fragmentation = Some((path, state));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// What the command line asked for, once there is a tree to do it to.
    ///
    /// Called from both places a tree can arrive — a scan finishing and a
    /// snapshot opening — because every one of these is about the first second
    /// of the window's life and none of them care which kind of tree it is.
    /// They lived in the scan path alone, so `kosha-gui <snapshot> --since
    /// <other>` accepted both arguments, opened the snapshot, and quietly did
    /// nothing with the second one. A flag that is taken and ignored is worse
    /// than one that is refused.
    fn apply_pending(&mut self, cx: &mut Context<Self>) {
        // A command-line query only makes sense once there is a tree to run it
        // against, and only for the tree it was given for. The box already
        // shows the text; this is what makes it take effect.
        // Routed through `query_text_changed` rather than setting the filter
        // directly: that is also where suggestions are worked out, and taking
        // the short path left a startup query showing an error with no offer of
        // a way forward.
        if let Some(query) = self.pending_query.take() {
            self.query_text_changed(query, cx);
        } else {
            // A new scan/snapshot replaces the arena, so apply the visible
            // draft to that arena before the table can show unrelated rows.
            let text = self.filter.read(cx).value().to_string();
            self.query_text_changed(text, cx);
        }
        // Only against a disk. A snapshot records how big every file was, not
        // what was in it, so there is nothing here to hash — see
        // `find_duplicates`.
        if std::mem::take(&mut self.pending_duplicates) {
            self.find_duplicates(cx);
        }
        // Last, because a comparison needs the tree it is holding the snapshot
        // against to exist already.
        if let Some(path) = self.pending_since.take() {
            self.compare_with_snapshot(path, cx);
        }
    }

    fn scan_finished(&mut self, tree: Arc<SizeTree>, cx: &mut Context<Self>) {
        self.state.phase = Phase::Ready(Ready::new(Arc::clone(&tree)));
        if tree.stats().cancelled {
            self.state.notice = Some("Scan stopped. Results are incomplete.".into());
        }

        // Free space has very likely just changed, and this is the one moment
        // it is worth asking again: executing a plan finishes by rescanning, so
        // this covers deletions too without a second place to remember.
        self.refresh_places();

        if self.is_volume_root(tree.root_path()) && !tree.is_incomplete() {
            let cached = Arc::clone(&tree);
            let taken = std::time::SystemTime::now();
            self.last_drive_record =
                crate::drive_history::DriveRecord::new(Arc::clone(&tree), taken);
            if let Some(record) = &self.last_drive_record {
                self.pending_drive_records.remember(record.clone());
            }
            let saved_root = tree.root_path().to_path_buf();
            let saved_stamp = kosha_core::snapshot::Meta::of(&tree, taken, "").at_ms();
            let generation = self.load_generation;
            let task = cx
                .background_executor()
                .spawn(async move { kosha_core::snapshot::store::save_automatic(&cached, taken) });
            cx.spawn(async move |this, cx| {
                let outcome = task.await;
                this.update(cx, |this, cx| {
                    this.pending_drive_records
                        .finished(&saved_root, saved_stamp);
                    if let Err(error) = outcome
                        && this.load_generation == generation
                    {
                        this.state.notice =
                            Some(format!("Could not remember this drive scan: {error}"));
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }

        self.apply_pending(cx);
        cx.notify();

        // Analysis is pure and CPU-bound, so it goes straight to the background
        // pool and lands whenever it lands. The tree is already browsable.
        let context = self.settings.analysis_context();
        // Read on every scan rather than once at startup, so editing a pack
        // and pressing rescan is enough — the alternative is a restart to see
        // whether a rule you just wrote works, which is most of the work of
        // writing one.
        let packs = kosha_analyze::pack::load_all();
        if packs.has_problems() {
            // The same place a settings-file problem goes. A rule that did not
            // load has to be *said*: its author is looking at a file they wrote
            // and no finding from it, with nothing else to go on.
            // The short form: the status bar is one line, and a message that
            // runs off the edge loses exactly the half that says what to do.
            // The full text, with the fix, is what `kosha-cli` prints.
            let problems = packs.summaries();
            self.state.notice = Some(match problems.len() {
                1 => format!("Rule pack: {}", problems[0]),
                n => format!("Rule pack: {} (and {} more)", problems[0], n - 1),
            });
        }

        // `findings_with` rather than `analyze`: the latter is the pure rule
        // engine, and some findings have to open files or come from the user's
        // own rules. Both front ends go through the same function so the window
        // and the command line cannot drift apart about what a scan found.
        let rules = packs.rules;
        let generation = self.load_generation;
        let task = cx
            .background_executor()
            .spawn(async move { kosha_analyze::findings_with(&tree, &context, &rules) });

        cx.spawn(async move |this, cx| {
            let insights = task.await;
            this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                if let Some(ready) = this.state.ready_mut() {
                    ready.insights = Some(Arc::new(insights));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------- interaction

    /// Which rails this window is wide enough for.
    ///
    /// The width answer, never the preference — see `metrics::Rails::opened`
    /// for why the two are kept apart. Read from the viewport rather than
    /// remembered, so dragging the window edge changes it live.
    fn fitting_rails(&self, window: &Window) -> metrics::Rails {
        metrics::rails_for(
            f32::from(window.viewport_size().width),
            self.settings.columns.fixed_width(),
        )
    }

    /// Open or close quick look.
    ///
    /// It is drawn at the top of the insights rail, so in a window too narrow
    /// to hold that rail there is nowhere to put it. Saying so beats toggling
    /// a flag that changes nothing: a key that silently does nothing is
    /// indistinguishable from a key that is broken.
    fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fits = self.fitting_rails(window);
        if !fits.insights {
            self.state.notice = Some("Quick look needs a wider window".into());
            cx.notify();
            return;
        }

        self.details_open = !self.details_open;
        // Asking for quick look in a closed rail is asking for the rail. The
        // alternative is a key that reports success and changes nothing on
        // screen, which is the same as a key that is broken.
        if self.details_open && !self.settings.show_insights {
            self.settings.show_insights = true;
            self.settings_changed(window, cx);
            return;
        }
        cx.notify();
    }

    fn on_scan_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Capture Escape before a focused input can consume it. Other keys
        // retain normal bubbling and overlay dismissal keeps its usual order.
        if event.keystroke.key == "escape" && self.is_scanning() {
            self.on_key(event, window, cx);
            cx.stop_propagation();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modified = event.keystroke.modifiers.secondary();

        // These work wherever we are, including mid-scan and before any scan.
        if modified {
            match key {
                "f" => {
                    self.open_filter(window, cx);
                    return;
                }
                "r" => {
                    self.rescan(cx);
                    return;
                }
                // Open, as in every other application. Bound here as well as
                // offered in the palette because a binding a palette advertises
                // and the window does not answer is worse than no binding.
                "o" => {
                    self.choose_folder(cx);
                    return;
                }
                // The question mark is where people look for help.
                "/" | "?" => {
                    self.open_help(window, cx);
                    return;
                }
                "m" if event.keystroke.modifiers.shift => {
                    self.toggle_mode(cx);
                    return;
                }
                // Toggles rather than only opens: the same keys that summoned
                // it are the ones a hand already on the keyboard reaches for to
                // dismiss it.
                "k" => {
                    if self.command_open {
                        self.close_palette(window, cx);
                    } else {
                        self.open_palette(window, cx);
                    }
                    return;
                }
                _ => {}
            }
        }

        // A text box has the keyboard: only the keys about the search itself are
        // ours, and the rest belong to the text being edited. The rule lives in
        // `key_intent` so it can be tested; see the note there.
        match crate::state::key_intent(
            key,
            crate::state::Keys {
                editing_text: self.editing_text(window, cx),
                has_suggestions: !self.suggestions.is_empty(),
                palette_open: self.command_open,
                unlock_pending: self.state.asking_to_unlock,
                action_pending: self.state.pending.is_some(),
                owners_open: self.owners_open,
                help_open: self.help_open,
                settings_open: self.settings_open,
                details_open: self.details_open,
                comparison_open: self
                    .state
                    .ready()
                    .is_some_and(|ready| ready.comparison.is_some()),
                hunting: self.state.ready().is_some_and(Ready::is_hunting),
                scanning: self.is_scanning(),
                filter_open: self.filter_open,
            },
        ) {
            KeyIntent::Navigate => {}
            KeyIntent::LeaveToText => return,
            KeyIntent::CloseHelp => {
                self.close_help(window, cx);
                return;
            }
            KeyIntent::CloseSearch => {
                self.close_filter(window, cx);
                return;
            }
            KeyIntent::CommitSearch => {
                window.focus(&self.focus, cx);
                cx.notify();
                return;
            }
            KeyIntent::AcceptSuggestion => {
                let query = self.suggestions[0].query.clone();
                self.apply_query(query, window, cx);
                return;
            }
            KeyIntent::ClosePalette => {
                self.close_palette(window, cx);
                return;
            }
            KeyIntent::PaletteMove(by) => {
                self.move_command(by, cx);
                return;
            }
            KeyIntent::RunCommand => {
                self.run_selected_command(window, cx);
                return;
            }
            KeyIntent::CloseDetails => {
                self.details_open = false;
                cx.notify();
                return;
            }
            KeyIntent::CloseOwners => {
                self.close_owners(cx);
                return;
            }
            KeyIntent::CloseSettings => {
                self.close_settings(window, cx);
                return;
            }
            KeyIntent::CancelUnlock => {
                self.state.cancel_unlock();
                cx.notify();
                return;
            }
            KeyIntent::CancelAction => {
                self.state.pending = None;
                cx.notify();
                return;
            }
            KeyIntent::CloseComparison => {
                self.close_comparison(cx);
                return;
            }
            KeyIntent::StopHunt => {
                if let Some(ready) = self.state.ready() {
                    ready.stop_hunt();
                }
                self.state.notice = Some("Stopping the duplicate hunt\u{2026}".into());
                cx.notify();
                return;
            }
            KeyIntent::StopScan => {
                self.state.cancel_scan();
                cx.notify();
                return;
            }
        }

        let Some(ready) = self.state.ready_mut() else {
            if key == "escape" {
                self.state.cancel_scan();
                cx.notify();
            }
            return;
        };

        match key {
            "up" => ready.move_selection(-1),
            "down" => ready.move_selection(1),
            "pageup" => ready.move_selection(-12),
            "pagedown" => ready.move_selection(12),
            "home" => ready.move_selection(isize::MIN / 2),
            "end" => ready.move_selection(isize::MAX / 2),
            // In results, Enter goes to where the match lives rather than
            // trying to descend into it from a directory it is not in.
            "right" | "enter" => {
                if ready.showing_results() {
                    if let Some(selected) = ready.selected {
                        ready.reveal(selected);
                    }
                } else {
                    ready.descend();
                }
                self.sync_query_input(window, cx);
            }
            "left" | "backspace" => {
                ready.ascend();
                self.sync_query_input(window, cx);
            }
            // Toggles, like every other panel key: the hand that summoned it is
            // already on the key that dismisses it. Only reached when no text
            // box has the keyboard, so a space typed into a search is still a
            // space — `key_intent` decides that, and a test pins it.
            "space" => {
                self.toggle_details(window, cx);
                cx.notify();
                return;
            }
            // Everything Escape could have put away was handled by
            // `key_intent`. Reaching here means nothing was in the way, so it
            // clears the one thing left.
            "escape" => self.state.notice = None,
            _ => return,
        }

        // Keep the selection on screen; a keyboard-driven table that scrolls
        // out from under you is worse than one with no keyboard support.
        if let Some(ready) = self.state.ready()
            && let Some(selected) = ready.selected
            && let Some(index) = ready.rows.iter().position(|&r| r == selected)
        {
            self.rows_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Turn a finding into a reviewable plan.
    ///
    /// The plan is built and dry-run *before* the dialog appears, so the counts
    /// and the byte total the user approves come out of the same code path that
    /// will execute — not from a parallel estimate that can drift away from it.
    fn begin_action(&mut self, index: usize, cx: &mut Context<Self>) {
        // Before the mode check, because it is the more fundamental refusal: a
        // snapshot is a record of a disk as it was, and acting on it would
        // mean deleting today's file because of last Tuesday's size. Read-write
        // mode does not unlock this and nothing else does either — there is no
        // combination of choices that makes a tree from a file a safe thing to
        // delete from.
        if self.viewing_snapshot() {
            self.state.notice = Some(
                "This is a snapshot, not the disk. Scan the folder to act on what is there now."
                    .into(),
            );
            cx.notify();
            return;
        }
        if !self.state.mode().is_read_write() {
            self.state.notice = Some("Read-only. Switch modes in the title bar to act.".into());
            cx.notify();
            return;
        }

        let Some(ready) = self.state.ready() else {
            return;
        };
        let Some(insights) = ready.insights.clone() else {
            return;
        };
        let Some(insight) = insights.get(index) else {
            return;
        };

        let action = match insight.action {
            Suggested::Trash => Action::Trash,
            Suggested::EmptyContents => Action::EmptyContents,
            Suggested::Compress => {
                // Offered only where it works. The rule that produces this
                // suggestion cannot know the platform, so the refusal belongs
                // here — and it says so rather than doing nothing, because a
                // button that goes quiet is indistinguishable from a bug.
                if !kosha_core::supports_compression() {
                    self.state.notice = Some(
                        "Filesystem compression is a Windows feature; this platform \
                         has no per-file equivalent."
                            .into(),
                    );
                    cx.notify();
                    return;
                }
                Action::Compress
            }
            // ReviewOnly never acts. That is the whole point of it.
            Suggested::ReviewOnly => return,
        };

        let plan = ActionPlan::build(&ready.tree, &insight.nodes, action, &self.state.session);
        let preview = plan.dry_run();
        self.state.pending = Some(PendingAction {
            title: insight.title.clone(),
            plan,
            preview,
        });
        cx.notify();
    }

    /// Flip a boolean setting and persist. Taken as a closure so the panel does
    /// not need a field enum that has to be kept in step with the struct.
    pub(crate) fn update_settings(
        &mut self,
        edit: impl FnOnce(&mut Settings),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        edit(&mut self.settings);
        self.settings_changed(window, cx);
    }

    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        window.focus(&self.focus, cx);
        // Or a button left mid-question would still be armed the next time the
        // panel is opened, waiting for a click nobody remembers starting.
        self.disarm();
        cx.notify();
    }

    /// Re-run the current scan, which is how a changed scan option takes
    /// effect. Doing it automatically would be worse: the option is changed
    /// mid-browse and a surprise rescan loses the user's place.
    pub(crate) fn rescan(&mut self, cx: &mut Context<Self>) {
        let root = self.state.session.root().to_path_buf();
        if root.is_dir() {
            self.scan(root, cx);
        }
    }

    /// The plan awaiting approval, for the confirmation dialog to render.
    pub(crate) fn pending(&self) -> Option<&PendingAction> {
        self.state.pending.as_ref()
    }

    pub(crate) fn confirm_action(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.state.pending.take() else {
            return;
        };

        let mut audit = AuditLog::open_default();
        let report = match self.state.session.grant() {
            Ok(grant) => pending.plan.execute(&grant, &mut audit),
            Err(error) => {
                self.state.notice = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.state.notice = Some(report.summary());

        // Rescanning the whole root rather than patching the tree in place:
        // a rescan is seconds at worst and cannot disagree with the disk, which
        // is not true of any incremental update we could write.
        let root = self.state.session.root().to_path_buf();
        self.scan(root, cx);
    }

    pub(crate) fn cancel_action(&mut self, cx: &mut Context<Self>) {
        self.state.pending = None;
        cx.notify();
    }

    /// Ask to change files, or stop being able to.
    ///
    /// The question and the token live in [`crate::state::State`], which is
    /// where they can be tested without a window: the guarantee worth pinning
    /// is that the session is still read-only while the dialog is up.
    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.state.ask_to_unlock();
        cx.notify();
    }

    /// How many entries a finished scan could not read, when that is any.
    ///
    /// Only for a scan: a snapshot's own line is about the snapshot, and a
    /// scan in flight has not finished counting them.
    pub(crate) fn unreadable_count(&self) -> Option<u64> {
        let ready = self.state.ready()?;
        if ready.source.is_past() {
            return None;
        }
        let count = ready.tree.stats().unreadable;
        (count > 0).then_some(count)
    }

    /// Put the audit log in front of somebody.
    ///
    /// **The safety model lists it as a layer and says it is "openable from
    /// the UI".** It was not openable from anywhere: the only mention of the
    /// log in the whole window was the line that writes to it, and the answer
    /// to "what did this thing do to my machine last Tuesday" was a file whose
    /// name you had to know.
    ///
    /// Revealed in the file manager rather than handed to whatever opens
    /// `.jsonl` — which on a fresh Windows is nothing at all, and an
    /// association dialog is not an answer. The path is in the notice either
    /// way, because that is the part somebody can act on.
    ///
    /// A log that does not exist is not an error: it means nothing has been
    /// changed, which is the most reassuring thing this command can say, and
    /// creating the file to show it would make that false.
    pub(crate) fn show_audit_log(&mut self, cx: &mut Context<Self>) {
        let Some(path) = kosha_fsops::audit::where_it_lives() else {
            self.state.notice =
                Some("This platform does not say where application data belongs.".into());
            cx.notify();
            return;
        };
        if !path.exists() {
            self.state.notice = Some(format!(
                "Nothing has been changed, so there is no log yet. It would be at {}.",
                format::printable(&path.to_string_lossy()),
            ));
            cx.notify();
            return;
        }
        self.state.notice = Some(match reveal(&path) {
            Ok(()) => format!(
                "The audit log is at {}.",
                format::printable(&path.to_string_lossy())
            ),
            Err(error) => format!(
                "The audit log is at {} — it could not be opened from here: {error}",
                format::printable(&path.to_string_lossy()),
            ),
        });
        cx.notify();
    }

    pub(crate) fn asking_to_unlock(&self) -> bool {
        self.state.asking_to_unlock
    }

    pub(crate) fn confirm_unlock(&mut self, cx: &mut Context<Self>) {
        self.state.unlock_confirmed();
        cx.notify();
    }

    pub(crate) fn cancel_unlock(&mut self, cx: &mut Context<Self>) {
        self.state.cancel_unlock();
        cx.notify();
    }
}

impl Focusable for KoshaApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for KoshaApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let read_write = self.state.mode().is_read_write();
        // `Some(None)` is a job with no measurable end, `Some(Some(f))` one
        // that has got f of the way through, and `None` is nothing running.
        let job: Option<Option<f32>> = if self.is_scanning() || self.comparison_busy {
            Some(None)
        } else {
            self.state
                .ready()
                .and_then(|ready| ready.hunt.as_ref())
                .map(|hunt| hunt.progress.and_then(|p| p.fraction()))
        };

        // Before anything is built, because quick look reads the answer below
        // and this is where a moved selection is noticed.
        self.track_fragmentation(cx);

        // The confirmation dialog is built first so the borrow of `self` it
        // needs is released before the main tree borrows `self` mutably.
        let dialog = crate::confirm::render(self, &palette, cx);
        // Over everything, including the palette that may have summoned it:
        // it is a question, and a question behind a panel is not asked.
        let unlock = crate::confirm::unlock(self, &palette, cx);
        // Last in the stack, because it is summoned over whatever is already
        // there and must not end up behind it.
        let command_palette = self
            .command_open
            .then(|| crate::commands::panel(self, &palette, cx));

        let settings_panel = self
            .settings_open
            .then(|| crate::settings_panel::render(self, &palette, cx).into_any_element());
        let help_panel = self
            .help_open
            .then(|| crate::query_help::panel(self, &palette, cx));
        let owners_panel = self
            .owners_open
            .then(|| crate::owners::panel(self, &palette, cx));

        // What fits. The table is sized first and the rails take what is
        // left: see `metrics::rails_for`. Read from the viewport on every
        // frame, so a drag of the window edge rearranges the layout live.
        let fits = self.fitting_rails(window);
        // What is left after the reader has had their say. The status bar gets
        // `fits` rather than this, so it offers to widen the window only for a
        // rail that width took away.
        let rails = fits.opened(self.settings.show_places, self.settings.show_insights);

        div()
            .id("kosha")
            .relative()
            .track_focus(&self.focus)
            .key_context("Kosha")
            .on_action(
                cx.listener(|app, _: &crate::native_menu::Quit, window, cx| {
                    app.run_command(commands::Action::Quit, window, cx);
                }),
            )
            .capture_key_down(cx.listener(Self::on_scan_key))
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .bg(palette.canvas)
            .text_color(palette.text)
            .text_size(px(metrics::TEXT))
            .font_family(cx.theme().font_family.clone())
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_title_bar(fits, &palette, window, cx))
                    // One strip for both long jobs. A scan cannot say how
                    // far along it is — the tree is what it is finding out —
                    // so it slides; a hunt knows its candidate count before it
                    // opens a file, so it fills.
                    .children(job.map(|fraction| job_strip(&palette, fraction, cx.reduce_motion())))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .items_stretch()
                            .children(rails.places.then(|| self.render_sidebar(&palette, cx)))
                            .child(self.render_main(rails, &palette, cx))
                            .children(rails.insights.then(|| self.render_insights(&palette, cx))),
                    )
                    .child(self.render_status_bar(fits, &palette, cx)),
            )
            .children(settings_panel)
            .children(help_panel)
            .children(owners_panel)
            .children(command_palette)
            .children(dialog)
            .children(unlock)
            // Read-write mode is a standing fact about the window, so the
            // window itself says so. The chip in the title bar is a word in a
            // corner; this is legible from across the room, and from under
            // whatever panel happens to be open over it.
            //
            // Drawn last, and absolutely, rather than as a border on the root:
            // a border would take two pixels out of the layout, so every
            // switch of mode would shuffle the table under the pointer.
            .when(read_write, |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .border_2()
                        .border_color(palette.danger),
                )
            })
            // A column resize is followed here, at the root, because the
            // pointer leaves the handle it grabbed within a pixel or two.
            .on_mouse_move(cx.listener(Self::on_column_drag))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_column_drag(cx)),
            )
            // Releasing outside the window never delivers a mouse-up, and a
            // drag that outlives the button would resize on ordinary movement.
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_column_drag(cx)),
            )
    }
}

// ----------------------------------------------------------------- title bar

impl KoshaApp {
    /// `fits` is what the window is wide enough to hold, not what it is
    /// showing: a rail the window has outgrown reads as closed on its toggle,
    /// and pressing that toggle says so rather than flipping a preference that
    /// changes nothing on screen.
    fn render_title_bar(
        &self,
        fits: metrics::Rails,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mode = self.state.mode();
        let read_write = mode.is_read_write();
        let elevated = kosha_core::elevated();

        // Every control up here is a glyph or a single word, and three of them
        // had nothing to say for themselves. The panel toggles were given
        // tooltips when they were built; these are the rest of them.
        //
        // The theme button names the theme a click would move *to*, which is
        // the half its icon deliberately does not say — the icon is the state
        // the window is in, because one that showed the other state would read
        // as a claim about now. Between the two the question is answered from
        // both ends.
        let theme_hint: SharedString = if palette.dark {
            "Switch to the light theme".into()
        } else {
            "Switch to the dark theme".into()
        };
        // The same string the command palette builds for this, so the two
        // places that name the shortcut cannot come to disagree about it.
        let key = format!("{}\u{21e7}M", commands::MOD);
        let mode_hint: SharedString = if read_write {
            format!(
                "Read-write: actions can delete and move files, each one still asking \
                 first. Click to go back to read-only  ({key})"
            )
            .into()
        } else {
            format!(
                "Read-only: nothing in this window can change a file. Click to allow \
                 actions  ({key})"
            )
            .into()
        };

        crate::window_chrome::title_bar(
            h_flex()
                .w_full()
                .h(px(metrics::TITLE_BAR_HEIGHT))
                .px(px(12.0))
                .gap(px(12.0))
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .gap(px(8.0))
                        .items_center()
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_LG))
                                .text_color(palette.text)
                                .child("kosha"),
                        )
                        .children(self.render_snapshot_chip(palette, cx))
                        .children(self.render_incomplete_chip(palette))
                        .child(self.render_breadcrumb(palette, cx)),
                )
                .child(
                    // Read the cluster from the right, which is the edge
                    // the hand goes to: close, maximise and minimise are
                    // the window's own and are drawn by the library; then
                    // the mode chip, which is what a click in this window
                    // can do; then the two buttons that change only how it
                    // looks. Nothing sits between the mode chip and the
                    // window controls, because the mode chip is the one
                    // thing here somebody goes looking for.
                    h_flex()
                        .gap(px(8.0))
                        .items_center()
                        // Elevation is the same kind of statement as the
                        // mode chip — a standing fact about this window —
                        // but it is not the one people look for, and the
                        // order to its right is fixed, so it goes at the
                        // head of the cluster. Quiet, too, where the
                        // read-write chip shouts: elevation widens what a
                        // scan can see and changes nothing about what an
                        // action may do.
                        .when(elevated, |this| {
                            this.child(
                                chip()
                                    .border_color(palette.hairline)
                                    .bg(palette.surface)
                                    .text_color(palette.text_dim)
                                    .child("Administrator"),
                            )
                        })
                        .child(
                            title_button("settings-toggle", palette)
                                .tooltip(|window, cx| {
                                    tooltip("Settings: thresholds, units, and what a scan collects")
                                        .build(window, cx)
                                })
                                .child(
                                    Icon::empty()
                                        .path(crate::icons::GEAR)
                                        .size(px(metrics::TITLE_ICON)),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_settings(window, cx);
                                })),
                        )
                        .child(
                            title_button("theme-toggle", palette)
                                .tooltip(move |window, cx| {
                                    tooltip(theme_hint.clone()).build(window, cx)
                                })
                                // The theme the window is in, not the one
                                // a click would move to. A button that
                                // shows the other state reads as a claim
                                // about now, and the people who read it
                                // that way press it to get back what it
                                // appeared to be saying.
                                .child(
                                    Icon::new(if palette.dark {
                                        IconName::Moon
                                    } else {
                                        IconName::Sun
                                    })
                                    .size(px(metrics::TITLE_ICON)),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.settings.theme = if cx.theme().mode.is_dark() {
                                        ThemePreference::Light
                                    } else {
                                        ThemePreference::Dark
                                    };
                                    this.settings_changed(window, cx);
                                })),
                        )
                        .child(
                            // The one piece of chrome allowed to be
                            // coloured, because it is the one piece that
                            // changes what a click can do. Red rather than
                            // amber, and the window grows a red edge to
                            // match: the colour that means "this can
                            // destroy something" is the colour of the mode
                            // in which something can be destroyed.
                            chip()
                                .id("mode-chip")
                                .map(clickable_in_title_bar)
                                .cursor_pointer()
                                .tooltip(move |window, cx| {
                                    tooltip(mode_hint.clone()).build(window, cx)
                                })
                                .when(read_write, |this| {
                                    this.bg(palette.danger.opacity(0.18))
                                        .border_color(palette.danger)
                                        .text_color(palette.danger)
                                        .hover(|this| this.bg(palette.danger.opacity(0.30)))
                                })
                                // Hover has to carry on both themes, and a
                                // background alone does not: between
                                // `surface` and `hover` there is one step
                                // of grey, which is nothing on a dark
                                // canvas and barely a tint on a light one.
                                // The border and the text move with it,
                                // and those are the changes that read.
                                .when(!read_write, |this| {
                                    this.bg(palette.surface)
                                        .border_color(palette.hairline)
                                        .text_color(palette.text_dim)
                                        .hover(|this| {
                                            this.bg(palette.hover)
                                                .border_color(palette.text_faint)
                                                .text_color(palette.text)
                                        })
                                })
                                .child(mode.label())
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_mode(cx))),
                        )
                        .child(self.render_panel_toggles(fits, palette, cx)),
                ),
            palette,
            window,
            cx,
        )
    }

    /// Show or hide the three panels.
    ///
    /// In the title bar rather than on the panels themselves, because a
    /// control that lives on the thing it hides cannot bring it back. Each
    /// glyph is the shape of the window with that panel filled in, so the
    /// button says where the thing it controls is rather than what it is
    /// called.
    ///
    /// Directly beside the window's own minimise, maximise and close: these
    /// are the same kind of control — what is on screen rather than what is in
    /// the tree — and that is the corner people already go to for it.
    fn render_panel_toggles(
        &self,
        fits: metrics::Rails,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .gap(px(4.0))
            .items_center()
            .child(panel_toggle(
                "toggle-places",
                IconName::PanelLeft,
                "Show or hide the volumes and pinned folders",
                fits.places && self.settings.show_places,
                palette,
                cx.listener(|this, _, window, cx| {
                    let fits = this.fitting_rails(window);
                    if fits.places {
                        this.settings.show_places = !this.settings.show_places;
                        this.settings_changed(window, cx);
                    } else {
                        this.state.notice = Some("The volumes rail needs a wider window".into());
                        cx.notify();
                    }
                }),
            ))
            .child(panel_toggle(
                "toggle-table",
                IconName::PanelBottom,
                "Show or hide the results table",
                self.settings.show_table,
                palette,
                cx.listener(|this, _, window, cx| {
                    this.settings.show_table = !this.settings.show_table;
                    this.settings_changed(window, cx);
                }),
            ))
            .child(panel_toggle(
                "toggle-insights",
                IconName::PanelRight,
                "Show or hide quick look and the findings",
                fits.insights && self.settings.show_insights,
                palette,
                cx.listener(|this, _, window, cx| {
                    let fits = this.fitting_rails(window);
                    if fits.insights {
                        this.settings.show_insights = !this.settings.show_insights;
                        this.settings_changed(window, cx);
                    } else {
                        this.state.notice = Some("The insights rail needs a wider window".into());
                        cx.notify();
                    }
                }),
            ))
    }

    /// The chip that says this is not the disk.
    ///
    /// Beside the breadcrumb rather than over in the cluster on the right,
    /// because it belongs to the *path*: it is a statement about what the
    /// crumbs after it describe. A reader who does not notice this has been
    /// told nothing, so it is the one chip drawn in an accent rather than a
    /// neutral — and the status bar says it again, in words, at the other end
    /// of the window.
    fn render_snapshot_chip(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let one = self.snapshot_source()?;
        let taken = crate::snapshots::when(&one.meta);
        let automatic = one.automatic;
        let root = one.meta.root.clone();

        Some(
            // The shape from `chip` below, which this had spelled out by hand
            // seventy lines above the helper that spells it: the same height,
            // the same padding, the same radius, the same border, the same
            // size of text. Only the colours and the second word are its own.
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .child(
                    chip()
                        .flex_none()
                        .gap(px(6.0))
                        .border_color(palette.accent)
                        .bg(palette.accent.opacity(0.16))
                        .text_color(palette.accent)
                        .child(if automatic { "Saved scan" } else { "Snapshot" })
                        .child(div().text_color(palette.accent.opacity(0.75)).child(taken)),
                )
                .when(automatic, |view| {
                    view.child(
                        crate::ui::button("cached-drive-rescan", palette)
                            .map(clickable_in_title_bar)
                            .child("Scan again")
                            .tooltip(|window, cx| {
                                tooltip("Refresh this saved view from the drive").build(window, cx)
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.scan(root.clone(), cx)),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_incomplete_chip(&self, palette: &Palette) -> Option<AnyElement> {
        let ready = self.state.ready()?;
        if !ready.tree.is_incomplete() {
            return None;
        }
        let reason: SharedString = if ready.tree.stats().cancelled {
            "Stopped before every folder was scanned. Totals cover only the scanned entries. Automatic drive history was not replaced.".into()
        } else {
            "The depth limit left folders unscanned. Totals cover only the scanned entries. Automatic drive history was not replaced.".into()
        };
        Some(
            chip()
                .id("incomplete-scan")
                .flex_none()
                .border_color(palette.caution)
                .bg(palette.caution.opacity(0.12))
                .text_color(palette.caution)
                .child("Incomplete scan")
                .tooltip(move |window, cx| tooltip(reason.clone()).build(window, cx))
                .into_any_element(),
        )
    }

    fn render_breadcrumb(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(ready) = self.state.ready() else {
            return div().into_any_element();
        };
        let trail = ready.trail();
        // Deep trees would otherwise push the mode chip off the window.
        let shown: Vec<NodeId> = trail.iter().rev().take(4).rev().copied().collect();
        let elided = trail.len() > shown.len();

        h_flex()
            .gap(px(4.0))
            .items_center()
            .text_color(palette.text_dim)
            .text_size(px(metrics::TEXT_SM))
            .when(elided, |this| {
                this.child(div().text_color(palette.text_faint).child("…"))
            })
            .children(shown.iter().enumerate().map(|(index, &id)| {
                let last = index + 1 == shown.len();
                let label: SharedString = if id == ready.tree.root() {
                    ready.tree.root_path().display().to_string().into()
                } else {
                    format::printable(&ready.tree.name_lossy(id))
                        .into_owned()
                        .into()
                };
                h_flex()
                    .gap(px(4.0))
                    .items_center()
                    .when(index > 0, |this| {
                        this.child(div().text_color(palette.text_faint).child("▸"))
                    })
                    .child(
                        div()
                            .id(("crumb", id.index()))
                            .cursor_pointer()
                            .map(clickable_in_title_bar)
                            .px(px(4.0))
                            .rounded(px(metrics::RADIUS_SM))
                            .when(last, |this| this.text_color(palette.text))
                            .hover(|this| this.bg(palette.hover))
                            .child(label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate_to(id, window, cx);
                            })),
                    )
            }))
            .into_any_element()
    }
}

/// A title-bar chip: a word in a box, on the same line as everything else up
/// there. The colours are the caller's, because what a chip *says* is the only
/// thing that should decide them.
fn chip() -> gpui_kit::Div {
    div()
        .h(px(metrics::TITLE_CONTROL))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(metrics::RADIUS_SM))
        .border_1()
        .text_size(px(metrics::TEXT_SM))
}

/// A title-bar toggle that shows whether the panel it controls is open.
///
/// Filled when the panel is showing, faint and unfilled when it is not. A
/// toggle that looks the same in both states is a button people press twice to
/// find out what it did.
fn panel_toggle<F>(
    id: &'static str,
    icon: IconName,
    hint: &'static str,
    open: bool,
    palette: &Palette,
    on_click: F,
) -> impl IntoElement + use<F>
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    title_button(id, palette)
        // Three glyphs with no labels, and the least self-explanatory controls
        // in the window. Naming what each one hides is the whole of their
        // documentation.
        .tooltip(move |window, cx| tooltip(hint).build(window, cx))
        .when(!open, |this| {
            this.bg(palette.canvas)
                .border_color(palette.hairline.opacity(0.5))
                .text_color(palette.text_faint)
        })
        .when(open, |this| this.text_color(palette.text))
        .child(Icon::new(icon).size(px(metrics::TITLE_ICON)))
        .on_click(on_click)
}

/// A square icon button in the title bar: the settings gear, the theme toggle
/// and the three panel toggles, which are the same shape, the same size and
/// want the same hover.
///
/// The hover moves three things at once — fill, border and glyph. The fill
/// alone does not carry: on the dark canvas `surface` to `hover` is a single
/// step of grey, and on the light one it is a tint on white. A control that
/// gives nothing back under the pointer reads as decoration, and these two
/// spent a while looking like exactly that.
fn title_button(id: &'static str, palette: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    let palette = *palette;
    div()
        .id(id)
        .map(clickable_in_title_bar)
        // Part of the same answer the hover gives: the pointer changes shape
        // before the fill does, and it is the half that works at the edge of
        // vision.
        .cursor_pointer()
        .size(px(metrics::TITLE_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(metrics::RADIUS_SM))
        .border_1()
        .border_color(palette.hairline)
        .bg(palette.surface)
        .text_color(palette.text_dim)
        .hover(move |this| {
            this.bg(palette.hover)
                .border_color(palette.text_faint)
                .text_color(palette.text)
        })
}

// ------------------------------------------------------------ a scan running

/// Three pixels of accent under the title bar, while a long job runs.
///
/// `fraction` is how far along it is, where that can be known — a duplicate
/// hunt counts its candidates before it opens one — and `None` where it cannot:
/// a scan is discovering the tree it would have to measure itself against.
///
/// The one indicator that is always there. A job started from the command
/// palette while the settings panel or the query help is open covers the middle
/// of the window, and this is above all of it.
fn job_strip(palette: &Palette, fraction: Option<f32>, reduce_motion: bool) -> AnyElement {
    /// How much of the width the moving part covers.
    const SEGMENT: f32 = 0.28;

    let track = div()
        .relative()
        .flex_none()
        .w_full()
        .h(px(metrics::SCAN_STRIP))
        .overflow_hidden()
        .bg(palette.accent.opacity(0.18));

    // A known fraction needs no animation at all: the bar moving *is* the
    // progress, and an indeterminate sweep on top of a real number would be
    // decoration pretending to be information.
    if let Some(done) = fraction {
        return track
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(relative(done.clamp(0.0, 1.0)))
                    .bg(palette.accent),
            )
            .into_any_element();
    }

    if reduce_motion {
        // A repeating animation is rendered at its *start* state when motion
        // is reduced, and this one starts off the left edge — so the strip
        // would be an empty groove. A steady band says the same thing without
        // moving, and the count in the middle of the window is still counting.
        return track
            .child(div().absolute().inset_0().bg(palette.accent.opacity(0.55)))
            .into_any_element();
    }

    track
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(relative(SEGMENT))
                .bg(palette.accent)
                .with_animation(
                    "scan-strip",
                    Animation::new(Duration::from_millis(1200))
                        .repeat()
                        .with_easing(ease_in_out),
                    // From entirely off the left edge to entirely off the
                    // right one, so the strip is empty for an instant at each
                    // end rather than the segment appearing to bounce.
                    |this, delta| this.left(relative(delta * (1.0 + SEGMENT) - SEGMENT)),
                ),
        )
        .into_any_element()
}

/// What the middle of the window shows while a scan is running.
fn scanning(
    palette: &Palette,
    path: &std::path::Path,
    progress: Option<Progress>,
    reduce_motion: bool,
    stopping: bool,
    estimate: Option<crate::scan_timing::Estimate>,
    reference_total: Option<u64>,
    cx: &mut Context<KoshaApp>,
) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .child(
            h_flex()
                .gap(px(10.0))
                .items_center()
                .child(pulse(palette, reduce_motion))
                .child(
                    div()
                        .text_size(px(metrics::TEXT_XL))
                        .text_color(palette.text)
                        .child(if stopping {
                            "Stopping scan…"
                        } else {
                            "Scanning"
                        }),
                ),
        )
        .child(
            div()
                .max_w(px(560.0))
                .truncate()
                .text_color(palette.text_dim)
                .child(SharedString::from(path.display().to_string())),
        )
        .child(
            // Never a spinner without a number. The count is the proof that
            // something is happening, and on a slow volume the first level can
            // take long enough that a word on its own would not be believed.
            div()
                .text_size(px(metrics::TEXT_LG))
                .text_color(palette.text)
                .child(match progress {
                    Some(p) => SharedString::from(format!(
                        "{} · {} queued",
                        format::plural(p.entries as usize, "entry"),
                        format::plural(p.dirs_pending as usize, "directory"),
                    )),
                    None => SharedString::from("Reading the first level…"),
                }),
        )
        .child(
            div()
                .mt(px(6.0))
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text_faint)
                .child(if stopping {
                    "Waiting for current filesystem reads to finish"
                } else {
                    "Esc to cancel"
                }),
        )
        .when(!stopping, |view| {
            view.child(
                v_flex()
                    .items_center()
                    .gap(px(4.0))
                    .text_size(px(metrics::TEXT_SM))
                    .text_color(palette.text_dim)
                    .child(match estimate {
                        Some(estimate) => {
                            let speed = format!("{:.0} entries/s", estimate.entries_per_second);
                            match estimate.remaining {
                                Some(time) => SharedString::from(format!(
                                    "{speed} · ~{} remaining",
                                    format::duration(time)
                                )),
                                None => SharedString::from(format!("{speed} · {}",
                                    if progress.is_some_and(|p| p.dirs_scanned.is_some() && p.dirs_pending == 0) {
                                        "finalizing scan…"
                                    } else if reference_total.is_some_and(|total| progress.is_some_and(|p| p.entries >= total)) {
                                        "discovering more entries…"
                                    } else if reference_total.is_some() { "measuring remaining time…" }
                                    else { "total entries unknown" })),
                            }
                        }
                        None => SharedString::from("Measuring scan speed…"),
                    })
                    .child(
                        div().text_color(palette.text_faint).child(SharedString::from(match reference_total {
                            Some(total) => format!("Based on {} in the previous finished scan · 30-second average", format::plural(total as usize, "entry")),
                            None => "A remaining-time estimate needs a previous finished scan · 30-second average".into(),
                        })),
                    ),
            )
        })
        .when(!stopping, |view| {
            view.child(
                crate::ui::button("stop-scan", palette)
                    .child("Stop scan")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.cancel_scan();
                        cx.notify();
                    })),
            )
        })
}

/// A dot that breathes, so the panel is alive in the moment between two
/// progress updates — which is most moments, since they arrive sixteen times a
/// second and a level of a deep tree can take longer than that to finish.
fn pulse(palette: &Palette, reduce_motion: bool) -> AnyElement {
    let dot = div().size(px(10.0)).rounded_full().bg(palette.accent);

    if reduce_motion {
        return dot.into_any_element();
    }

    dot.with_animation(
        "scan-pulse",
        Animation::new(Duration::from_millis(900)).repeat(),
        // A triangle rather than an ease: the dot should fade out and back at
        // one steady rate, and easing both halves gives it a heartbeat, which
        // is a different and more alarming thing to say.
        |this, delta| this.opacity(0.35 + 0.65 * (1.0 - (delta * 2.0 - 1.0).abs())),
    )
    .into_any_element()
}

// ------------------------------------------------------------------- sidebar

impl KoshaApp {
    fn render_sidebar(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        // Built in loops rather than `map`: `cx` is a unique borrow and cannot
        // be captured by the `FnMut` an iterator adaptor would need.
        let mut personal = Vec::new();
        for place in &self.places {
            personal.push(self.place_row(palette, place, cx).into_any_element());
        }
        let mut distributions = Vec::new();
        for (index, distribution) in self.distributions.iter().enumerate() {
            distributions.push(
                self.distribution_row(palette, distribution, index, cx)
                    .into_any_element(),
            );
        }
        let mut scopes = Vec::new();
        for (index, (place, present)) in self.scopes.iter().enumerate() {
            scopes.push(
                self.scope_row(palette, place, *present, index, cx)
                    .into_any_element(),
            );
        }

        let mut volumes = Vec::new();
        for (index, volume) in self.volumes.iter().enumerate() {
            volumes.push(
                self.volume_row(palette, volume, index, cx)
                    .into_any_element(),
            );
        }
        let snapshots = crate::snapshots::rail(self, palette, cx);

        let body = v_flex()
            .id("sidebar")
            .size_full()
            .p(px(12.0))
            .pr(px(18.0))
            .gap(px(2.0))
            .overflow_y_scroll()
            .track_scroll(&self.sidebar_scroll)
            .child(crate::ui::section_label(palette, "SCAN"))
            .children(personal)
            // Above PINNED and VOLUMES because a distribution is a place you
            // scan, not a disk you watch fill up — Windows already reports the
            // whole of one as a single `ext4.vhdx` on whichever drive holds
            // it, and that number is in VOLUMES already and says nothing.
            .when(!distributions.is_empty(), |this| {
                this.child(div().h(px(10.0)))
                    .child(crate::ui::section_label(palette, "LINUX"))
                    .children(distributions)
            })
            // Only when there are any. An empty heading is a promise of
            // something the rail does not have.
            .when(!scopes.is_empty(), |this| {
                this.child(div().h(px(10.0)))
                    .child(crate::ui::section_label(palette, "PINNED"))
                    .children(scopes)
            })
            // Above the volumes, and only when there are any.
            //
            // They were below them, on the argument that everything above the
            // line is a disk and everything below it is a record of one. That
            // argument is still true and it cost more than it was worth: this
            // machine lists seven volumes, each two lines tall, so the section
            // began below the fold of a maximised window and every snapshot in
            // it was reachable only by scrolling a rail nobody had a reason to
            // scroll. A person who had taken snapshots in order to compare them
            // reported that there was no way to compare them. There was; it was
            // just further down than anybody would look.
            //
            // Volumes are a place to *go*, and the rail has other ways to get
            // there. Snapshots are a thing you have, and this is the only place
            // that lists them.
            .when(!snapshots.is_empty(), |this| {
                this.child(div().h(px(10.0)))
                    .child(crate::ui::section_label(palette, "SNAPSHOTS"))
                    .children(snapshots)
            })
            .child(div().h(px(10.0)))
            .child(crate::ui::section_label(palette, "VOLUMES"))
            .children(volumes);
        div()
            .relative()
            .w(px(metrics::SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .bg(palette.surface)
            .border_r_1()
            .border_color(palette.hairline)
            .child(body)
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.sidebar_scroll)
                        .id("places-scrollbar")
                        .viewport_from_layout()
                        .mode(ScrollbarMode::Always),
                ),
            )
    }

    fn place_row(
        &self,
        palette: &Palette,
        place: &crate::places::Place,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path = place.path.clone();
        let active = self
            .state
            .ready()
            .is_some_and(|r| r.tree.root_path() == path);
        let label = SharedString::from(place.label.clone());

        h_flex()
            .id(SharedString::from(place.path.display().to_string()))
            .cursor_pointer()
            .h(px(metrics::ROW_HEIGHT))
            .px(px(8.0))
            .gap(px(8.0))
            .items_center()
            .flex_none()
            .rounded(px(metrics::RADIUS_SM))
            .text_size(px(metrics::TEXT))
            .when(active, |this| {
                this.bg(palette.selected).text_color(palette.text)
            })
            .when(!active, |this| this.text_color(palette.text_dim))
            .hover(|this| this.bg(palette.hover))
            .child(
                div()
                    .flex_none()
                    .text_color(palette.text_faint)
                    .child(place.glyph),
            )
            .child(div().overflow_hidden().child(label))
            .tooltip({
                let title = place.path.clone();
                move |window, cx| tooltip(scans(&title)).build(window, cx)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.start_scan(path.clone(), event, cx);
            }))
    }

    /// A pinned folder.
    ///
    /// One that has gone — an unmounted drive, a directory that moved — stays
    /// on the rail, faint and inert and labelled "gone", rather than
    /// disappearing. This used to say "struck through", which nothing in the
    /// window has ever been: there is no `line_through` in the crate.
    /// Somebody who pinned it will want to know it is gone; a row that quietly
    /// vanishes reads as the app forgetting things.
    fn scope_row(
        &self,
        palette: &Palette,
        place: &crate::places::Place,
        present: bool,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path = place.path.clone();
        let active = self
            .state
            .ready()
            .is_some_and(|r| r.tree.root_path() == path);
        let label = SharedString::from(place.label.clone());
        let title = place.path.clone();

        h_flex()
            .id(("scope", index))
            .when(present, |this| this.cursor_pointer())
            .h(px(metrics::ROW_HEIGHT))
            .px(px(8.0))
            .gap(px(8.0))
            .items_center()
            .flex_none()
            .rounded(px(metrics::RADIUS_SM))
            .text_size(px(metrics::TEXT))
            .when(active, |this| {
                this.bg(palette.selected).text_color(palette.text)
            })
            .when(!active && present, |this| this.text_color(palette.text_dim))
            .when(!present, |this| this.text_color(palette.text_faint))
            .when(present, |this| this.hover(|s| s.bg(palette.hover)))
            .child(
                div()
                    .flex_none()
                    .text_color(palette.text_faint)
                    .child(place.glyph),
            )
            .child(div().flex_1().overflow_hidden().child(label))
            .when(!present, |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_size(px(metrics::TEXT_XS))
                        .text_color(palette.caution)
                        .child("gone"),
                )
            })
            .tooltip(move |window, cx| {
                tooltip(if present {
                    scans(&title)
                } else {
                    SharedString::from(format!("{} is not there now", title.display()))
                })
                .build(window, cx)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if present {
                    this.start_scan(path.clone(), event, cx);
                }
            }))
    }

    /// A WSL distribution: its name, and what its disk image costs Windows.
    ///
    /// The size is the point of the row. A WSL virtual disk grows and does not
    /// shrink back on its own, so it is routinely far larger than the files
    /// inside it — and every Windows tool, kosha's own volume list included,
    /// reports that big number and says nothing about the small one. Showing
    /// it here means the gap is visible before the scan, which is what tells
    /// somebody whether scanning is even the right thing to do.
    ///
    /// No bar, unlike a volume. A percentage would need a capacity to be a
    /// percentage of, and a virtual disk that grows on demand has no fixed
    /// one; drawing a bar against the drive it sits on would be comparing two
    /// different things.
    fn distribution_row(
        &self,
        palette: &Palette,
        distribution: &crate::places::Distribution,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path = distribution.place.path.clone();
        let active = self
            .state
            .ready()
            .is_some_and(|r| r.tree.root_path() == path);
        let label = SharedString::from(distribution.place.label.clone());
        let image = distribution
            .image
            .map(|bytes| SharedString::from(format!("{} image", self.bytes(bytes))));

        v_flex()
            .id(("distribution", index))
            .cursor_pointer()
            .px(px(8.0))
            .py(px(4.0))
            .gap(px(2.0))
            .flex_none()
            .rounded(px(metrics::RADIUS_SM))
            .text_size(px(metrics::TEXT))
            .when(active, |this| {
                this.bg(palette.selected).text_color(palette.text)
            })
            .when(!active, |this| this.text_color(palette.text_dim))
            .hover(|this| this.bg(palette.hover))
            .child(
                h_flex()
                    .gap(px(8.0))
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .text_color(palette.text_faint)
                            .child(distribution.place.glyph),
                    )
                    .child(div().flex_1().overflow_hidden().child(label)),
            )
            // Absent rather than zero when the registry did not say where the
            // image is, or it is not where it said. "0 B" would be a claim.
            .children(image.map(|image| {
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.text_faint)
                    .child(image)
            }))
            .tooltip({
                let title = distribution.place.path.clone();
                move |window, cx| tooltip(scans(&title)).build(window, cx)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.start_scan(path.clone(), event, cx);
            }))
    }

    /// A volume, with how full it is.
    ///
    /// The bar is the point of the row. A scan says "this folder is 40 GiB",
    /// which is a number; the bar says whether 40 GiB is most of the disk or
    /// none of it, which is the thing somebody is actually trying to find out.
    ///
    /// Those two paragraphs had ended up above `distribution_row`, which was
    /// inserted between them and the function they describe — so the
    /// distribution's documentation opened by describing a volume, and went on
    /// to explain that it has no bar, having just said the bar was the point.
    fn volume_row(
        &self,
        palette: &Palette,
        volume: &crate::places::Volume,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path = volume.place.path.clone();
        let active = self
            .state
            .ready()
            .is_some_and(|r| r.tree.root_path() == path);
        let label = SharedString::from(volume.place.label.clone());

        // Colour carries the only judgement in the sidebar, and it is one worth
        // making: a disk with a sliver left is the problem this app exists for,
        // and it should be visible before anybody reads a number.
        let fraction = volume.usage.map_or(0.0, |usage| usage.fraction_used());
        let bar = if fraction >= 0.95 {
            palette.danger
        } else if fraction >= 0.85 {
            palette.caution
        } else {
            palette.accent
        };

        let free = volume
            .usage
            .map(|usage| SharedString::from(format!("{} free", self.bytes(usage.available))));
        let percent = volume
            .usage
            .map(|usage| SharedString::from(format!("{:.0}%", usage.fraction_used() * 100.0)));

        v_flex()
            .id(("volume", index))
            .cursor_pointer()
            .px(px(8.0))
            .py(px(4.0))
            .gap(px(4.0))
            .flex_none()
            .rounded(px(metrics::RADIUS_SM))
            .text_size(px(metrics::TEXT))
            .when(active, |this| {
                this.bg(palette.selected).text_color(palette.text)
            })
            .when(!active, |this| this.text_color(palette.text_dim))
            .hover(|this| this.bg(palette.hover))
            .child(
                h_flex()
                    .gap(px(8.0))
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .text_color(palette.text_faint)
                            .child(volume.place.glyph),
                    )
                    .child(div().flex_1().overflow_hidden().child(label))
                    .children(percent.map(|percent| {
                        div()
                            .flex_none()
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(palette.text_faint)
                            .child(percent)
                    })),
            )
            // No bar at all when the volume could not be asked. An empty bar
            // would read as "nearly empty", which is a specific claim we have
            // no basis for.
            .children(volume.usage.map(|_| {
                div()
                    .h(px(3.0))
                    .w_full()
                    .rounded(px(2.0))
                    .bg(palette.hairline)
                    .child(
                        div()
                            .h_full()
                            // Anything in use is at least a visible sliver, so
                            // a nearly-empty disk does not look unmeasured.
                            .w(relative(fraction.clamp(0.02, 1.0)))
                            .rounded(px(2.0))
                            .bg(bar),
                    )
            }))
            .children(free.map(|free| {
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.text_faint)
                    .child(free)
            }))
            .tooltip({
                let title = volume.place.path.clone();
                move |window, cx| tooltip(SharedString::from(format!(
                    "Open {} — show its saved scan when available. Shift-click to scan again using the filesystem's index, where available.",
                    title.display()
                ))).build(window, cx)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.start_scan(path.clone(), event, cx);
            }))
    }
}

/// What a sidebar row promises: the path a click scans, and the modifier that
/// changes how it is read.
///
/// The label on a row is a friendly name — "Home", "This folder", a
/// distribution's name — and the path is what is about to be walked, which is
/// worth being able to check before a scan of somewhere unexpected starts. The
/// rows had none of this: one of them carried the path in an element that was
/// both `invisible()` and zero pixels wide, which draws nothing at all.
///
/// Shift is named here because the sidebar is the only place it does anything,
/// and the only other place it is written down is a paragraph at the bottom of
/// the settings panel, under a toggle, below the fold. A modifier nobody can
/// find is a modifier nobody has. Left out where neither of the two structures
/// it asks for exists, rather than promising a key that would do nothing.
fn scans(path: &std::path::Path) -> SharedString {
    let mut hint = format!("Scan {}", path.display());
    if kosha_core::exact_size_costs_extra() {
        hint.push_str("  (Shift: read the filesystem's own index, where there is one)");
    }
    SharedString::from(hint)
}

// ---------------------------------------------------------------- main region

impl KoshaApp {
    fn render_main(
        &mut self,
        rails: metrics::Rails,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let body = match &self.state.phase {
            Phase::Idle => self.render_welcome(rails, palette, cx).into_any_element(),
            Phase::Failed {
                path,
                message,
                hint,
            } => self
                .render_failure(palette, path, message, hint.as_deref())
                .into_any_element(),
            // A scan has no tree until it finishes — `state.ready()` is `None`
            // throughout — so this arm used to draw an empty treemap over an
            // empty table: a blank window, with the only sign of life eleven
            // points tall in the bottom corner.
            Phase::Scanning {
                path,
                progress,
                cancel,
            } => scanning(
                palette,
                path,
                *progress,
                cx.reduce_motion(),
                cancel.load(std::sync::atomic::Ordering::Relaxed),
                self.scan_timing.estimate(),
                self.scan_timing.reference_total(),
                cx,
            )
            .into_any_element(),
            // A comparison replaces the treemap and the table rather than
            // filtering them: its rows are differences, and half of them are
            // about paths that are not in the tree on screen at all.
            Phase::Ready(ready) if ready.comparison.is_some() => {
                let comparison = ready.comparison.as_ref().expect("just checked");
                crate::comparison::render(self, comparison, palette, cx)
            }
            Phase::Ready(_) if self.settings.show_table => {
                self.render_table(palette, cx).into_any_element()
            }
            Phase::Ready(_) => self.render_treemap(palette, false, cx).into_any_element(),
        };

        v_flex().flex_1().min_w(px(0.0)).h_full().child(body)
    }

    /// Keep the graph control at the graph's top edge, even when collapsed.
    fn render_graph_header(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let label = if self.settings.show_treemap {
            "Hide graph"
        } else {
            "Show graph"
        };
        h_flex()
            .flex_none()
            .h(px(28.0))
            .px(px(12.0))
            .items_center()
            .bg(palette.canvas)
            .border_b_1()
            .border_color(palette.hairline)
            .text_size(px(metrics::TEXT_XS))
            .text_color(palette.text_dim)
            .child("Space usage")
            .child(
                crate::ui::chip("toggle-space-graph")
                    .ml_auto()
                    .flex_none()
                    .px(px(8.0))
                    .role(gpui_kit::Role::Button)
                    .aria_label(label)
                    .hover(|this| this.bg(palette.hover).text_color(palette.text))
                    .child(label)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.settings.show_treemap = !this.settings.show_treemap;
                        this.settings_changed(window, cx);
                    })),
            )
            .into_any_element()
    }

    /// The first thing anybody sees, so it has to be true.
    ///
    /// "Pick a location on the left" was written when there was always a rail
    /// on the left. In a window narrow enough to have dropped it, that
    /// sentence points at nothing — found by opening the Linux build at 760px
    /// and reading what it said.
    fn render_welcome(
        &self,
        rails: metrics::Rails,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .child(
                div()
                    .text_size(px(metrics::TEXT_XL))
                    .text_color(palette.text)
                    .child("What is eating your disk?"),
            )
            .child(div().text_color(palette.text_dim).child(if rails.places {
                SharedString::from("Pick a location on the left, or choose any folder.")
            } else {
                // The palette lists the same places the rail would have, so
                // this is a different route to the same thing rather than a
                // consolation prize. `MOD` so a Mac is told about ⌘.
                SharedString::from(format!(
                    "Scan your home directory, or press {}K to find another.",
                    crate::commands::MOD
                ))
            }))
            .child(
                h_flex()
                    .mt(px(8.0))
                    .gap(px(8.0))
                    .items_center()
                    .child(
                        // The one button a first run is meant to press, and the
                        // one button in the window that was not built out of
                        // the shared shapes: its own radius, its own padding,
                        // no border, no pointer, and a height nothing else
                        // uses. None of that was a decision — it is what
                        // `ui.rs` exists to stop, written before `ui.rs`
                        // existed and then not revisited, on the screen with
                        // the fewest other things to compare it against.
                        crate::ui::primary("scan-home", palette, false)
                            .child("Scan home directory")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(home) = crate::places::home() {
                                    this.scan(home, cx);
                                }
                            })),
                    )
                    .child(
                        // Plain, deliberately: colour is reserved for meaning,
                        // so a second filled
                        // button here would make neither of them the answer.
                        // Home is still what a first run is meant to press;
                        // this is what everybody who wanted something else had
                        // no way to say.
                        crate::ui::button("choose-folder", palette)
                            .child("Choose a folder…")
                            .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                    ),
            )
    }

    fn render_failure(
        &self,
        palette: &Palette,
        path: &std::path::Path,
        message: &str,
        hint: Option<&str>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .child(
                // The same size as "Scanning" and "What is eating your disk?".
                // All three are the whole window saying one thing, and this one
                // was a step smaller than its two siblings — which made the
                // failure the quietest of the three states, exactly backwards.
                div()
                    .text_size(px(metrics::TEXT_XL))
                    .text_color(palette.danger)
                    .child("That scan could not start"),
            )
            .child(
                div()
                    .text_color(palette.text_dim)
                    .child(SharedString::from(path.display().to_string())),
            )
            .child(
                div()
                    .text_color(palette.text_faint)
                    .child(SharedString::from(message.to_string())),
            )
            // Below the reason and in the accent, because it is the only line
            // on this screen anybody can act on.
            .when_some(hint, |this, hint| {
                this.child(
                    div()
                        .mt(px(4.0))
                        .text_color(palette.accent)
                        .child(SharedString::from(hint.to_string())),
                )
            })
    }
}

/// What the window can offer about a scan that would not start.
///
/// Only one failure has an answer the window can give, and it is the one
/// somebody reaches by dropping a `.vhdx` on it: reading disk images is a
/// preference, off by default, so the scan refuses a file it would otherwise
/// have read. "kosha was not asked to read images" is true and leaves nowhere
/// to go — the two ways to ask are a held modifier and a setting, and neither
/// is guessable from the sentence.
fn failure_hint(error: &kosha_core::ScanError) -> Option<String> {
    match error {
        kosha_core::ScanError::ImageNotRead(_) => Some(
            concat!(
                "Hold Shift when you start a scan to read it, ",
                "or turn on reading disk images in settings.",
            )
            .into(),
        ),
        _ => None,
    }
}

// ------------------------------------------------------------------- treemap

impl KoshaApp {
    /// How many levels the treemap draws inside the directory in view.
    ///
    /// Two, so a directory shows its children and their children at once:
    /// enough to see that one project's `target` is the whole of it without
    /// clicking, and not so much that the picture becomes a texture. Nesting
    /// stops on its own wherever a tile is too small to subdivide, so this is
    /// a ceiling rather than a promise.
    const TREEMAP_DEPTH: u8 = 2;

    /// `table` is whether the results table is showing under it, because
    /// that decides whether the map takes its share of the height or all of
    /// it. With the table shut and the map still 42% tall, the window would be
    /// half empty and nothing would say why.
    fn render_treemap(
        &mut self,
        palette: &Palette,
        table: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bounds = self.treemap_bounds;
        let hovered = self.hovered;

        let Some(ready) = self.state.ready() else {
            return div()
                .h(px(bounds.h))
                .w_full()
                .bg(palette.canvas)
                .into_any_element();
        };

        let units = self.units();
        // Flat while showing results. The rows are then matches from all over
        // the tree rather than the contents of one directory, and their
        // children are not results — nesting would fill the map with things
        // that did not match the query the user is looking at.
        let depth = if ready.showing_results() {
            0
        } else {
            Self::TREEMAP_DEPTH
        };
        let layout = treemap::layout(&ready.tree, &ready.rows, bounds, 4.0, depth);
        let pooled = layout.pooled;
        let tree = Arc::clone(&ready.tree);
        let selected = ready.selected;

        let cells: Vec<_> = layout
            .tiles
            .iter()
            .map(|tile| {
                let node = tree.node(tile.id);
                let name = format::printable(&tree.name_lossy(tile.id)).into_owned();
                let category = Category::dominant(&tree, tile.id);
                let colour = palette.tile(category, node.depth.saturating_sub(1));
                let is_selected = selected == Some(tile.id);
                let is_hovered = hovered == Some(tile.id);
                // Labels only where they fit. A name broken across two lines
                // mid-word ("TinyWeb / wser") reads worse than no label at all,
                // so the width bar is set above the longest name we will show.
                // The same test decides whether the layout reserved room for
                // one, so a label can never land on top of a nested tile.
                let roomy = treemap::labelled(tile.rect);
                let nested = tile.depth > 0;
                let id = tile.id;

                div()
                    .id(("tile", id.index()))
                    .cursor_pointer()
                    .absolute()
                    .left(px(tile.rect.x))
                    .top(px(tile.rect.y))
                    .w(px((tile.rect.w - 2.0).max(1.0)))
                    .h(px((tile.rect.h - 2.0).max(1.0)))
                    .rounded(px(3.0))
                    // Nested tiles sit on their parent and must read as inside
                    // it, not beside it: more opaque so they separate from the
                    // ground they are drawn on, with a hairline of that ground
                    // showing between siblings.
                    .bg(colour.opacity(if is_hovered {
                        0.98
                    } else if nested {
                        0.92
                    } else {
                        0.75
                    }))
                    .when(nested, |this| {
                        this.border_1().border_color(palette.canvas.opacity(0.35))
                    })
                    .when(is_selected, |this| {
                        this.border_2().border_color(palette.text)
                    })
                    .when(roomy, |this| {
                        this.p(px(6.0)).child(
                            v_flex()
                                .gap(px(2.0))
                                .overflow_hidden()
                                .child(
                                    div()
                                        // One line, ellipsised from the middle,
                                        // for the same reason the table's names
                                        // are. A tile is only ever a few words
                                        // wide, so a wrapping name broke across
                                        // the extension — `…-final-v3.tar.`
                                        // above `gz` — which reads as a
                                        // rendering fault rather than as a long
                                        // name. The second line of a two-line
                                        // label also pushed the size out of a
                                        // tile that had been measured as having
                                        // room for both.
                                        .min_w(px(0.0))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis_middle()
                                        .text_size(px(if nested {
                                            metrics::TEXT_XS
                                        } else {
                                            metrics::TEXT_SM
                                        }))
                                        .text_color(if palette.dark {
                                            palette.canvas
                                        } else {
                                            palette.text
                                        })
                                        .child(SharedString::from(name.clone())),
                                )
                                .child(
                                    div()
                                        // `1.90 GiB` is short, but a tile only
                                        // just wide enough for a label is not
                                        // always wide enough for both words.
                                        .min_w(px(0.0))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_size(px(metrics::TEXT_XS))
                                        .text_color(if palette.dark {
                                            palette.canvas.opacity(0.75)
                                        } else {
                                            palette.text.opacity(0.7)
                                        })
                                        .child(SharedString::from(format::bytes_in(
                                            node.allocated,
                                            units,
                                        ))),
                                ),
                        )
                    })
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.hovered != Some(id) {
                            this.hovered = Some(id);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(ready) = this.state.ready_mut() {
                            ready.selected = Some(id);
                        }
                        let is_dir = this
                            .state
                            .ready()
                            .is_some_and(|ready| ready.tree.node(id).is_dir());
                        if is_dir {
                            this.navigate_to(id, window, cx);
                        } else {
                            cx.notify();
                        }
                    }))
            })
            .collect();

        // A query that matched nothing, or a directory with nothing in it,
        // otherwise leaves a large blank rectangle where the picture goes —
        // which reads as something failing to load rather than as an answer.
        let empty = (cells.is_empty() && pooled == 0).then(|| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(crate::ui::note(
                    palette,
                    if matches!(ready.search, Search::Pending) {
                        "Searching this scan\u{2026}"
                    } else if matches!(ready.search, Search::Invalid { .. }) {
                        "Fix the search below to see results."
                    } else if ready.showing_results() {
                        "Nothing matched."
                    } else if !ready.filter.trim().is_empty() {
                        "Nothing here matches that."
                    } else if ready.tree.root_unreadable() {
                        // **A folder nothing could be read from is not an
                        // empty folder.** Measured on `C:\System Volume
                        // Information`, which an ordinary account cannot
                        // open: this said "This folder is empty." in the
                        // middle of the window, with `1 unreadable` in the
                        // status bar as the only thing to the contrary.
                        // `ui.md` promises the opposite — "an inline card
                        // explaining exactly which permission is missing and
                        // the platform-specific way to grant it".
                        if cfg!(windows) {
                            "This folder could not be read. An administrator can: \
                             the command palette offers \u{201c}Restart as \
                             administrator\u{201d}."
                        } else {
                            "This folder could not be read. Check its permissions, \
                             or open it as its owner."
                        }
                    } else {
                        "This folder is empty."
                    },
                ))
        });

        Self::treemap_frame(table, cx)
            .children(cells)
            .children(empty)
            // Last, so it is painted over the tiles rather than under them.
            // GPUI paints children in order, and this sat inside the frame
            // before the tiles were appended — so the caption was drawn and
            // then covered by the very tiles it was labelling.
            .children(pooled_caption(palette, pooled))
            .into_any_element()
    }

    /// The treemap's container, and the measurement that makes it fit.
    ///
    /// The map starts at a share of the centre column and shrinks when the
    /// search controls and result rows need more room. Flex decides the actual
    /// pixels; the `canvas` reads them back
    /// and stores them, and the next frame lays the tiles out into the size the
    /// window really has.
    ///
    /// That measurement is the whole point. `treemap_bounds` existed and was
    /// never assigned, so every layout went into the 800×360 it was
    /// initialised with: on a maximised window the map stopped two-thirds of
    /// the way across and left a wide band of empty canvas beside it. Invisible
    /// at the default window size, where 800 happens to be about right.
    fn treemap_frame(table: bool, cx: &mut Context<Self>) -> gpui_kit::Div {
        let view = cx.entity();

        div()
            .relative()
            .w_full()
            .when(table, |this| {
                this.flex_initial()
                    .h(relative(TREEMAP_SHARE))
                    .min_h(px(TREEMAP_MIN_H))
                    .max_h(px(TREEMAP_MAX_H))
            })
            .when(!table, |this| this.flex_1().min_h(px(0.0)))
            .p(px(TREEMAP_PAD))
            .overflow_hidden()
            .child(
                gpui_kit::canvas(
                    move |bounds, _window, cx| {
                        let size = Rect::new(
                            0.0,
                            0.0,
                            f32::from(bounds.size.width) - TREEMAP_PAD * 2.0,
                            f32::from(bounds.size.height) - TREEMAP_PAD * 2.0,
                        );
                        // Only on a real change. Storing unconditionally
                        // would ask for a frame, which would measure, which
                        // would store — a repaint loop rather than a
                        // measurement.
                        let changed = view.update(cx, |this, _| {
                            if this.treemap_bounds.approx_eq(size) {
                                return false;
                            }
                            this.treemap_bounds = size;
                            true
                        });
                        // Deferred, because this runs during prepaint: the
                        // frame being built has already read the entity, so
                        // both `notify` and `window.refresh` from here leave
                        // the new size sitting in the field with nothing
                        // redrawing from it — which looks exactly like the
                        // measurement not working. `defer` runs after the
                        // current cycle, where a notify schedules a real frame.
                        if changed {
                            let view = view.clone();
                            cx.defer(move |cx| {
                                view.update(cx, |_, cx| cx.notify());
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }
}

/// "+412 smaller": the top-level children too small to draw.
///
/// A squarified layout puts its smallest tiles in the bottom-right corner, so
/// this necessarily sits over exactly the tiles it is talking about. Opaque
/// with a hairline border, then, rather than translucent — through a
/// translucent chip the mosaic behind it read as a rendering fault rather than
/// as a label.
fn pooled_caption(palette: &Palette, pooled: usize) -> Option<gpui_kit::Div> {
    (pooled > 0).then(|| {
        div()
            .absolute()
            .right(px(12.0))
            .bottom(px(10.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(metrics::RADIUS_SM))
            .bg(palette.raised)
            .border_1()
            .border_color(palette.hairline)
            .text_size(px(metrics::TEXT_XS))
            .text_color(palette.text_faint)
            .child(SharedString::from(format!(
                "+{} smaller",
                format::count(pooled as u64)
            )))
    })
}

/// The treemap's share of the centre column, and the bounds it is kept within.
///
/// Proportional rather than fixed because the alternative was a 360-pixel band
/// that looked right at one window size and wrong at every other.
const TREEMAP_SHARE: f32 = 0.42;
const TREEMAP_MIN_H: f32 = 36.0;
const TREEMAP_MAX_H: f32 = 560.0;
/// Breathing room between the map and the panels around it. Subtracted from
/// the measured bounds, because the tiles are laid out inside it.
const TREEMAP_PAD: f32 = 8.0;

// ------------------------------------------------------------- quick look

impl KoshaApp {
    /// `Space` — everything known about the selected entry.
    ///
    /// Rendered into the right rail rather than over the table, because the
    /// panel follows the selection: a hand on the arrow keys walks the rows and
    /// watches this change, so it must not be sitting on the rows. The first
    /// version floated above them, and a screenshot showed it covering the name
    /// of every row below the selected one.
    ///
    /// `None` when nothing is selected. A panel that appears only to say
    /// "nothing selected" teaches people the key is broken.
    /// How many pieces the selected entry is in, as a line of text.
    ///
    /// `None` where there is nothing worth saying, which is most of the time:
    /// the panel earns its place by being short, and "1 piece" on a healthy
    /// file is a row that pushes a useful one off the bottom.
    ///
    /// **Solid state says nothing at all.** Fragmentation there costs a
    /// pointer dereference rather than a seek, so a number invites somebody to
    /// go and defragment an SSD — which wears it out and buys nothing. The
    /// media is asked about first and the row is dropped on anything without a
    /// head that has to move. Where the device would not say, the count is
    /// still shown without the judgement: the fact is true whatever the disk
    /// is, and it is only the advice that depends on knowing.
    fn fragmentation_row(&self) -> Option<String> {
        use kosha_core::MediaType;

        let (_, state) = self.fragmentation.as_ref()?;
        match state {
            Fragments::Measuring => Some("measuring…".into()),
            Fragments::Unavailable => None,
            Fragments::Known(found, media) => {
                if *media == Some(MediaType::SolidState) || !found.is_fragmented() {
                    return None;
                }
                let pieces = format::plural(found.extents as usize, "piece");
                let more = if found.truncated { "over " } else { "" };
                Some(match media {
                    // Named only where it is the reason the number matters.
                    Some(MediaType::Spinning) => format!("{more}{pieces}, on a spinning disk"),
                    _ => format!("{more}{pieces}"),
                })
            }
        }
    }

    fn render_details(&self, palette: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ready = self.state.ready()?;
        let selected = ready.selected?;
        let d = crate::details::of(&ready.tree, selected);
        let units = self.units();
        let now = format::now_unix();
        let mono = cx.theme().mono_font_family.clone();

        let line = |label: SharedString, value: String| {
            h_flex()
                .w_full()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .text_size(px(metrics::TEXT_SM))
                        .text_color(palette.text_faint)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(metrics::TEXT_SM))
                        .font_family(mono.clone())
                        .text_color(palette.text)
                        .child(SharedString::from(value)),
                )
        };
        let row = |label: &'static str, value: String| line(SharedString::from(label), value);

        // The two size lines are the point of the panel, so the difference
        // between them is named rather than left as arithmetic for the reader.
        let difference = if d.saved > 0 {
            Some(row(
                "saved by the filesystem",
                format::bytes_in(d.saved, units),
            ))
        } else if d.slack > 0 {
            Some(row("cluster slack", format::bytes_in(d.slack, units)))
        } else {
            None
        };

        let body = v_flex()
            .gap(px(4.0))
            .child(row("on disk", format::bytes_in(d.allocated, units)))
            .child(row("apparent", format::bytes_in(d.logical, units)))
            .children(difference)
            .child(row(
                "share of this folder",
                format!("{:.1}%", d.of_parent * 100.0),
            ))
            // Suppressed one level down, where the containing folder *is*
            // the scan: two identical percentages under different labels read
            // as a bug in one of them.
            .children(
                (d.depth > 1)
                    .then(|| row("share of the scan", format!("{:.1}%", d.of_scan * 100.0))),
            )
            // Only where the scan collected it. A blank owner line on every
            // entry would be a promise the window keeps failing to keep.
            .children(
                self.tree()
                    .and_then(|tree| tree.owners().of(selected))
                    .map(|owner| row("owner", owner.label().into_owned())),
            )
            .child(row("modified", format::age(d.mtime, now)))
            .children(d.files_below.map(|files| {
                row(
                    "files below",
                    format!(
                        "{} in {}",
                        format::count(u64::from(files)),
                        format::plural(d.directories_below.unwrap_or(0) as usize, "folder"),
                    ),
                )
            }))
            .children(d.deepest_below.filter(|&levels| levels > 0).map(|levels| {
                row(
                    "deepest",
                    format!("{} below", format::plural(levels as usize, "level")),
                )
            }))
            .children(d.largest_child.as_ref().map(|(name, bytes)| {
                row(
                    "largest inside",
                    format!("{name} · {}", format::bytes_in(*bytes, units)),
                )
            }))
            .children(self.fragmentation_row().map(|text| row("in", text)));

        // What is actually in there, which is the question a directory raises
        // and the table cannot answer without drilling into it.
        let breakdown = (!d.extensions.is_empty()).then(|| {
            v_flex()
                .gap(px(4.0))
                .child(crate::ui::section_label(palette, "BY EXTENSION"))
                .children(
                    d.extensions
                        .iter()
                        .map(|e| {
                            // A leading dot reads as an extension; the group
                            // for names without one needs words instead.
                            let label = if e.ext.is_empty() {
                                SharedString::from("no extension")
                            } else {
                                SharedString::from(format!(".{}", e.ext))
                            };
                            line(
                                label,
                                format!(
                                    "{} · {}",
                                    format::bytes_in(e.allocated, units),
                                    format::plural(e.files as usize, "file")
                                ),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
                .children(d.other_extensions.as_ref().map(|rest| {
                    row(
                        "everything else",
                        format!(
                            "{} · {}",
                            format::bytes_in(rest.allocated, units),
                            format::plural(rest.files as usize, "file")
                        ),
                    )
                }))
        });

        // Each caveat is a reason a number above it is not the plain truth.
        let caveats = (!d.caveats.is_empty()).then(|| {
            v_flex().gap(px(4.0)).mt(px(6.0)).children(
                d.caveats
                    .iter()
                    .map(|caveat| {
                        div()
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(palette.caution)
                            .child(caveat.line())
                    })
                    .collect::<Vec<_>>(),
            )
        });

        Some(
            div()
                .w_full()
                .flex_none()
                .p(px(10.0))
                .rounded(px(6.0))
                .bg(palette.canvas)
                .border_1()
                .border_color(palette.hairline)
                .child(
                    v_flex()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(metrics::TEXT))
                                .text_color(palette.text)
                                .child(SharedString::from(d.name.clone())),
                        )
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(palette.text_faint)
                                .child(SharedString::from(format!(
                                    "{} · {}",
                                    d.kind,
                                    d.category.label()
                                ))),
                        )
                        .child(
                            div()
                                // Allowed to wrap: a path is longer than this
                                // rail is wide, and a clipped one is a path you
                                // cannot read or retype, which is most of why
                                // it is here.
                                .mb(px(8.0))
                                .text_size(px(metrics::TEXT_XS))
                                .font_family(mono.clone())
                                .text_color(palette.text_dim)
                                .child(SharedString::from(d.path.clone())),
                        ),
                )
                .child(body)
                .children(breakdown)
                .children(caveats)
                .into_any_element(),
        )
    }
}

// --------------------------------------------------------------------- table

impl KoshaApp {
    fn render_table(&mut self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *palette;
        let count = self.state.ready().map_or(0, |r| r.rows.len());
        let mono = cx.theme().mono_font_family.clone();
        let units = self.units();
        // Cloned into the row builder rather than read through `this`, which
        // the closure already holds for the tree. It is four columns.
        let layout = self.settings.columns.clone();
        // Results come from all over the tree, so a bare name is ambiguous:
        // twenty rows called `index.js` tell you nothing about which is which.
        let show_paths = self.state.ready().is_some_and(Ready::showing_results);
        let graph = self
            .settings
            .show_treemap
            .then(|| self.render_treemap(&palette, true, cx).into_any_element());

        // The graph and rows share the same flex column so the search controls
        // take their measured height before the remaining space is divided.
        v_flex()
            .size_full()
            .child(self.render_graph_header(&palette, cx))
            .children(graph)
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(palette.hairline)
                    .child(self.render_query_bar(&palette, cx)),
            )
            .children(crate::query_help::suggestions(self, &palette, cx))
            .child(self.render_table_header(&palette, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(metrics::ROW_HEIGHT * 2.0))
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "rows",
                            count,
                            cx.processor(
                                move |this, range: std::ops::Range<usize>, _window, cx| {
                                    let mono = mono.clone();
                                    let layout = &layout;
                                    let Some(ready) = this.state.ready() else {
                                        return Vec::new();
                                    };
                                    range
                                        .filter_map(|index| {
                                            let id = *ready.rows.get(index)?;
                                            Some(row(
                                                ready,
                                                id,
                                                index,
                                                &palette,
                                                layout,
                                                mono.clone(),
                                                units,
                                                show_paths,
                                                cx,
                                            ))
                                        })
                                        .collect()
                                },
                            ),
                        )
                        .track_scroll(&self.rows_scroll)
                        .size_full(),
                    )
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::vertical(&self.rows_scroll)
                                .id("results-scrollbar")
                                .viewport_from_layout()
                                .mode(ScrollbarMode::Always),
                        ),
                    ),
            )
    }

    fn render_table_header(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let sort = self.state.ready().map(|r| r.sort).unwrap_or_default();
        let dragging = self.column_drag.map(|drag| drag.column);

        h_flex()
            .flex_none()
            .h(px(metrics::ROW_HEIGHT))
            .px(px(12.0))
            .items_center()
            .bg(palette.surface)
            .border_b_1()
            .border_color(palette.hairline)
            .text_size(px(metrics::TEXT_XS))
            .child(name_header(palette, sort, cx))
            .children(
                self.settings
                    .columns
                    .visible()
                    .map(|(column, width)| {
                        column_header(palette, sort, column, width, dragging == Some(column), cx)
                    })
                    // Collected because each closure captures `column`, which
                    // cannot be captured by the `FnMut` an iterator adaptor
                    // would need.
                    .collect::<Vec<_>>(),
            )
    }

    /// Start, continue and finish a column resize.
    ///
    /// Split across the root element rather than the handle because the
    /// pointer leaves the handle immediately — a drag that only tracked while
    /// over its own 6-pixel strip would stick after the first few pixels.
    fn on_column_drag(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.column_drag else {
            return;
        };
        // The fixed columns sit to the right of the flexible name column, so
        // the handle on a column's left edge widens it when pulled left.
        let width = drag.width - (f32::from(event.position.x) - drag.origin);
        if width == self.settings.columns.width(drag.column) {
            return;
        }
        self.settings.columns.set_width(drag.column, width);
        cx.notify();
    }

    fn end_column_drag(&mut self, cx: &mut Context<Self>) {
        if self.column_drag.take().is_none() {
            return;
        }
        // Written once, at the end. Saving on every mouse move would put a
        // file write in the middle of a drag.
        self.persist_settings();
        cx.notify();
    }

    /// The query row, shown while a search is active.
    ///
    /// Carries the status too — how many matched, how much they hold, or what
    /// is wrong with the query. A search box that gives no feedback leaves the
    /// user unable to tell an empty result from a mistyped one.
    fn render_query_bar(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.state.ready().map(|ready| ready.search.clone());
        let creation_unavailable = self.search_bar.read(cx).creation_unavailable;

        v_flex().flex_none().child(self.search_bar.clone()).child(
            h_flex()
                .h(px(32.0))
                .px(px(12.0))
                .gap(px(8.0))
                .items_center()
                .bg(palette.surface)
                .border_b_1()
                .border_color(palette.hairline)
                .children(self.render_save_star(palette, cx))
                .children(self.render_resume_chip(palette, cx))
                .children(status.map(|search| {
                    if creation_unavailable && matches!(search, Search::Results { matched: 0, .. })
                    {
                        div()
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(palette.caution)
                            .child("Creation-date results unavailable")
                            .into_any_element()
                    } else {
                        self.render_search_status(palette, &search)
                    }
                }))
                .children(
                    self.state
                        .ready()
                        .filter(|ready| {
                            ready.showing_results()
                                && !matches!(ready.search, Search::Pending | Search::Invalid { .. })
                        })
                        .map(|_| {
                            h_flex()
                                .gap(px(4.0))
                                .items_center()
                                .child(
                                    div()
                                        .text_size(px(metrics::TEXT_XS))
                                        .text_color(palette.text_dim)
                                        .child("Export all:"),
                                )
                                .children(
                                    [
                                        kosha_core::export::Format::Txt,
                                        kosha_core::export::Format::Csv,
                                        kosha_core::export::Format::Jsonl,
                                    ]
                                    .into_iter()
                                    .map(|format| {
                                        let label = format.extension().to_uppercase();
                                        crate::ui::chip(match format {
                                            kosha_core::export::Format::Txt => "search-export-txt",
                                            kosha_core::export::Format::Csv => "search-export-csv",
                                            _ => "search-export-jsonl",
                                        })
                                        .role(gpui_kit::Role::Button)
                                        .aria_label(SharedString::from(format!(
                                            "Export all search results as {label}"
                                        )))
                                        .px(px(6.0))
                                        .text_color(palette.text_dim)
                                        .hover(|this| {
                                            this.bg(palette.hover).text_color(palette.text)
                                        })
                                        .child(SharedString::from(label))
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.export(format, window, cx)
                                            }),
                                        )
                                    }),
                                )
                        }),
                )
                .child(
                    crate::ui::glyph(
                        "query-help",
                        palette,
                        // The shortcut from the same constant the palette uses, so
                        // the two cannot come to disagree about the key.
                        SharedString::from(format!(
                            "What you can search for  ({}/)",
                            commands::MOD
                        )),
                    )
                    .hover(|this| this.bg(palette.hover).text_color(palette.accent))
                    .child("?")
                    .on_click(cx.listener(|this, _, window, cx| this.open_help(window, cx))),
                )
                .children(
                    self.state
                        .ready()
                        .is_some_and(|r| r.duplicates.is_some())
                        .then(|| {
                            crate::ui::close("filter-close", palette, "Clear the search  (Esc)")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_filter(window, cx)
                                    }),
                                )
                        }),
                ),
        )
    }

    /// The star that keeps the running query, filled once it is kept.
    ///
    /// One click, no naming dialog. A dialog between "that was useful" and
    /// keeping it is how a feature ends up never used, and the query is
    /// already the most accurate description of itself; the settings file has
    /// a separate name field for anybody who wants a better one.
    fn render_save_star(&self, palette: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let query = self.saveable_query()?;
        let saved = self.settings.has_saved_query(&query);

        Some(
            crate::ui::glyph(
                "save-query",
                palette,
                if saved {
                    "Forget this saved search"
                } else {
                    "Save this search, so it is in the command palette"
                },
            )
            // Amber is what says this search is *kept*. The hover must not
            // borrow it: an unsaved star that turns amber under the pointer is
            // telling you the thing you are about to do has already happened.
            .when(saved, |this| this.text_color(palette.caution))
            .hover(|this| this.bg(palette.hover))
            .child(if saved { "\u{2605}" } else { "\u{2606}" })
            .on_click(cx.listener(move |this, _, window, cx| {
                let Some(query) = this.saveable_query() else {
                    return;
                };
                if this.settings.has_saved_query(&query) {
                    this.forget_query(&query, window, cx);
                } else {
                    this.save_query(&query, window, cx);
                }
            }))
            .into_any_element(),
        )
    }

    /// "‹ size > 1gb", offered after navigating out of a set of results.
    fn render_resume_chip(&self, palette: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let previous = self.state.ready()?.previous_search.clone()?;
        let label = SharedString::from(format!("\u{2039} {previous}"));

        let hint = SharedString::from(format!("Go back to {previous}"));

        Some(
            // Two pixels of padding above and below made this eighteen pixels
            // tall, which is two thirds of the hit target the design doc
            // promises — and it sits in a row with a `?` and a `✕` that are
            // both twenty-eight. A chip is the shape for it.
            crate::ui::chip("resume-search")
                .flex_none()
                .bg(palette.raised)
                .border_1()
                .border_color(palette.hairline)
                .text_color(palette.accent)
                .hover(|this| this.bg(palette.hover))
                .tooltip(move |window, cx| tooltip(hint.clone()).build(window, cx))
                .child(label)
                .on_click(cx.listener(|this, _, window, cx| {
                    let Some(previous) = this
                        .state
                        .ready_mut()
                        .and_then(crate::state::Ready::resume_search)
                    else {
                        return;
                    };
                    this.apply_query(previous, window, cx);
                }))
                .into_any_element(),
        )
    }

    fn render_search_status(&self, palette: &Palette, search: &Search) -> AnyElement {
        let small = |colour, text: String| {
            let hint = text.clone();
            div()
                .id("search-status")
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(metrics::TEXT_XS))
                .text_color(colour)
                .tooltip(move |window, cx| tooltip(hint.clone()).build(window, cx))
                .child(SharedString::from(text))
                .into_any_element()
        };

        // Checked before the match, not as an arm in it. The duplicate view
        // leaves `search` at `Browsing`, and `Browsing` is the first arm — so
        // as a guarded arm this never ran.
        if let Some(found) = self.state.ready().and_then(|r| r.duplicates.as_ref()) {
            return small(
                palette.accent,
                format!(
                    "{} in {} · {} could go, freeing {}",
                    format::plural(found.rows.len(), "copy"),
                    format::plural(found.groups, "set"),
                    format::count(found.redundant.len() as u64),
                    self.bytes(found.reclaimable),
                ),
            );
        }

        // Before the arms, because it explains a *result* rather than replacing
        // one: a query about owners run against a scan that did not record any
        // matches nothing, and "0 matched" reads the same as "nobody matched".
        // The scan is the answer here, not the query.
        if self.query_needs_missing_owners() {
            return small(
                palette.caution,
                "this scan did not record who owns anything — turn on \
                 \"Record who owns each file\" in Settings and scan again"
                    .into(),
            );
        }

        match search {
            Search::Browsing => small(palette.text_faint, "Search the recorded scan".into()),
            Search::Pending => small(palette.text_faint, "Searching\u{2026}".into()),
            Search::Filtering { hidden } if *hidden > 0 => small(
                palette.text_faint,
                format!(
                    "{} shown, {} hidden",
                    format::count(self.rows_shown() as u64),
                    format::count(*hidden as u64),
                ),
            ),
            Search::Filtering { .. } => small(
                palette.text_faint,
                format!("{} shown", format::count(self.rows_shown() as u64)),
            ),
            Search::Results {
                matched,
                bytes,
                capped,
            } => small(
                palette.accent,
                format!(
                    "{} matched · {}{}",
                    format::count(*matched as u64),
                    self.bytes(*bytes),
                    if *capped {
                        format!(
                            " · showing the first {} in this order",
                            format::count(self.rows_shown() as u64)
                        )
                    } else {
                        String::new()
                    },
                ),
            ),
            // The error is the whole point of the row at this moment, so it
            // gets the space rather than being truncated beside a count.
            Search::Invalid { message } => small(palette.danger, message.clone()),
        }
    }

    fn rows_shown(&self) -> usize {
        self.state.ready().map_or(0, |ready| ready.rows.len())
    }

    /// Whether the running query asks about owners this tree does not have.
    ///
    /// Re-parsed rather than remembered, because it is asked once a frame for
    /// one status line and parsing a query is cheap by construction: it already
    /// happens on every keystroke, before running over the whole tree.
    fn query_needs_missing_owners(&self) -> bool {
        let Some(ready) = self.state.ready() else {
            return false;
        };
        if ready.tree.owners().were_collected() {
            return false;
        }
        let filter = ready.filter.trim();
        !filter.is_empty()
            && kosha_query::Query::parse(filter).is_ok_and(|query| query.needs_owners())
    }
}

/// The name column's heading. Flexible, unmovable, and never resized: it takes
/// whatever the fixed columns leave, which is what stops them adding up to more
/// than the window.
fn name_header(
    palette: &Palette,
    sort: crate::state::Sort,
    cx: &mut Context<KoshaApp>,
) -> impl IntoElement {
    heading(palette, sort, SortKey::Name, "NAME", cx).flex_1()
}

/// One clickable column heading, with its sort arrow when it is the active one
/// and a resize handle on its leading edge.
fn column_header(
    palette: &Palette,
    sort: crate::state::Sort,
    column: Column,
    width: f32,
    dragging: bool,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    h_flex()
        .h_full()
        .w(px(width))
        .items_center()
        // The handle overhangs into the gap on the left, so grabbing it does
        // not mean hitting the exact boundary pixel.
        .child(column_resize_handle(palette, column, dragging, cx))
        .child(
            heading(palette, sort, column.sort_key(), column.label(), cx)
                .flex_1()
                .justify_end(),
        )
        .into_any_element()
}

/// The grab strip between two columns.
///
/// Six pixels wide and drawn as nothing until it is hovered, because a visible
/// divider on every boundary is a lot of furniture for a table that mostly is
/// not being rearranged. The cursor changing is the affordance.
fn column_resize_handle(
    palette: &Palette,
    column: Column,
    dragging: bool,
    cx: &mut Context<KoshaApp>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("resize-{}", column.label())))
        .w(px(6.0))
        .h_full()
        .flex_none()
        .cursor(CursorStyle::ResizeLeftRight)
        .child(
            div()
                .w(px(1.0))
                .h_full()
                .when(dragging, |this| this.bg(palette.accent))
                .when(!dragging, |this| this.bg(palette.hairline.opacity(0.0))),
        )
        .hover(|this| this.bg(palette.hover))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.column_drag = Some(ColumnDrag {
                    column,
                    origin: f32::from(event.position.x),
                    width: this.settings.columns.width(column),
                });
                // Or the click falls through to the heading and sorts the
                // table every time somebody grabs an edge.
                cx.stop_propagation();
            }),
        )
        // A double-click on a divider restores that column's width. The
        // convention everywhere else, and the only way back from a width
        // dragged down to the minimum without opening settings.
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if event.click_count() >= 2 {
                this.settings
                    .columns
                    .set_width(column, column.default_width());
                this.persist_settings();
                cx.notify();
            }
        }))
}

/// The clickable label and sort arrow shared by every heading.
fn heading(
    palette: &Palette,
    sort: crate::state::Sort,
    key: SortKey,
    label: &'static str,
    cx: &mut Context<KoshaApp>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let indicator = sort.indicator(key);
    let active = indicator.is_some();

    h_flex()
        .id(label)
        .cursor_pointer()
        .gap(px(4.0))
        .items_center()
        .h_full()
        .when(active, |this| this.text_color(palette.text))
        .when(!active, |this| this.text_color(palette.text_faint))
        .hover(|this| this.text_color(palette.text))
        .child(label)
        .children(indicator.map(|arrow| div().child(arrow)))
        .on_click(cx.listener(move |this, _, _, cx| {
            let searching = this
                .state
                .ready()
                .is_some_and(|ready| !ready.filter.is_empty());
            if let Some(ready) = this.state.ready_mut() {
                ready.set_sort(key);
            }
            if searching {
                let text = this.filter.read(cx).value().to_string();
                this.query_text_changed(text, cx);
            }
            cx.notify();
        }))
}

/// One table row.
///
/// A free function rather than a method: `uniform_list` hands back an `App`,
/// not a `Context<Self>`, and this keeps the borrow of `ready` obvious.
#[allow(clippy::too_many_arguments)]
fn row(
    ready: &Ready,
    id: NodeId,
    index: usize,
    palette: &Palette,
    layout: &crate::columns::Layout,
    mono: SharedString,
    units: kosha_core::format::Units,
    show_path: bool,
    cx: &mut Context<KoshaApp>,
) -> gpui_kit::AnyElement {
    let node = ready.tree.node(id);
    let name = if show_path {
        // Relative to the scan root: the absolute prefix is the same on every
        // row and would push the interesting end off the edge.
        let full = ready.tree.path_of(id);
        format::printable(
            &full
                .strip_prefix(ready.tree.root_path())
                .unwrap_or(&full)
                .to_string_lossy(),
        )
        .into_owned()
    } else {
        format::printable(&ready.tree.name_lossy(id)).into_owned()
    };
    let selected = ready.selected == Some(id);
    let fraction = ready.tree.fraction_of_parent(id);
    let category = Category::dominant(&ready.tree, id);
    let now = format::now_unix();
    let in_results = ready.showing_results();

    h_flex()
        .id(("row", index))
        .h(metrics::row_height())
        // Without this the row shrinks to fit its contents, `flex_1` on the
        // name column has nothing to expand into, and every number sits a few
        // hundred pixels to the left of the heading it belongs under. The
        // header is a direct child of a stretching column and so was always
        // full width, which is what made the two disagree.
        .w_full()
        .px(px(12.0))
        .items_center()
        .text_size(px(metrics::TEXT))
        .when(selected, |this| this.bg(palette.selected))
        .when(!selected, |this| this.hover(|s| s.bg(palette.hover)))
        .child(
            h_flex()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(8.0))
                .items_center()
                // Normally an inline proportion bar, which reads faster than a
                // percentage and costs one element. In the duplicate view it
                // carries the grouping instead: rows are uniform height, so a
                // section header is not available and an alternating tint is.
                .child(div().w(px(3.0)).h(px(16.0)).rounded(px(2.0)).bg(
                    match ready.duplicates.as_ref() {
                        Some(found) => palette.group_tint(found.tint(id)),
                        None => palette.category[category as usize].opacity(
                            // Barely-visible slivers still carry their hue.
                            (0.25 + fraction * 0.75).clamp(0.25, 1.0),
                        ),
                    },
                ))
                .child(
                    div()
                        // A name longer than the column is clipped rather than
                        // drawn over the numbers to its right. It used to
                        // overflow: the cell shrinks, the text did not, and a
                        // deep path in a narrow window ran straight through
                        // the size column.
                        //
                        // From the middle, because both ends of a filename
                        // carry meaning — the extension most of all, in a tool
                        // whose whole subject is what kind of thing is taking
                        // up the disk.
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .text_color(if node.is_dir() {
                            palette.text
                        } else {
                            palette.text_dim
                        })
                        .child(SharedString::from(if node.is_dir() {
                            format!("{name}/")
                        } else {
                            name
                        })),
                )
                .when(node.is_partial(), |this| {
                    this.child(
                        div()
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(palette.caution)
                            .child("partial"),
                    )
                }),
        )
        // Built from the same layout the header is built from, so a cell can
        // no longer drift out from under its heading. It used to be four
        // hand-written widths here and four more there.
        .children(
            layout
                .visible()
                .map(|(column, width)| {
                    let cell = div().w(px(width)).text_right();
                    match column {
                        Column::Allocated => cell
                            .font_family(mono.clone())
                            .text_color(palette.text)
                            .child(SharedString::from(format::bytes_in(node.allocated, units))),
                        Column::Logical => cell
                            .font_family(mono.clone())
                            .text_color(palette.text_faint)
                            .child(SharedString::from(format::bytes_in(node.logical, units))),
                        Column::Items => cell
                            .font_family(mono.clone())
                            .text_color(palette.text_dim)
                            .child(SharedString::from(if node.is_dir() {
                                format::count(u64::from(node.subtree_files))
                            } else {
                                "—".into()
                            })),
                        Column::Modified => cell
                            .text_color(palette.text_faint)
                            .text_size(px(metrics::TEXT_SM))
                            .child(SharedString::from(format::age(node.mtime, now))),
                    }
                })
                // Collected because each arm captures `column`, which cannot be
                // captured by the `FnMut` an iterator adaptor would need.
                .collect::<Vec<_>>(),
        )
        // One click selects, two open — the convention everywhere else, and
        // previously the rows had an id and no handler at all, so clicking a
        // row did nothing.
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            // `ClickEvent` covers keyboard activation too, and its accessor
            // reports one click for that — which is the right reading.
            let opening = event.click_count() >= 2;
            if let Some(ready) = this.state.ready_mut() {
                ready.selected = Some(id);
                if opening {
                    if in_results {
                        ready.reveal(id);
                    } else {
                        ready.descend();
                    }
                }
            }
            if opening {
                this.sync_query_input(window, cx);
            }
            cx.notify();
        }))
        .into_any_element()
}

// ------------------------------------------------------------------ insights

impl KoshaApp {
    fn render_insights(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        // Quick look takes the top of this rail rather than floating over the
        // table. The first version was an overlay anchored above the rows, and
        // a screenshot settled it: it covered the names of every row below the
        // selected one, so arrowing down meant watching a panel describe
        // entries you could no longer see. This rail is already the "about
        // what you are looking at" column.
        let details = self
            .details_open
            .then(|| self.render_details(palette, cx))
            .flatten();

        let panel = v_flex()
            .w(px(metrics::INSIGHTS_WIDTH))
            .flex_none()
            .h_full()
            .bg(palette.surface)
            .border_l_1()
            .border_color(palette.hairline)
            .p(px(12.0))
            .gap(px(8.0))
            .children(details)
            .child(crate::ui::section_label(palette, "INSIGHTS"))
            .child(self.render_duplicates_button(palette, cx))
            .children(self.render_snapshot_button(palette, cx));

        let Some(ready) = self.state.ready() else {
            return panel.child(crate::ui::note(
                palette,
                // Mid-scan there is nothing to show *yet*, which is a
                // different sentence from having nothing to show because
                // nobody has scanned anything — and telling somebody to scan
                // a location while one is running reads as the scan being
                // ignored.
                if self.is_scanning() {
                    "Findings appear when the scan finishes."
                } else {
                    "Scan a location to see what can be reclaimed."
                },
            ));
        };

        let Some(insights) = ready.insights.clone() else {
            return panel.child(crate::ui::note(palette, "Analysing…"));
        };

        if insights.is_empty() {
            // "Nothing reclaimable" directly under a button that has just found
            // gigabytes of duplicates is a flat contradiction. The rules and the
            // duplicate hunt are separate questions, and the panel should say so
            // rather than speak for both.
            return panel.child(crate::ui::note(
                palette,
                if ready.duplicates.is_some() {
                    "No rules matched — the duplicate sets are in the table."
                } else {
                    "Nothing obviously reclaimable here."
                },
            ));
        }

        let total = kosha_analyze::total_reclaimable(&insights);

        // Built in a loop rather than a closure: `cx` is a unique borrow and
        // cannot be captured by the `FnMut` that `map` would need.
        let mut cards = Vec::with_capacity(insights.len().min(24));
        for (index, insight) in insights.iter().take(24).enumerate() {
            cards.push(
                self.render_insight_card(palette, insight, index, cx)
                    .into_any_element(),
            );
        }
        panel
            .when(total > 0, |this| {
                this.child(
                    v_flex()
                        .gap(px(2.0))
                        .pb(px(6.0))
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_XL))
                                .text_color(palette.positive)
                                .child(SharedString::from(self.bytes(total))),
                        )
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(palette.text_faint)
                                .child("reclaimable with confidence"),
                        ),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(
                        div()
                            .id("insight-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.insights_scroll)
                            .pr(px(12.0))
                            .child(v_flex().gap(px(8.0)).children(cards)),
                    )
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::vertical(&self.insights_scroll)
                                .id("insight-scrollbar")
                                .viewport_from_layout()
                                .mode(ScrollbarMode::Always),
                        ),
                    ),
            )
    }

    /// Keep this scan — or, while looking at a kept one, leave the past.
    ///
    /// Beside the duplicate hunt because they are the same kind of offer —
    /// things to do with the tree you have — and because this is where
    /// somebody is already looking when a scan has just finished, which is the
    /// one moment taking a snapshot is obviously worth doing.
    ///
    /// A fresh scan can be kept; an automatic record can be promoted with its
    /// original date. An explicit snapshot is already kept, so its offer is
    /// to scan again. Automatic views also retain Scan again in the title bar.
    fn render_snapshot_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ready = self.state.ready()?;
        let source = ready.source.snapshot();
        let original_date = source
            .filter(|one| one.automatic)
            .map(|one| crate::snapshots::exactly(&one.meta));
        let past = source
            .filter(|one| !one.automatic)
            .map(|one| one.meta.root.clone());

        let (id, text, hint) = match &past {
            Some(_) => (
                "leave-snapshot",
                SharedString::from("Scan it as it is now"),
                SharedString::from(
                    "Leave the snapshot and scan the folder it describes, as it is today",
                ),
            ),
            None if original_date.is_some() => (
                "keep-saved-scan",
                SharedString::from("Keep saved scan as snapshot"),
                SharedString::from(
                    "Keep this historical drive scan in the snapshot list with its original date; no rescan is needed",
                ),
            ),
            None => (
                "take-snapshot",
                SharedString::from("Keep a snapshot"),
                SharedString::from(
                    "Write this scan down, so a later one can say what changed since today",
                ),
            ),
        };

        Some(
            v_flex()
                .child(
                    crate::ui::button(id, palette)
                        .mb(px(4.0))
                        .tooltip(move |window, cx| tooltip(hint.clone()).build(window, cx))
                        .child(text)
                        .on_click(cx.listener(move |this, _, _, cx| match past.clone() {
                            Some(root) => this.scan(root, cx),
                            None => this.take_snapshot(cx),
                        })),
                )
                .children(original_date.map(|date| {
                    crate::ui::note(
                        palette,
                        SharedString::from(format!("From {date} · original scan date kept")),
                    )
                }))
                .into_any_element(),
        )
    }

    fn render_duplicates_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ready = self.state.ready();
        let hunt = ready.and_then(|ready| ready.hunt.as_ref());
        let running = hunt.is_some();
        let showing = ready.is_some_and(|ready| ready.duplicates.is_some());
        // Not over a snapshot: there are no file contents in one to compare.
        // Shown and dim rather than hidden, because a button that comes and
        // goes with what is on screen is one people stop believing is there.
        let past = ready.is_some_and(|ready| ready.source.is_past());
        let enabled = ready.is_some() && !past;

        // What the button says while it works. Files rather than bytes,
        // because bytes cannot be a denominator here — see
        // `duplicates::Progress`. Before the first update there is no fraction
        // and the label says what is happening instead of guessing at a
        // number: on a million-entry tree, grouping by size takes a moment and
        // "0%" would be a claim about reading that has not started.
        let progress = hunt.and_then(|hunt| hunt.progress);
        let fraction = progress.and_then(|p| p.fraction());
        let label: SharedString = match (running, showing, progress) {
            (true, _, Some(p)) if p.files_total > 0 => format!(
                "Reading {} of {} files — click to stop",
                format::count(p.files_done),
                format::count(p.files_total),
            )
            .into(),
            (true, _, _) => "Looking for candidates\u{2026}".into(),
            (false, true, _) => "Showing duplicates — click to stop".into(),
            (false, false, _) => "Find duplicate files".into(),
        };
        let hint: SharedString = if past {
            "A snapshot records sizes, not contents — scan the folder as it is now to look \
             for duplicates in it"
                .into()
        } else {
            "Read every file and group the ones whose contents are identical".into()
        };

        crate::ui::button("find-duplicates", palette)
            .mb(px(4.0))
            // Showing results is a state the button is *in*, so it wears the
            // accent rather than the neutral fill. Not `ui::primary`: that one
            // is filled, and a filled control the width of the rail would
            // outshout the insight cards it sits above.
            .when(showing, |this| {
                this.bg(palette.accent.opacity(0.18))
                    .border_color(palette.accent)
                    .text_color(palette.accent)
            })
            .when(!enabled, |this| this.text_color(palette.text_faint))
            // The bar is the button. A separate one under it would be a second
            // element saying the same thing, and this control is already the
            // width of the rail — the thing a progress bar wants to be.
            .when(running, |this| {
                this.relative()
                    .border_color(palette.accent)
                    .text_color(palette.text)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .bg(palette.accent.opacity(0.22))
                            .when_some(fraction, |this, done| this.w(relative(done)))
                            // No fraction yet: a full-width wash rather than an
                            // empty groove, so the control reads as busy rather
                            // than as stalled at nothing.
                            .when(fraction.is_none(), |this| {
                                this.w_full().bg(palette.accent.opacity(0.10))
                            }),
                    )
            })
            .tooltip(move |window, cx| tooltip(hint.clone()).build(window, cx))
            // Over the bar, which is painted absolutely underneath it.
            .child(div().relative().child(label))
            .on_click(cx.listener(|this, _, _, cx| {
                // Running first: while a hunt is in flight this control is a
                // stop button, and pressing it must not start a second one.
                if this.state.ready().is_some_and(Ready::is_hunting) {
                    if let Some(ready) = this.state.ready() {
                        ready.stop_hunt();
                    }
                    this.state.notice = Some("Stopping the duplicate hunt\u{2026}".into());
                    cx.notify();
                    return;
                }
                let showing = this
                    .state
                    .ready()
                    .is_some_and(|ready| ready.duplicates.is_some());
                if showing {
                    if let Some(ready) = this.state.ready_mut() {
                        ready.clear_duplicates();
                    }
                    cx.notify();
                } else {
                    this.find_duplicates(cx);
                }
            }))
    }

    fn render_insight_card(
        &self,
        palette: &Palette,
        insight: &Insight,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let accent = match insight.confidence {
            Confidence::Certain => palette.positive,
            Confidence::Likely => palette.accent,
            Confidence::Possible => palette.text_faint,
        };
        let destructive = insight.action.is_destructive();
        let actionable = insight.action.is_actionable();
        let read_write = self.state.mode().is_read_write();
        let jump_to = insight.nodes.first().copied();

        v_flex()
            .p(px(10.0))
            .gap(px(6.0))
            .rounded(px(metrics::RADIUS))
            .bg(palette.raised)
            .border_1()
            .border_color(palette.hairline)
            .child(
                h_flex()
                    .gap(px(6.0))
                    .items_center()
                    .child(div().w(px(3.0)).h(px(12.0)).rounded(px(2.0)).bg(accent))
                    .child(
                        div()
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(accent)
                            .child(match insight.confidence {
                                Confidence::Certain => "CERTAIN",
                                Confidence::Likely => "LIKELY",
                                Confidence::Possible => "POSSIBLE",
                            }),
                    )
                    .when(insight.reclaimable > 0, |this| {
                        this.child(
                            div()
                                .ml_auto()
                                .text_size(px(metrics::TEXT_SM))
                                .text_color(palette.text)
                                .child(SharedString::from(self.bytes(insight.reclaimable))),
                        )
                    }),
            )
            .child(
                div()
                    .text_color(palette.text)
                    .child(SharedString::from(insight.title.clone())),
            )
            // Counts live here rather than in the rule's prose, because the
            // engine trims overlapping findings after the wording is written.
            // Rendering from the live node list keeps the number true.
            .child(
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.text_faint)
                    .child(SharedString::from(if insight.superseded > 0 {
                        format!(
                            "{} · {} counted in a finding above",
                            format::plural(insight.len(), &insight.subject),
                            format::count(insight.superseded as u64),
                        )
                    } else {
                        format::plural(insight.len(), &insight.subject)
                    })),
            )
            .child(
                div()
                    .text_size(px(metrics::TEXT_SM))
                    .text_color(palette.text_dim)
                    .child(SharedString::from(insight.detail.clone())),
            )
            .child(
                h_flex()
                    .gap(px(6.0))
                    .child(
                        crate::ui::button(("reveal", index), palette)
                            .child("Show me")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(target) = jump_to {
                                    if let Some(ready) = this.state.ready_mut() {
                                        ready.reveal(target);
                                    }
                                    this.sync_query_input(window, cx);
                                }
                                cx.notify();
                            })),
                    )
                    // In read-only mode the action is present but visibly
                    // inert, so the user can see what read-write mode would
                    // offer without being able to trigger it.
                    //
                    // Gated on "does this act", not "is it destructive":
                    // compressing changes the filesystem without removing
                    // anything, so it earns a button — and, below, not the red
                    // one, because colouring it like a deletion would spend the
                    // warning people need for the actions that lose things.
                    .when(actionable, |this| {
                        this.child(
                            // `chip` rather than `button`: this one carries no
                            // hover, because in read-only mode it is present
                            // and inert, and a highlight that follows the
                            // pointer promises a click that does nothing.
                            crate::ui::chip(("act", index))
                                .border_1()
                                .when(read_write && destructive, |this| {
                                    this.bg(palette.danger.opacity(0.16))
                                        .border_color(palette.danger)
                                        .text_color(palette.danger)
                                })
                                .when(read_write && !destructive, |this| {
                                    this.bg(palette.accent.opacity(0.16))
                                        .border_color(palette.accent)
                                        .text_color(palette.accent)
                                })
                                .when(!read_write, |this| {
                                    this.bg(palette.surface)
                                        .border_color(palette.hairline)
                                        .text_color(palette.text_faint)
                                        .opacity(0.6)
                                })
                                .child(insight.action.label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.begin_action(index, cx);
                                })),
                        )
                    }),
            )
    }
}

// ---------------------------------------------------------------- status bar

impl KoshaApp {
    /// What a wider window would show, for the corner of the status bar.
    ///
    /// The insights rail is the one thing narrowing genuinely costs. The
    /// table is still the table and the places are still in the palette, but
    /// the advice — the part of this application that is not just a measuring
    /// tape — is simply gone, and "insights hidden" does not say how much is
    /// being missed. Naming the number turns a silent loss into a known one,
    /// and lets somebody decide whether it is worth widening for.
    ///
    /// Falls back to the plain wording whenever there is no number yet: before
    /// a scan, while one is running, and when the analysis genuinely found
    /// nothing. Announcing "0 insights" would be worse than saying nothing —
    /// it reads as a fault rather than as a clean bill of health.
    fn hidden_rail_hint(&self, rails: metrics::Rails) -> SharedString {
        let plain = if rails.places {
            "narrow window · insights hidden"
        } else {
            "narrow window · rails hidden"
        };

        let Some(insights) = self
            .state
            .ready()
            .and_then(|ready| ready.insights.as_ref())
            .filter(|insights| !insights.is_empty())
        else {
            return SharedString::from(plain);
        };

        let total = kosha_analyze::total_reclaimable(insights);
        let count = insights.len();
        let noun = if count == 1 { "insight" } else { "insights" };
        SharedString::from(if total > 0 {
            format!(
                "{count} {noun} · {} to reclaim — widen to see",
                self.bytes(total)
            )
        } else {
            // Findings worth showing that reclaim nothing by themselves: deep
            // nesting, a dominant child. A size would be a lie about them.
            format!("{count} {noun} — widen to see")
        })
    }

    fn render_status_bar(
        &self,
        rails: metrics::Rails,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text: SharedString = match &self.state.phase {
            Phase::Idle => "Ready".into(),
            Phase::Failed { .. } => "Scan failed".into(),
            Phase::Scanning { path, progress, .. } => match progress {
                // Never a spinner without a number.
                Some(p) => format!(
                    "Scanning {} · {} · {} queued",
                    path.display(),
                    format::plural(p.entries as usize, "entry"),
                    format::plural(p.dirs_pending as usize, "directory"),
                )
                .into(),
                None => format!("Scanning {}…", path.display()).into(),
            },
            // A snapshot's status line is about the snapshot, not about a scan
            // that is not happening. The numbers below would all be true and
            // all be read as a description of the disk right now.
            Phase::Ready(ready) if ready.source.is_past() => {
                let stats = ready.tree.stats();
                let approx = if ready.tree.is_partial() { "~" } else { "" };
                let taken = ready
                    .source
                    .snapshot()
                    .map(|one| crate::snapshots::when(&one.meta))
                    .unwrap_or_default();
                format!(
                    "{}Snapshot taken {taken} · {} · {approx}{} on disk then · \
                     not the disk as it is now",
                    if ready.tree.is_incomplete() {
                        "Incomplete scan · "
                    } else {
                        ""
                    },
                    format::plural(stats.entries as usize, "entry"),
                    self.bytes(ready.tree.total_allocated()),
                )
                .into()
            }
            Phase::Ready(ready) => {
                let stats = ready.tree.stats();
                let approx = if ready.tree.is_partial() { "~" } else { "" };
                format!(
                    "{}{} in {} · {approx}{} on disk · {} in memory{}{}",
                    if ready.tree.is_incomplete() {
                        "Incomplete scan · "
                    } else {
                        ""
                    },
                    format::plural(stats.entries as usize, "entry"),
                    format::duration(stats.duration),
                    self.bytes(ready.tree.total_allocated()),
                    self.bytes(ready.tree.memory_bytes() as u64),
                    // Not here any more when something was unreadable: that
                    // one is a link, below, because `ui.md` promises the
                    // status bar "links to the skipped list" and this was a
                    // count with nowhere to go.
                    if stats.unreadable > 0 {
                        String::new()
                    } else if stats.depth_limited {
                        // The other reason the `~` above is there. A depth-limited
                        // total is a floor, and a tilde with nothing to explain it
                        // is a tilde nobody reads.
                        format!(" · stopped at depth {}", stats.max_depth)
                    } else {
                        String::new()
                    },
                    // Said only when it is news. An elevated scan sees files an
                    // ordinary one cannot, so two scans of the same folder can
                    // legitimately differ and the window has to say which kind
                    // it is showing. The ordinary walk is what everybody gets,
                    // and repeating that on every scan is how the line that
                    // matters would stop being read.
                    match (stats.strategy, stats.declined) {
                        (kosha_core::Strategy::DirectoryWalk, None) => String::new(),
                        // **The core's line, which is the command line's line.**
                        // Building it here instead left out *what* was read out
                        // of an image — "disk image" does not say which
                        // filesystem, and on a dual-boot disk it does not say
                        // whether partition 2 of 3 is the Windows one or the
                        // Linux one — and lowercased the strategy by rule, which
                        // turned `Master File Table` into `master file table`.
                        // `Strategy::in_a_sentence` exists because that is a
                        // mistake somebody already made once, on the other front
                        // end, and wrote a method to stop.
                        (_, None) => format!(" · {}", stats.provenance_in_a_sentence()),
                        // Its own wording, and deliberately: the reason a faster
                        // strategy was declined is a parenthetical rather than
                        // another fact, so it reads with a comma where the rest
                        // of the line uses a separator.
                        (_, Some(reason)) => format!(" · directory walk, {}", reason.short()),
                    },
                )
                .into()
            }
        };

        h_flex()
            .h(px(metrics::STATUS_BAR_HEIGHT))
            .flex_none()
            .px(px(12.0))
            .gap(px(12.0))
            .items_center()
            .bg(palette.surface)
            .border_t_1()
            .border_color(palette.hairline)
            .text_size(px(metrics::TEXT_XS))
            .text_color(palette.text_faint)
            // The one part of this bar that may shrink. The legend is a key
            // and a half-drawn key is useless; the narrow-window hint is an
            // explanation and half an explanation reads as a rendering fault.
            // The summary ends with the least load-bearing thing it says, so
            // it is the one that gives way — and it ellipsises rather than
            // being clipped, so it is visibly abbreviated rather than broken.
            .child(div().min_w(px(0.0)).truncate().child(text))
            // **The skipped list, which was a number and nowhere to go.**
            // `ui.md` has promised since the beginning that "the status bar
            // links to the skipped list", and the query language has had an
            // `unreadable` flag for as long — so the link is the query, and
            // clicking it lists exactly the entries the `~` on the total is
            // about. Nothing new to maintain: the same box, the same results
            // table, and the text stays in the box afterwards so it is
            // obvious what is being shown and how to undo it.
            .when_some(self.unreadable_count(), |this, count| {
                this.child(
                    div()
                        .id("unreadable-link")
                        .flex_none()
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .text_color(palette.caution)
                        .hover(|this| this.text_color(palette.text))
                        .child(SharedString::from(format!(
                            "{} unreadable",
                            format::count(count)
                        )))
                        .tooltip(move |window, cx| {
                            tooltip("Show what could not be read").build(window, cx)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.apply_query("unreadable".into(), window, cx);
                        })),
                )
            })
            // Only beside a treemap that is showing something to key.
            .when(
                self.state
                    .ready()
                    .is_some_and(|ready| ready.comparison.is_none())
                    && (self.settings.show_treemap || !self.settings.show_table),
                |this| {
                    this.child(div().flex_none().text_color(palette.hairline).child("│"))
                        .child(legend(palette))
                },
            )
            .when_some(self.state.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .ml_auto()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_color(palette.text_dim)
                        .child(SharedString::from(notice)),
                )
            })
            // A rail that vanishes without explanation reads as a bug. This
            // says what a wider window would get back, in the place already
            // used for telling the user what the window is doing.
            .when(rails.anything_hidden(), |this| {
                this.child(
                    div()
                        .when(self.state.notice.is_none(), |this| this.ml_auto())
                        .flex_none()
                        .whitespace_nowrap()
                        .text_color(palette.text_faint)
                        .child(self.hidden_rail_hint(rails)),
                )
            })
    }
}

/// Show a file of ours in the platform's own file manager.
///
/// Selected in a folder window rather than opened: the audit log is `.jsonl`,
/// which a fresh Windows has no association for, and "how do you want to open
/// this file?" is not the answer to "what did kosha do". A folder with the
/// file highlighted is the same gesture every application uses for "here it
/// is", and it cannot fail for want of an editor.
fn reveal(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // `/select,` wants the path attached to it, and Explorer is
        // particular about the comma: one argument, no space after it, which
        // is what `raw_arg` is for — the quoting std applies for an ordinary
        // argument puts the comma inside the quotes and Explorer opens the
        // user's documents folder instead.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No `-R` here: `xdg-open` on a directory opens the file manager at
        // it, which is the nearest thing every desktop agrees on.
        let directory = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(directory)
            .spawn()
            .map(|_| ())
    }
}

/// One line per file in the store that is not a snapshot this build can open.
///
/// The name and the reason, because the reasons differ and one of them is not
/// damage at all: a file somebody dropped in that directory was never a
/// snapshot, and saying so is different from saying one rotted. Worded here,
/// once, rather than in the rail, so the rail draws strings rather than
/// re-reading a directory every frame.
fn problem_lines(unreadable: &[kosha_core::snapshot::store::Unreadable]) -> Vec<SharedString> {
    unreadable
        .iter()
        .map(|one| {
            let name = one
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| one.path.to_string_lossy().into_owned());
            SharedString::from(format!("{} — {}", format::printable(&name), one.why))
        })
        .collect()
}

/// The colour the desktop publishes, if the settings want it.
///
/// Kept out of `Palette` so that the palette stays a pure function of its
/// inputs and can be tested without a registry or a D-Bus session. Windows
/// ships a light and a dark variant, so the current mode is part of the
/// question.
fn desktop_accent(settings: &crate::settings::Settings, cx: &App) -> Option<crate::accent::Rgb> {
    use crate::settings::AccentSource;

    match settings.accent {
        AccentSource::Kosha => None,
        AccentSource::System => crate::accent::system(cx.theme().mode.is_dark()),
    }
}

// ------------------------------------------------------------------- helpers

/// Make a control inside the title bar actually clickable.
///
/// `TitleBar` arms a window drag on any left press inside it, and completes the
/// drag on the next mouse move — which hands the mouse to the operating system,
/// so the button never sees its own release and the click never happens. A hand
/// always moves a pixel or two between pressing and releasing, so every control
/// in the bar was inert in practice while appearing to work under a synthetic
/// click that moved not at all.
///
/// Swallowing the press stops the drag arming for this element only; the rest
/// of the bar stays draggable. The component library does the same thing for
/// its own window controls.
fn clickable_in_title_bar<E: InteractiveElement>(element: E) -> E {
    element.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A key for the treemap's colours. Colour that encodes data needs one.
///
/// In the status bar rather than floating over the treemap. It used to sit in
/// the bottom-left corner on a translucent chip, which was tolerable while the
/// map was one level deep and a corner was usually the smallest tile. Nesting
/// filled that corner with the tiles most worth reading, and a key that hides
/// the data it is a key to is worse than no key.
fn legend(palette: &Palette) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap(px(10.0))
        .items_center()
        .text_size(px(metrics::TEXT_XS))
        .text_color(palette.text_faint)
        .children(Category::ALL.map(|category| {
            h_flex()
                .gap(px(4.0))
                .items_center()
                .child(
                    div()
                        .w(px(7.0))
                        .h(px(7.0))
                        .rounded(px(2.0))
                        .bg(palette.category[category as usize]),
                )
                .child(category.label())
        }))
}

/// Open the window and start the app.
/// What the command line asked the window to do before anybody touches it.
///
/// A struct rather than four positional arguments: they are all optional, they
/// are all about the first second of the window's life, and a call site with
/// `None, None, false` in it is one where a later addition goes in the wrong
/// slot without the compiler noticing.
/// Called only on the export worker. A displayed top-N list is never the
/// source of truth for a completed whole-tree query.
fn export_rows(
    tree: &SizeTree,
    query: Option<&kosha_query::Query>,
    displayed: Option<Vec<NodeId>>,
    now: u32,
) -> Option<Vec<NodeId>> {
    query.map_or(displayed, |query| {
        Some(query.run(tree, now, tree.len()).matches)
    })
}

#[cfg(test)]
mod search_export_tests {
    use super::*;

    #[test]
    fn exporting_a_capped_search_writes_all_matches_from_its_recorded_tree() {
        let dir = tempfile::tempdir().unwrap();
        for number in 0..2_005 {
            std::fs::write(dir.path().join(format!("entry-{number}.txt")), b"test").unwrap();
        }
        let tree = Scanner::new(Default::default()).scan(dir.path()).unwrap();
        let query = kosha_query::Query::parse("file and ext = txt").unwrap();
        let displayed = query.run(&tree, 1_000, 2_000).matches;
        assert_eq!(displayed.len(), 2_000);
        // Files disappear after scanning: export still uses the captured tree.
        std::fs::remove_file(dir.path().join("entry-0.txt")).unwrap();
        let rows = export_rows(&tree, Some(&query), Some(displayed), 1_000).unwrap();
        assert_eq!(rows.len(), 2_005);
        for format in [
            kosha_core::export::Format::Txt,
            kosha_core::export::Format::Csv,
            kosha_core::export::Format::Jsonl,
        ] {
            let mut output = Vec::new();
            let written = kosha_core::export::write_rows(
                &tree,
                &rows,
                &kosha_core::export::Options {
                    format,
                    absolute_paths: true,
                    ..Default::default()
                },
                &mut output,
            )
            .unwrap();
            assert_eq!(written.rows, 2_005);
            let text = String::from_utf8(output).unwrap();
            assert_eq!(
                text.lines().count(),
                if format == kosha_core::export::Format::Csv {
                    2_006
                } else {
                    2_005
                }
            );
        }
        let subset = vec![rows[0]];
        assert_eq!(
            export_rows(&tree, None, Some(subset.clone()), 1_000),
            Some(subset)
        );
        assert_eq!(export_rows(&tree, None, None, 1_000), None);
    }
}

#[derive(Default)]
pub struct Start {
    /// A directory to scan, or a snapshot to open.
    pub path: Option<PathBuf>,
    pub query: Option<String>,
    pub duplicates: bool,
    /// A snapshot to hold the first scan against.
    pub since: Option<PathBuf>,
}

pub fn open(
    start: Start,
    theme: Option<ThemeMode>,
    cx: &mut App,
) -> gpui_kit::Result<Entity<gpui_kit::component::Root>> {
    use gpui_kit::{Bounds, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, point, size};

    // The narrowest the window will go. Named here because two things need to
    // agree about it: the option below, and the check that refuses to restore
    // a saved size smaller than it.
    let minimum = gpui_kit::Size {
        width: 680.0_f32,
        height: 480.0_f32,
    };

    // Read once, here, because the geometry has to be known before there is a
    // window to ask — and then handed to the app rather than read again.
    let loaded = crate::settings::load();

    // Visible bounds rather than full bounds: a window positioned under the
    // taskbar is another way to be unreachable.
    let displays: Vec<Bounds<gpui_kit::Pixels>> = cx
        .displays()
        .iter()
        .map(|display| display.visible_bounds())
        .collect();

    let window_bounds = crate::geometry::restore(loaded.settings.window, &displays, minimum)
        // Centred at the shipping size: a first run, and the fallback for a
        // saved position that is no longer anywhere a window can go.
        .unwrap_or_else(|| {
            WindowBounds::Windowed(Bounds::centered(None, size(px(1280.0), px(820.0)), cx))
        });

    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        titlebar: Some(TitlebarOptions {
            title: Some("kosha".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.0), px(12.0))),
        }),
        kind: WindowKind::Normal,
        // Wayland has no window title to match on: the compositor pairs a
        // window with its `.desktop` file, and therefore its icon and its
        // taskbar identity, by `app_id`. Unset, kosha is an anonymous window
        // with a placeholder icon. The same string names the macOS bundle and
        // the directory kosha keeps its files in, which is why it comes from
        // `kosha_core::dirs` rather than being written out again here.
        app_id: Some(kosha_core::dirs::BUNDLE_ID.to_string()),
        // The rails collapse on the way down — insights first, then places —
        // so the table keeps its room instead of having the names drawn on top
        // of the numbers. See `metrics::rails_for`.
        //
        // What is left at this minimum is the table alone: the shipping
        // columns cost 392, the row padding 24, and the name column will not
        // go below 220. That is 636, and the title bar's breadcrumb and
        // buttons want a little more than the table does, which is where 680
        // comes from. The height is a dozen rows plus the three bars.
        window_min_size: Some(size(px(minimum.width), px(minimum.height))),
        ..Default::default()
    };

    cx.open_window(options, |window, cx| {
        let view: Entity<KoshaApp> = cx.new(|cx| {
            let path = start.path.clone();
            let mut app = KoshaApp::new(window, loaded, start, theme, cx);
            if let Some(path) = path {
                // A snapshot, a folder or an image. Told apart by what is in
                // the file, the same way the command line tells them apart —
                // `kosha <SNAPSHOT>` browses one, and the window handed the
                // same path used to try to scan it as a directory and report
                // that it was not one. The decision is `state::opening`, where
                // it can be tested; an image handed to the window used to end
                // at a refusal whose own advice is about a scan you start
                // yourself, not one that was started for you.
                match crate::state::opening(&path, app.state.scan_options) {
                    crate::state::Opening::Snapshot => app.open_snapshot(path, cx),
                    crate::state::Opening::Scan(options) => app.scan_with(path, options, cx),
                }
            }
            app
        });

        // Where the geometry is written down. Closing the window is the
        // ordinary way out; quitting from the palette is handled where that
        // command runs, because this does not fire for it on every platform.
        //
        // Always true: this is a place to take a note, never a veto. A window
        // that declines to close because a settings file could not be written
        // would be a worse bug than the one it was protecting against.
        let closing = view.clone();
        window.on_window_should_close(cx, move |window, cx| {
            closing.update(cx, |app, _| app.remember_geometry(window));
            true
        });

        cx.new(|cx| gpui_kit::component::Root::new(view, window, cx))
    })
    .map(|handle| handle.entity(cx).expect("window was just opened"))
}
