// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reusable search editor. The host supplies interpretation and execution
//! feedback; this component owns editing and emits intent, never scans a tree.

use gpui_kit::base::input::{InputEditorStyle, InputEvent, InputState};
use gpui_kit::base::{Input, h_flex, v_flex};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, EventEmitter, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    div, px,
};
use kosha_query::{Expr, Field, Op, Value};
use kosha_search::{SearchMode, Translation};

use crate::theme::{Palette, metrics};

const QUICK_EXAMPLE_COUNT: usize = 4;
const NATURAL_EXAMPLES: &[(&str, &str)] = &[
    ("large", "files greater than 10 MB"),
    ("markdown", "files ending with md"),
    ("source", "C source files"),
    ("created", "folders created last week"),
    ("images", "images over 5 MB"),
    ("exclude-pdf", "documents except pdf"),
    ("exact-name", "files named \"budget.xlsx\""),
    ("name-prefix", "files starting with \"report\""),
    ("empty-folders", "empty folders"),
    ("created-today", "folders created today"),
    ("created-recently", "files created in last 24 hours"),
    ("size-words", "files larger than ten megabytes"),
    ("folder-count", "folders with at least one hundred files"),
    ("month", "files modified in October 2026"),
    ("days-ago", "files created two days ago"),
    ("apparent-size", "files apparent size over 10 MB"),
    ("size-range", "files between 10 and 100 MB"),
    (
        "compact-allocation",
        "files apparent size over 10 MB and allocated size under 2 MB",
    ),
    ("type-choice", "pdf or xlsx files"),
    ("type-list", "files extensions md or txt"),
    ("spreadsheets", "excel sheets"),
    ("name-suffix", "files with names ending with '.tar.gz'"),
    ("name-contains", "files whose name contains 'invoice'"),
    ("path", "files in 'Downloads'"),
    ("path-exclusion", "files not in 'cache'"),
    ("modified-old", "files modified more than 30 days ago"),
    ("created-recent", "files created less than 7 days ago"),
    (
        "created-old-folders",
        "folders created at least two weeks ago",
    ),
    ("folder-children", "folders at most ten children"),
    ("empty-files", "empty files"),
];

const RECIPE_GROUPS: &[(&str, &[&str])] = &[
    (
        "Size and allocation",
        &[
            "large",
            "size-words",
            "size-range",
            "apparent-size",
            "compact-allocation",
        ],
    ),
    (
        "Types and exclusions",
        &[
            "markdown",
            "source",
            "images",
            "exclude-pdf",
            "type-choice",
            "type-list",
            "spreadsheets",
        ],
    ),
    (
        "Names and paths",
        &[
            "exact-name",
            "name-prefix",
            "name-suffix",
            "name-contains",
            "path",
            "path-exclusion",
        ],
    ),
    (
        "Created and modified",
        &[
            "created",
            "created-today",
            "created-recently",
            "month",
            "days-ago",
            "modified-old",
            "created-recent",
            "created-old-folders",
        ],
    ),
    (
        "Folder contents",
        &[
            "empty-folders",
            "folder-count",
            "folder-children",
            "empty-files",
        ],
    ),
];

pub enum SearchEvent {
    Changed(String),
    Refresh,
    Rescan,
}

pub struct SearchBar {
    input: Entity<InputState>,
    mode: SearchMode,
    natural: String,
    dsl: String,
    dsl_edited: bool,
    literal: String,
    translation: Option<Translation>,
    date_summaries: Vec<String>,
    examples_open: bool,
    examples_scroll: gpui_kit::ScrollHandle,
    custom_recipes: Vec<String>,
    recipe_pack: Option<kosha_search::RecipePackMetadata>,
    pub warning: Option<String>,
    pub creation_unavailable: bool,
    pub can_rescan_for_creation: bool,
}

impl EventEmitter<SearchEvent> for SearchBar {}

pub fn needs_creation(expression: &kosha_query::Expr) -> bool {
    use kosha_query::{Expr, Field};
    match expression {
        Expr::And(a, b) | Expr::Or(a, b) => needs_creation(a) || needs_creation(b),
        Expr::Not(inner) => needs_creation(inner),
        Expr::Compare { field, .. } | Expr::In { field, .. } => *field == Field::Created,
        _ => false,
    }
}

fn editable_dsl(draft: &str, edited: bool, translation: Option<&Translation>) -> String {
    if edited {
        return draft.to_owned();
    }
    translation
        .and_then(|translation| translation.dsl.clone())
        .unwrap_or_else(|| draft.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DateBound {
    seconds: u64,
    inclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DateRange {
    field: Field,
    lower: Option<DateBound>,
    upper: Option<DateBound>,
}

impl DateRange {
    fn constrain(&mut self, op: Op, seconds: u64) -> bool {
        let bound = DateBound {
            seconds,
            inclusive: matches!(op, Op::Ge | Op::Le | Op::Eq),
        };
        match op {
            Op::Ge | Op::Gt => {
                if self.lower.is_none_or(|old| {
                    seconds > old.seconds || (seconds == old.seconds && !bound.inclusive)
                }) {
                    self.lower = Some(bound);
                }
            }
            Op::Le | Op::Lt => {
                if self.upper.is_none_or(|old| {
                    seconds < old.seconds || (seconds == old.seconds && !bound.inclusive)
                }) {
                    self.upper = Some(bound);
                }
            }
            Op::Eq => return self.constrain(Op::Ge, seconds) && self.constrain(Op::Le, seconds),
            _ => return false,
        }
        if let (Some(lower), Some(upper)) = (self.lower, self.upper) {
            lower.seconds < upper.seconds
                || (lower.seconds == upper.seconds && lower.inclusive && upper.inclusive)
        } else {
            true
        }
    }

    fn describe(&self, timestamp: impl Fn(u64) -> String) -> String {
        let label = if self.field == Field::Created {
            "Created"
        } else {
            "Modified"
        };
        let condition = match (self.lower, self.upper) {
            (Some(lower), Some(upper)) if lower == upper => {
                format!("exactly {}", timestamp(lower.seconds))
            }
            (Some(lower), Some(upper)) => {
                let from = if lower.inclusive { "from" } else { "after" };
                let until = if upper.inclusive {
                    "through"
                } else {
                    "until (excluding)"
                };
                format!(
                    "{from} {} {until} {}",
                    timestamp(lower.seconds),
                    timestamp(upper.seconds)
                )
            }
            (Some(lower), None) => format!(
                "{} {}",
                if lower.inclusive {
                    "on or after"
                } else {
                    "after"
                },
                timestamp(lower.seconds)
            ),
            (None, Some(upper)) => format!(
                "{} {}",
                if upper.inclusive {
                    "on or before"
                } else {
                    "before"
                },
                timestamp(upper.seconds)
            ),
            (None, None) => return String::new(),
        };
        format!("{label}: {condition}")
    }
}

/// These are required date conditions, not a paraphrase of an entire query.
/// Branches and negation can put different dates on different kinds of entry;
/// omit that annotation rather than flattening their meaning into one range.
fn required_date_ranges(expression: &Expr) -> Vec<DateRange> {
    fn mentions_date(expression: &Expr) -> bool {
        match expression {
            Expr::And(a, b) | Expr::Or(a, b) => mentions_date(a) || mentions_date(b),
            Expr::Not(inner) => mentions_date(inner),
            Expr::Compare { field, .. } | Expr::In { field, .. } => {
                matches!(field, Field::Created | Field::Modified)
            }
            _ => false,
        }
    }
    fn collect(expression: &Expr, ranges: &mut Vec<DateRange>) -> bool {
        match expression {
            Expr::And(left, right) => collect(left, ranges) && collect(right, ranges),
            // An unrelated type alternative or exclusion does not weaken a
            // mandatory date condition outside it. Date-bearing branches are
            // still deliberately not collapsed into a single summary.
            Expr::Or(_, _) | Expr::Not(_) => !mentions_date(expression),
            Expr::Compare {
                field: field @ (Field::Created | Field::Modified),
                op,
                value: Value::Number(seconds),
            } => {
                let index = ranges
                    .iter()
                    .position(|range| range.field == *field)
                    .unwrap_or_else(|| {
                        ranges.push(DateRange {
                            field: *field,
                            lower: None,
                            upper: None,
                        });
                        ranges.len() - 1
                    });
                ranges[index].constrain(*op, *seconds)
            }
            Expr::Compare {
                field: Field::Created | Field::Modified,
                ..
            }
            | Expr::In {
                field: Field::Created | Field::Modified,
                ..
            } => false,
            _ => true,
        }
    }
    let mut ranges = Vec::with_capacity(2);
    if collect(expression, &mut ranges) {
        ranges
    } else {
        Vec::new()
    }
}

impl SearchBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search this scan: files greater than 10 MB")
        });
        cx.subscribe(&input, |this, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                if this.mode == SearchMode::Dsl && this.dsl != text {
                    this.dsl_edited = true;
                }
                if this.mode == SearchMode::Natural && this.natural != text {
                    this.dsl_edited = false;
                }
                *this.draft_mut() = text.clone();
                cx.emit(SearchEvent::Changed(text));
                cx.notify();
            }
        })
        .detach();
        Self {
            input,
            mode: SearchMode::Natural,
            natural: String::new(),
            dsl: String::new(),
            dsl_edited: false,
            literal: String::new(),
            translation: None,
            date_summaries: Vec::new(),
            examples_open: false,
            examples_scroll: gpui_kit::ScrollHandle::new(),
            custom_recipes: Vec::new(),
            recipe_pack: None,
            warning: None,
            creation_unavailable: false,
            can_rescan_for_creation: false,
        }
    }

    pub fn input(&self) -> Entity<InputState> {
        self.input.clone()
    }
    pub fn set_recipes(&mut self, book: &kosha_search::RecipeBook) {
        self.custom_recipes = book
            .recipes()
            .iter()
            .map(|recipe| recipe.phrase.clone())
            .collect();
        self.recipe_pack = book.metadata().cloned();
    }
    pub fn mode(&self) -> SearchMode {
        self.mode
    }
    fn draft_mut(&mut self) -> &mut String {
        match self.mode {
            SearchMode::Natural => &mut self.natural,
            SearchMode::Dsl => &mut self.dsl,
            SearchMode::Literal => &mut self.literal,
        }
    }

    pub fn interpret(&mut self, translation: Translation, cx: &mut Context<Self>) {
        self.date_summaries = translation
            .query
            .as_ref()
            .map(|query| {
                required_date_ranges(query.expression())
                    .iter()
                    .map(|range| {
                        range.describe(|seconds| {
                            kosha_core::format::local_timestamp(seconds).replacen('T', " ", 1)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.translation = Some(translation);
        self.warning = None;
        self.creation_unavailable = false;
        self.can_rescan_for_creation = false;
        cx.notify();
    }

    /// Programmatic saved/help queries are explicitly DSL; the natural draft
    /// survives toggling to inspect and edit its translation.
    pub fn set_dsl(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = SearchMode::Dsl;
        self.dsl = text.clone();
        self.dsl_edited = true;
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.natural.clear();
        self.dsl.clear();
        self.dsl_edited = false;
        self.literal.clear();
        self.translation = None;
        self.date_summaries.clear();
        self.warning = None;
        self.creation_unavailable = false;
        self.can_rescan_for_creation = false;
        self.examples_open = false;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn switch(&mut self, mode: SearchMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        if mode == SearchMode::Dsl && !self.dsl_edited {
            self.dsl = editable_dsl(&self.dsl, self.dsl_edited, self.translation.as_ref());
        }
        self.mode = mode;
        let text = self.draft_mut().clone();
        self.input
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.input.read(cx).focus_handle(cx).focus(window, cx);
        // Switching an unchanged input is still an interpretation change.
        cx.emit(SearchEvent::Changed(text));
        cx.notify();
    }

    fn example_chip(
        &self,
        (id, phrase): (&'static str, &'static str),
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::ui::chip(id)
            .role(gpui_kit::Role::Button)
            .aria_label(phrase)
            .px(px(7.0))
            .text_color(palette.accent)
            .bg(palette.raised)
            .hover(|this| this.bg(palette.hover))
            .child(phrase)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.mode = SearchMode::Natural;
                this.dsl_edited = false;
                this.natural = phrase.into();
                this.examples_open = false;
                this.input
                    .update(cx, |input, cx| input.set_value(phrase, window, cx));
                this.input.read(cx).focus_handle(cx).focus(window, cx);
                cx.emit(SearchEvent::Changed(phrase.into()));
                cx.notify();
            }))
            .into_any_element()
    }

    fn examples_toggle(
        &self,
        empty: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if self.examples_open {
            "Hide examples"
        } else if empty {
            "More examples"
        } else {
            "Examples"
        };
        crate::ui::chip("search-examples-toggle")
            .role(gpui_kit::Role::Button)
            .aria_label(label)
            .px(px(7.0))
            .text_color(palette.text_dim)
            .hover(|this| this.bg(palette.hover))
            .child(label)
            .on_click(cx.listener(|this, _, _, cx| {
                this.examples_open = !this.examples_open;
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for SearchBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        // The bare input owns no presentation. Project every theme change into
        // its placeholder, caret and selection as well as the inherited ink.
        self.input.update(cx, |input, _| {
            input.set_editor_style(InputEditorStyle {
                foreground: palette.text,
                muted_foreground: palette.text_dim,
                background: palette.canvas,
                border: palette.text_faint,
                caret: palette.text,
                selection: palette.accent.opacity(0.25),
                ..InputEditorStyle::default()
            })
        });
        let focus = self.input.read(cx).focus_handle(cx);
        let text = self.input.read(cx).value();
        let empty = text.trim().is_empty();
        let modes = [
            ("natural", "Natural", SearchMode::Natural),
            ("dsl", "DSL", SearchMode::Dsl),
            ("literal", "Name", SearchMode::Literal),
        ];
        let mut row = h_flex().gap(px(6.0)).items_center().child(
            h_flex()
                .id("scan-search-input")
                .flex_1()
                .min_w(px(0.0))
                .h(px(36.0))
                .px(px(10.0))
                .items_center()
                .rounded(px(6.0))
                .bg(palette.canvas)
                .border_1()
                .border_color(palette.text_faint)
                .track_focus(&focus)
                .focus(|this| this.border_color(palette.accent))
                .text_size(px(metrics::TEXT_SM))
                .text_color(palette.text)
                .child(Input::new(&self.input))
                .when(!empty, |field| {
                    field.child(
                        crate::ui::glyph("clear-search-input", &palette, "Clear search (Esc)")
                            .role(gpui_kit::Role::Button)
                            .aria_label("Clear search")
                            .child("×")
                            .hover(|this| this.bg(palette.hover).text_color(palette.text))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.clear(window, cx);
                                this.input.read(cx).focus_handle(cx).focus(window, cx);
                                cx.emit(SearchEvent::Changed(String::new()));
                            })),
                    )
                }),
        );
        for (id, label, mode) in modes {
            row = row.child(
                crate::ui::chip(id)
                    .role(gpui_kit::Role::Button)
                    .aria_label(label)
                    .flex_none()
                    .px(px(7.0))
                    .text_color(palette.text_dim)
                    .when(self.mode == mode, |this| {
                        this.bg(palette.selected).text_color(palette.accent)
                    })
                    .hover(|this| this.bg(palette.hover))
                    .child(label)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.switch(mode, window, cx)),
                    ),
            );
        }
        row = row.child(
            crate::ui::glyph(
                "search-refresh",
                &palette,
                "Refresh relative dates against the current local clock",
            )
            .child("↻")
            .hover(|this| this.bg(palette.hover))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SearchEvent::Refresh))),
        );

        let mut body = v_flex().min_w(px(0.0)).gap(px(5.0)).child(row)
            .child(div().text_size(px(metrics::TEXT_XS)).text_color(palette.text_faint)
                .child("Scope: whole scanned tree · Default size: on disk · local dates · scan metadata, no file contents"));
        if empty {
            let mut examples = h_flex().flex_wrap().gap(px(6.0));
            for &example in NATURAL_EXAMPLES.iter().take(QUICK_EXAMPLE_COUNT) {
                examples = examples.child(self.example_chip(example, &palette, cx));
            }
            body = body.child(examples.child(self.examples_toggle(empty, &palette, cx)));
        } else if let Some(translation) = &self.translation {
            if self.mode == SearchMode::Natural && translation.query.is_some() {
                for summary in &self.date_summaries {
                    body = body.child(
                        div()
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(palette.accent)
                            .whitespace_normal()
                            .child(SharedString::from(summary.clone())),
                    );
                }
            }
            if self.mode == SearchMode::Natural && !translation.entities.is_empty() {
                let mut entities = h_flex().flex_wrap().gap(px(6.0));
                for entity in &translation.entities {
                    if matches!(
                        entity.kind,
                        kosha_search::EntityKind::Dsl | kosha_search::EntityKind::Literal
                    ) {
                        continue;
                    }
                    if (entity.kind == kosha_search::EntityKind::Created
                        && self
                            .date_summaries
                            .iter()
                            .any(|s| s.starts_with("Created:")))
                        || (entity.kind == kosha_search::EntityKind::Modified
                            && self
                                .date_summaries
                                .iter()
                                .any(|s| s.starts_with("Modified:")))
                    {
                        continue;
                    }
                    entities = entities.child(
                        div()
                            .rounded(px(3.0))
                            .max_w(px(240.0))
                            .whitespace_normal()
                            .px(px(5.0))
                            .bg(palette.raised)
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(palette.text_dim)
                            .child(SharedString::from(format!(
                                "{:?}: {}",
                                entity.kind, entity.value
                            ))),
                    );
                }
                body = body.child(entities);
            }
            for diagnostic in &translation.diagnostics {
                let start = text
                    .char_indices()
                    .take_while(|(byte, _)| *byte < diagnostic.span.start)
                    .count()
                    + 1;
                let end = text
                    .char_indices()
                    .take_while(|(byte, _)| *byte < diagnostic.span.end)
                    .count()
                    .max(start);
                body = body.child(
                    div()
                        .text_size(px(metrics::TEXT_XS))
                        .text_color(palette.danger)
                        .whitespace_normal()
                        .child(SharedString::from(format!(
                            "{} {} (characters {}–{})",
                            diagnostic.message, diagnostic.hint, start, end
                        ))),
                );
            }
        }
        if let Some(warning) = &self.warning {
            body = body.child(
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(palette.caution)
                    .whitespace_normal()
                    .child(SharedString::from(warning.clone()))
                    .when(self.creation_unavailable, |note| {
                        note.mt(px(4.0))
                            .p(px(8.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(palette.caution)
                            .children(self.can_rescan_for_creation.then(|| {
                                crate::ui::chip("search-rescan")
                                    .role(gpui_kit::Role::Button)
                                    .aria_label("Scan again to collect creation dates")
                                    .mt(px(5.0))
                                    .px(px(8.0))
                                    .text_color(palette.text)
                                    .bg(palette.raised)
                                    .hover(|this| this.bg(palette.hover))
                                    .child("Scan again")
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(SearchEvent::Rescan)),
                                    )
                            }))
                    }),
            );
        }
        if !empty {
            body = body.child(self.examples_toggle(empty, &palette, cx));
        }
        if self.examples_open {
            // Leave room for the graph, search controls, and result rows in a
            // short window. The example list has its own scrollbar.
            let examples_height =
                px((f32::from(window.viewport_size().height) - 400.0).clamp(32.0, 160.0));
            let mut examples = v_flex().gap(px(8.0));
            if !self.custom_recipes.is_empty() {
                let mut chips = h_flex().flex_wrap().gap(px(6.0));
                for (index, phrase) in self.custom_recipes.iter().enumerate() {
                    let phrase = phrase.clone();
                    chips = chips.child(
                        crate::ui::chip(("custom-recipe", index))
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(phrase.clone()))
                            .px(px(6.0))
                            .max_w(px(320.0))
                            .whitespace_normal()
                            .text_color(palette.accent)
                            .bg(palette.raised)
                            .child(SharedString::from(phrase.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.mode = SearchMode::Natural;
                                this.dsl_edited = false;
                                this.natural = phrase.clone();
                                this.examples_open = false;
                                this.input.update(cx, |input, cx| {
                                    input.set_value(phrase.clone(), window, cx)
                                });
                                this.input.read(cx).focus_handle(cx).focus(window, cx);
                                cx.emit(SearchEvent::Changed(phrase.clone()));
                                cx.notify();
                            })),
                    );
                }
                examples = examples.child(
                    v_flex()
                        .flex_none()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(palette.text_dim)
                                .child(self.recipe_pack.as_ref().map_or_else(
                                    || SharedString::from("Your recipes"),
                                    |pack| SharedString::from(pack.title.clone()),
                                )),
                        )
                        .children(self.recipe_pack.as_ref().map(|pack| {
                            div()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(palette.text_dim)
                                .whitespace_normal()
                                .child(SharedString::from(pack.description.clone()))
                        }))
                        .child(chips),
                );
            }
            let skip = if empty { QUICK_EXAMPLE_COUNT } else { 0 };
            for &(label, ids) in RECIPE_GROUPS {
                let members: Vec<_> = NATURAL_EXAMPLES
                    .iter()
                    .skip(skip)
                    .filter(|(id, _)| ids.contains(id))
                    .copied()
                    .collect();
                if members.is_empty() {
                    continue;
                }
                let mut chips = h_flex().flex_wrap().gap(px(6.0));
                for example in members {
                    chips = chips.child(self.example_chip(example, &palette, cx));
                }
                examples = examples.child(
                    v_flex()
                        .flex_none()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(palette.text_dim)
                                .child(label),
                        )
                        .child(chips),
                );
            }
            for group in kosha_search::BUILTIN_RECIPE_GROUPS {
                examples = examples.child(
                    v_flex()
                        .flex_none()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(palette.text_dim)
                                .child(group.label),
                        )
                        .child(
                            h_flex().flex_wrap().gap(px(6.0)).children(
                                group
                                    .recipes
                                    .iter()
                                    .map(|&(phrase, _)| {
                                        self.example_chip((phrase, phrase), &palette, cx)
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                        ),
                );
            }
            body = body.child(
                div()
                    .relative()
                    .flex_none()
                    .min_h(px(0.0))
                    .max_h(examples_height)
                    .overflow_hidden()
                    .child(
                        div()
                            .id("search-example-list")
                            .max_h(examples_height)
                            .overflow_y_scroll()
                            .track_scroll(&self.examples_scroll)
                            .pr(px(12.0))
                            .child(examples),
                    )
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::vertical(&self.examples_scroll)
                                .id("search-example-scrollbar")
                                .viewport_from_layout()
                                .mode(ScrollbarMode::Always),
                        ),
                    ),
            );
        }
        body.px(px(12.0)).py(px(7.0)).bg(palette.surface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(dsl: &str) -> Vec<DateRange> {
        let query = kosha_query::Query::parse(dsl).unwrap();
        required_date_ranges(query.expression())
    }

    #[test]
    fn readable_dates_keep_exact_inclusive_and_exclusive_bounds() {
        let range = &ranges("file and created >= @100 and created < @200")[0];
        assert_eq!(
            range.lower,
            Some(DateBound {
                seconds: 100,
                inclusive: true
            })
        );
        assert_eq!(
            range.upper,
            Some(DateBound {
                seconds: 200,
                inclusive: false
            })
        );
        assert_eq!(
            range.describe(|seconds| seconds.to_string()),
            "Created: from 100 until (excluding) 200"
        );

        let range = &ranges("created > @100 and created <= @200")[0];
        assert_eq!(
            range.lower,
            Some(DateBound {
                seconds: 100,
                inclusive: false
            })
        );
        assert_eq!(
            range.upper,
            Some(DateBound {
                seconds: 200,
                inclusive: true
            })
        );
        assert_eq!(
            range.describe(|seconds| seconds.to_string()),
            "Created: after 100 through 200"
        );

        let range = &ranges("modified > @100 and modified < @200")[0];
        assert_eq!(
            range.describe(|seconds| seconds.to_string()),
            "Modified: after 100 until (excluding) 200"
        );
        assert_eq!(
            ranges("modified = @150")[0].describe(|seconds| seconds.to_string()),
            "Modified: exactly 150"
        );
        assert_eq!(
            ranges("created >= @100 and created <= @200")[0]
                .describe(|seconds| seconds.to_string()),
            "Created: from 100 through 200"
        );
    }

    #[test]
    fn readable_dates_merge_strongest_bounds_without_combining_fields() {
        let ranges = ranges(
            "created >= @100 and created > @100 and created >= @100 and created >= @90 and created <= @220 and created < @200 and created <= @200 and modified >= @300",
        );
        assert_eq!(ranges.len(), 2);
        assert_eq!(
            ranges[0].lower,
            Some(DateBound {
                seconds: 100,
                inclusive: false
            })
        );
        assert_eq!(
            ranges[0].upper,
            Some(DateBound {
                seconds: 200,
                inclusive: false
            })
        );
        assert_eq!(ranges[1].field, Field::Modified);
        assert_eq!(
            ranges[1].lower,
            Some(DateBound {
                seconds: 300,
                inclusive: true
            })
        );
        assert_eq!(ranges[1].upper, None);
        assert_eq!(
            ranges[1].describe(|seconds| seconds.to_string()),
            "Modified: on or after 300"
        );
        assert_eq!(
            self::ranges("created <= @150")[0].describe(|seconds| seconds.to_string()),
            "Created: on or before 150"
        );
    }

    #[test]
    fn readable_dates_never_flatten_branches_negation_or_impossible_ranges() {
        for dsl in [
            "(file and created >= @100) or (dir and created < @200)",
            "not (file and created >= @100)",
            "file and not (created < @200)",
            "created >= @200 and created < @200",
            "created > @200 and created < @100",
            "created != @150",
            "created in (@100, @200)",
        ] {
            assert!(
                ranges(dsl).is_empty(),
                "misleading date annotation for {dsl}"
            );
        }
        assert_eq!(ranges("file and created >= @100 and ext != pdf").len(), 1);
        assert_eq!(ranges("(file or dir) and created >= @100").len(), 1);
        assert_eq!(
            ranges("created >= @100 and created <= @200 and not ext = pdf").len(),
            1
        );
    }

    #[test]
    fn every_offered_natural_example_has_an_executable_translation() {
        let context = kosha_search::SearchContext::new(1_795_000_000, [2026, 11, 18]);
        for &(id, phrase) in NATURAL_EXAMPLES {
            let translation =
                kosha_search::translate_with_mode(phrase, context, SearchMode::Natural);
            assert!(
                translation.is_valid(),
                "example {id}: {phrase}: {:?}",
                translation.diagnostics
            );
            let dsl = translation.dsl.expect("valid examples have editable DSL");
            assert!(!dsl.trim().is_empty(), "empty DSL for example {id}");
            assert!(
                kosha_query::Query::parse(&dsl).is_ok(),
                "uneditable DSL for example {id}: {dsl}"
            );
        }
    }

    #[test]
    fn switching_modes_keeps_edited_dsl_until_translation_is_explicitly_requested() {
        let translation = kosha_search::translate(
            "files greater than 10 MB",
            kosha_search::SearchContext::new(1_795_000_000, [2026, 11, 18]),
        );
        assert_eq!(
            editable_dsl("name ~ budget", true, Some(&translation)),
            "name ~ budget"
        );
        assert_eq!(
            editable_dsl("name ~ budget", false, Some(&translation)),
            translation.dsl.unwrap()
        );
    }

    #[test]
    fn creation_metadata_warning_checks_nested_predicates() {
        let query = kosha_query::Query::parse("file and not (created > 2026-01-01)").unwrap();
        assert!(needs_creation(query.expression()));
        assert!(!needs_creation(
            kosha_query::Query::parse("modified > 2026-01-01")
                .unwrap()
                .expression()
        ));
    }
}
