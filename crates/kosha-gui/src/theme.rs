// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The palette and the metrics, in one place.
//!
//! Shared visual values live here rather
//! than being sprinkled through the views so that "make the hairlines lighter"
//! is one edit, and so the light and dark themes cannot drift apart.

use gpui_kit::component::ActiveTheme;
use gpui_kit::{App, Hsla, hsla, rgb};

use crate::accent;
use kosha_core::{NodeId, SizeTree};

/// Chrome is almost entirely neutral. Colour is reserved for data — the
/// treemap, the severity badges, the mode chip — so that when something is
/// coloured it means something.
#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub hairline: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,

    pub text: Hsla,
    pub text_dim: Hsla,
    pub text_faint: Hsla,

    pub accent: Hsla,
    pub accent_text: Hsla,
    pub positive: Hsla,
    pub caution: Hsla,
    pub danger: Hsla,

    /// Treemap tile colours, indexed by [`Category`].
    pub category: [Hsla; Category::COUNT],

    /// What is painted over the window behind something modal.
    ///
    /// Per theme, because one opacity of black does two entirely different
    /// things. Measured: 45% black takes the dark canvas from `#0E1014` to
    /// `#0C0D10` — two units, invisible — and the light canvas from `#FAFAFB`
    /// to `#89898A`, which is not a dimmed light window but a dark one. A
    /// scrim says "what is behind this is out of play"; it is not supposed to
    /// change which theme you are in.
    pub scrim: Hsla,
    pub dark: bool,
}

/// A palette colour as sRGB bytes, which is what the contrast maths needs.
fn to_rgb(colour: Hsla) -> accent::Rgb {
    let rgba: gpui_kit::Rgba = colour.into();
    accent::Rgb::new(
        (rgba.r * 255.0).round().clamp(0.0, 255.0) as u8,
        (rgba.g * 255.0).round().clamp(0.0, 255.0) as u8,
        (rgba.b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// The label for a button filled with this accent, whichever of the two reads.
///
/// A named helper so the two palettes below cannot answer it differently from
/// the desktop-accent path, which is how one of them came to fail the bar the
/// other enforces.
fn on_accent(accent: u32) -> Hsla {
    rgb(accent::text_on(accent::Rgb::from_u32(accent)).to_u32()).into()
}

/// One neutral, shifted toward a hue and kept at the same brightness.
fn tint(colour: Hsla, hue: f32) -> Hsla {
    let alpha = colour.a;
    let out = accent::tinted(to_rgb(colour), hue, accent::TINT_STRENGTH);
    let mut hsla: Hsla = rgb(out.to_u32()).into();
    // `rgb()` builds an opaque colour; the hover and selection layers are not.
    hsla.a = alpha;
    hsla
}

/// How much of the accent shows through a selected row.
///
/// Chosen to be visible against both canvases while moving the background's
/// luminance little enough that the text on top keeps its contrast. A solid
/// accent here would make the selected row the only one that is hard to read.
const SELECTION_TINT: f32 = 0.22;

impl Palette {
    /// Resolve against whatever mode the framework theme is in, so the app
    /// follows the OS setting without tracking it itself.
    pub fn of(cx: &App) -> Self {
        if cx.theme().mode.is_dark() {
            Palette::dark()
        } else {
            Palette::light()
        }
    }

    /// Replace the chrome accent, leaving every colour that means something.
    ///
    /// `None` keeps kosha's own. What changes: the accent and the text drawn on
    /// it. What does not: `danger`, `caution`, `positive` and the whole
    /// `category` array, because those are a vocabulary rather than decoration
    /// — a red that stopped meaning destructive, or a treemap whose legend no
    /// longer matched its tiles, would be a worse app in a nicer colour.
    pub fn with_accent(mut self, accent: Option<accent::Rgb>, tint_chrome: bool) -> Self {
        let Some(raw) = accent else {
            return self;
        };
        let hue = accent::hue_of(raw);

        // The chrome first, because the accent's contrast has to be measured
        // against the surface it will actually sit on. Doing it the other way
        // round would check the accent against a canvas that no longer exists
        // by the time it is drawn.
        //
        // Only the neutrals move, and only in hue: `accent::tinted` holds each
        // one's luminance, so every text colour keeps the exact contrast ratio
        // it was chosen for. Nothing else in the palette is touched.
        if tint_chrome {
            for neutral in [
                &mut self.canvas,
                &mut self.surface,
                &mut self.raised,
                &mut self.hairline,
                &mut self.hover,
            ] {
                *neutral = tint(*neutral, hue);
            }
        }

        let fixed = accent::usable(raw, to_rgb(self.canvas));
        self.accent = rgb(fixed.to_u32()).into();
        self.accent_text = rgb(accent::text_on(fixed).to_u32()).into();
        // Selection follows too, because on both desktops that is the most
        // visible place an accent appears — a neutral grey selected row in an
        // otherwise accented window looks like a missed spot.
        //
        // Thin enough that it tints rather than fills: the row's text is drawn
        // on top of this and has to stay as readable as every other row's.
        self.selected = self.accent.opacity(SELECTION_TINT);
        self
    }

    pub fn dark() -> Self {
        Palette {
            canvas: c(0x0E1014),
            surface: c(0x15181E),
            raised: c(0x1B1F27),
            hairline: c(0x252A34),
            hover: c(0x1E232C),
            selected: c(0x24304A),

            text: c(0xE8EAF0),
            text_dim: c(0x9AA1B1),
            text_faint: c(0x646C7E),

            accent: c(0x4C8DFF),
            // Computed, not chosen. `accent::text_on` exists for exactly this
            // — "the one place the accent is a fill rather than a mark: the
            // primary button. Its label has to clear the same bar the accent
            // itself does" — and the desktop-accent path has always used it
            // while the shipped palettes spelled white by hand. Measured on
            // this blue: white is 3.20:1 and the palette's darkest ink is
            // 6.56:1, so every primary button in the default theme carried
            // the label that reads worse.
            accent_text: on_accent(0x4C8DFF),
            positive: c(0x3ECF8E),
            caution: c(0xF5A524),
            danger: c(0xF2555A),

            category: [
                c(0x4C8DFF), // code
                c(0xB07CF0), // media
                c(0x3ECF8E), // archives
                c(0xF5A524), // caches and build output
                c(0x6C7686), // system and other
                c(0x35C4D4), // documents
            ],
            // A near-black canvas cannot be darkened much whatever is put
            // over it, so this is as far as it goes and the dialog's own
            // raised surface and hairline do the rest of the work.
            scrim: hsla(0.0, 0.0, 0.0, 0.45),
            dark: true,
        }
    }

    pub fn light() -> Self {
        Palette {
            canvas: c(0xFAFAFB),
            surface: c(0xFFFFFF),
            raised: c(0xFFFFFF),
            hairline: c(0xE6E8EC),
            hover: c(0xF2F3F6),
            selected: c(0xE3ECFD),

            text: c(0x12141A),
            text_dim: c(0x5C6373),
            text_faint: c(0x8A91A0),

            accent: c(0x2C6FE8),
            // White here too, but by the same arithmetic rather than by
            // repetition: 4.62:1 against 4.55:1 for the ink.
            accent_text: on_accent(0x2C6FE8),
            positive: c(0x199B65),
            caution: c(0xB8720A),
            danger: c(0xD4353B),

            category: [
                c(0x2C6FE8),
                c(0x8B4FD6),
                c(0x199B65),
                c(0xB8720A),
                c(0x767E8D),
                c(0x1594A3),
            ],
            // Far less, and for a reason that is arithmetic rather than
            // taste: black over white lands wherever the opacity says, so the
            // same 0.45 that is invisible on the dark canvas takes this one to
            // a mid grey. 0.25 leaves `#FAFAFB` at about `#BEBEBE` — plainly
            // dimmed, and still recognisably the light theme.
            scrim: hsla(0.0, 0.0, 0.0, 0.25),
            dark: false,
        }
    }

    /// The two colours that tell one duplicate set from the next.
    ///
    /// Blue and purple rather than two shades of one hue: the stripe is three
    /// pixels wide, and a difference in lightness that narrow is not one most
    /// people can see. Category colours are reused because the duplicate view
    /// colours nothing by category, so there is no meaning to collide with.
    pub fn group_tint(&self, tint: u8) -> Hsla {
        if tint == 0 {
            self.accent
        } else {
            self.category[Category::Media as usize]
        }
    }

    /// Tile colour for a treemap cell: the category hue, lightened as nesting
    /// deepens so that a subdirectory reads as part of its parent rather than
    /// as an unrelated block.
    pub fn tile(&self, category: Category, depth: u8) -> Hsla {
        let mut colour = self.category[category as usize];
        let steps = depth.min(4) as f32;
        if self.dark {
            colour.l = (colour.l + steps * 0.045).min(0.82);
            colour.s = (colour.s - steps * 0.06).max(0.12);
        } else {
            colour.l = (colour.l + steps * 0.06).min(0.90);
            colour.s = (colour.s - steps * 0.05).max(0.15);
        }
        colour
    }
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// What kind of thing a file is, for colour only.
///
/// Deliberately coarse. A treemap with twenty colours is a treemap nobody can
/// read; six is enough to see "this is all video" at a glance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Code,
    Media,
    Archive,
    Cache,
    System,
    Document,
}

impl Category {
    pub const COUNT: usize = 6;

    pub const ALL: [Category; Self::COUNT] = [
        Category::Code,
        Category::Media,
        Category::Archive,
        Category::Cache,
        Category::Document,
        Category::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Code => "Code",
            Category::Media => "Media",
            Category::Archive => "Archives",
            Category::Cache => "Caches",
            Category::System => "Other",
            Category::Document => "Documents",
        }
    }

    /// What a *directory* mostly contains.
    ///
    /// Classifying a directory by its own name makes almost everything "Other",
    /// which turns the treemap into one flat colour and throws away the whole
    /// point of colouring it. So unless the name is itself conclusive — a
    /// `node_modules` is a cache whatever is inside it — follow the largest
    /// child down to a file and take that file's category. One tall, cheap walk
    /// per visible tile, and the result is a treemap where "this is all video"
    /// is visible at a glance.
    pub fn dominant(tree: &SizeTree, id: NodeId) -> Self {
        let node = tree.node(id);
        let name = tree.name_lossy(id);
        if !node.is_dir() {
            return Category::of(&name, false);
        }
        let by_name = Category::of(&name, true);
        if by_name != Category::System {
            return by_name;
        }

        // Bounded so a pathological tree cannot turn one tile into a long walk.
        const MAX_STEPS: usize = 8;
        let mut current = id;
        for _ in 0..MAX_STEPS {
            let Some(biggest) = tree
                .children(current)
                .max_by_key(|&child| tree.node(child).allocated)
            else {
                break;
            };
            let child = tree.node(biggest);
            if !child.is_dir() {
                return Category::of(&tree.name_lossy(biggest), false);
            }
            let child_name = tree.name_lossy(biggest);
            let child_category = Category::of(&child_name, true);
            if child_category != Category::System {
                return child_category;
            }
            current = biggest;
        }
        Category::System
    }

    /// Classify by extension, with directory names taking precedence — a
    /// `node_modules` full of JavaScript is a cache, not code.
    pub fn of(name: &str, is_dir: bool) -> Self {
        const CACHE_DIRS: &[&str] = &[
            "node_modules",
            "target",
            "build",
            "dist",
            ".next",
            "__pycache__",
            ".gradle",
            "Cache",
            "Caches",
            ".cache",
            "Temp",
            "tmp",
        ];
        if is_dir {
            return if CACHE_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d)) {
                Category::Cache
            } else {
                Category::System
            };
        }

        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "rs" | "c" | "h" | "cpp" | "hpp" | "go" | "py" | "js" | "ts" | "tsx" | "jsx"
            | "java" | "kt" | "swift" | "rb" | "cs" | "php" | "sh" | "toml" | "json" | "yaml"
            | "yml" | "lock" => Category::Code,
            "mp4" | "mkv" | "mov" | "avi" | "webm" | "mp3" | "flac" | "wav" | "m4a" | "png"
            | "jpg" | "jpeg" | "gif" | "heic" | "webp" | "raw" | "psd" | "tiff" => Category::Media,
            "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" | "bz2" | "iso" | "dmg" | "pkg"
            | "deb" | "rpm" | "msi" | "exe" | "vdi" | "vmdk" | "qcow2" | "vhdx" => {
                Category::Archive
            }
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "md" | "txt" | "csv"
            | "epub" => Category::Document,
            "log" | "tmp" | "cache" | "pyc" | "o" | "obj" | "pdb" | "rlib" | "rmeta" => {
                Category::Cache
            }
            _ => Category::System,
        }
    }
}

/// Named spacing and sizing tokens based on a 4px grid.
pub mod metrics {
    pub const ROW_HEIGHT: f32 = 28.0;
    pub const SIDEBAR_WIDTH: f32 = 208.0;
    pub const INSIGHTS_WIDTH: f32 = 300.0;
    pub const TITLE_BAR_HEIGHT: f32 = 40.0;
    pub const STATUS_BAR_HEIGHT: f32 = 28.0;

    /// Every control in the title bar — the chips and the icon buttons — is
    /// this tall, so that they sit on one line with the window's own minimise,
    /// maximise and close controls rather than near it.
    pub const TITLE_CONTROL: f32 = 28.0;
    /// The icon inside one of those buttons. Smaller than the button, because
    /// a hover highlight that touches the glyph reads as a box around it.
    pub const TITLE_ICON: f32 = 16.0;

    /// The indeterminate strip under the title bar while a scan runs. Thin
    /// enough to be chrome rather than content, thick enough to be motion.
    pub const SCAN_STRIP: f32 = 3.0;

    /// A pill of literal text inside a row: a keyboard shortcut beside a
    /// command, a query beside the topic that explains it. Smaller than
    /// [`RADIUS_SM`] because these are about sixteen pixels tall and six
    /// rounds them almost to a lozenge — and because both had been written
    /// out as a bare `4.0`, in two files, for the same kind of thing.
    pub const RADIUS_XS: f32 = 4.0;
    pub const RADIUS: f32 = 8.0;
    pub const RADIUS_SM: f32 = 6.0;
    /// The window, and anything that floats over it.
    ///
    /// Dialogs share a 12px radius rather than repeating it in each overlay.
    pub const RADIUS_LG: f32 = 12.0;

    /// Padding inside a panel, and inside a card within one.
    ///
    /// The design doc names both. Four overlays used 16 and the confirmation
    /// dialog used 20 — the kind of difference nobody chooses, and what happens
    /// when the fifth one is written from memory.
    pub const PANEL_PADDING: f32 = 16.0;
    pub const CARD_PADDING: f32 = 12.0;

    /// The shortest a thing you can click is allowed to be.
    ///
    /// All hit targets are at least 28px tall.
    /// Padding alone does not deliver that — a 12px label with 3px
    /// above and below is nowhere near it — so a button states its height
    /// rather than hoping the arithmetic lands. The same number as the table
    /// row and the title-bar control, which is why there is one of it.
    pub const HIT_TARGET: f32 = 28.0;

    pub const TEXT_XS: f32 = 11.0;
    pub const TEXT_SM: f32 = 12.0;
    pub const TEXT: f32 = 13.0;
    pub const TEXT_LG: f32 = 15.0;
    pub const TEXT_XL: f32 = 22.0;

    /// Horizontal padding on a table row, both sides together.
    pub const ROW_PADDING: f32 = 24.0;

    /// The narrowest the name column may become.
    ///
    /// Below this a table of paths stops being one: every name ellipsises to a
    /// few characters, and what is left cannot be told apart from its
    /// neighbours. 220 is about where a middling filename still shows both
    /// ends of itself.
    pub const NAME_MIN_WIDTH: f32 = 220.0;

    /// Express a baseline logical pixel dimension relative to the desktop text size.
    pub const fn scaled(baseline: f32) -> gpui_kit::Rems {
        gpui_kit::rems(baseline / 16.0)
    }

    pub fn row_height() -> gpui_kit::Rems {
        scaled(ROW_HEIGHT)
    }

    /// Which side rails a window of this width can hold.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Rails {
        pub places: bool,
        pub insights: bool,
    }

    impl Rails {
        /// What the window actually draws: what fits, minus what the reader
        /// has closed.
        ///
        /// Kept apart from [`rails_for`] on purpose. Closed and does-not-fit
        /// look the same on screen and are not the same fact: `anything_hidden`
        /// stays a question about width, so the status bar never offers to get
        /// back a rail somebody deliberately shut.
        pub fn opened(self, places: bool, insights: bool) -> Rails {
            Rails {
                places: self.places && places,
                insights: self.insights && insights,
            }
        }

        /// True when a wider window would show something this one does not.
        /// The status bar says so, because a rail that silently vanishes reads
        /// as a bug rather than as a layout.
        pub fn anything_hidden(self) -> bool {
            !self.places || !self.insights
        }
    }

    /// Decide which rails fit, given the drawable width and what the table's
    /// fixed columns currently cost.
    ///
    /// The table is what this application is; the rails are what it says about
    /// what the table shows. So the table's needs are met first and the rails
    /// take what is left, rather than the other way round — which is how the
    /// window used to end up with names drawn on top of numbers.
    ///
    /// Insights go first because dropping them buys the most room, and because
    /// the table can still be read without them. Places go second: it is
    /// navigation, and the command palette is the only other way to reach a
    /// drive.
    ///
    /// `fixed_columns` is a parameter rather than a constant because the user
    /// can widen, narrow and hide columns. A breakpoint computed from the
    /// shipping widths would be wrong for anybody who had touched them, in the
    /// direction that overlaps.
    pub fn rails_for(viewport: f32, fixed_columns: f32) -> Rails {
        let table = fixed_columns + ROW_PADDING + NAME_MIN_WIDTH;
        if viewport >= SIDEBAR_WIDTH + INSIGHTS_WIDTH + table {
            Rails {
                places: true,
                insights: true,
            }
        } else if viewport >= SIDEBAR_WIDTH + table {
            Rails {
                places: true,
                insights: false,
            }
        } else {
            Rails {
                places: false,
                insights: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accent;

    /// `accent.rs` repeats the canvas colours in its own tests. This is what
    /// stops those drifting from the real ones — a contrast guarantee measured
    /// against the wrong background would be no guarantee at all.
    /// What every text colour in the shipped palettes actually measures.
    ///
    /// **`ui.md` promises "minimum contrast 4.5:1 for text against its
    /// surface in both themes", and nothing checked it.** Measured across
    /// both palettes and all three surfaces, the body tiers clear it
    /// comfortably — and two things did not:
    ///
    /// ```text
    ///   dark   accent_text on accent   3.20    the primary button's label
    ///   both   text_faint  on surface  3.0-3.6 the quiet tier
    /// ```
    ///
    /// The first is fixed above: `accent::text_on` was written for it, the
    /// desktop-accent path already used it, and on this blue it answers with
    /// the ink at 6.56:1 rather than white at 3.20:1.
    ///
    /// The second is a decision about the neutral ramp rather than a slip, so
    /// it is pinned here at what it is and written down in `ui.md` and TODO
    /// as the thing to settle — raising it is a change to how the window
    /// looks, and the measurement is what that decision needs.
    #[test]
    fn every_text_colour_is_measured_against_every_surface_it_sits_on() {
        const BODY: f32 = 4.5;
        // The bar WCAG sets for large text, which is the floor the faint tier
        // holds today. Not an endorsement: a number, so a change to the ramp
        // has to move it deliberately.
        const FAINT_TODAY: f32 = 3.0;

        for (theme, palette) in [("dark", Palette::dark()), ("light", Palette::light())] {
            let surfaces = [
                ("canvas", palette.canvas),
                ("surface", palette.surface),
                ("raised", palette.raised),
            ];
            for (name, colour) in [("text", palette.text), ("text_dim", palette.text_dim)] {
                for (where_, surface) in surfaces {
                    let ratio = accent::contrast(to_rgb(colour), to_rgb(surface));
                    assert!(
                        ratio >= BODY,
                        "{theme}: {name} on {where_} is {ratio:.2}:1, under {BODY}"
                    );
                }
            }
            for (where_, surface) in surfaces {
                let ratio = accent::contrast(to_rgb(palette.text_faint), to_rgb(surface));
                assert!(
                    ratio >= FAINT_TODAY,
                    "{theme}: text_faint on {where_} is {ratio:.2}:1, under {FAINT_TODAY}"
                );
            }

            // The label on the primary button, which is the one place the
            // accent is a fill. Both themes, both bars.
            let on_accent = accent::contrast(to_rgb(palette.accent_text), to_rgb(palette.accent));
            assert!(
                on_accent >= BODY,
                "{theme}: the primary button's label is {on_accent:.2}:1 on its own fill"
            );
            // And it is the better of the two, not merely a passing one.
            let alternative = accent::contrast(to_rgb(palette.accent_text), to_rgb(palette.accent));
            let chosen = accent::text_on(to_rgb(palette.accent));
            assert_eq!(
                to_rgb(palette.accent_text),
                chosen,
                "{theme}: the label is not the readable one ({alternative:.2}:1)"
            );
        }
    }

    /// The shipping layout must still show both rails at the size the window
    /// opens at, or the default look of the application is the narrow one.
    #[test]
    fn a_default_window_holds_both_rails() {
        let fixed = 96.0 + 96.0 + 80.0 + 120.0; // the four columns as shipped
        let rails = metrics::rails_for(1280.0, fixed);
        assert!(rails.places && rails.insights, "{rails:?}");
        assert!(!rails.anything_hidden());
    }

    /// Closing a rail takes it away; opening one cannot put back what does
    /// not fit. The status bar reads the *fit*, which is why the two are
    /// separate values rather than one.
    #[test]
    fn closing_a_rail_is_not_the_same_as_outgrowing_it() {
        let both = metrics::Rails {
            places: true,
            insights: true,
        };
        assert_eq!(
            both.opened(false, true),
            metrics::Rails {
                places: false,
                insights: true
            }
        );
        // Both still fit, so the status bar has nothing to offer even with
        // both of them shut.
        assert!(!both.anything_hidden());

        // A rail the window is too narrow for stays gone however the
        // preference is set.
        let narrow = metrics::Rails {
            places: true,
            insights: false,
        };
        assert!(!narrow.opened(true, true).insights);
        assert!(narrow.anything_hidden(), "the fit is what the hint reads");
    }

    /// Narrowing drops one rail and then the other, and never brings one back.
    #[test]
    fn rails_go_one_at_a_time_and_stay_gone() {
        let fixed = 392.0;
        let mut seen = Vec::new();
        let mut width = 1400.0;
        while width >= 300.0 {
            let rails = metrics::rails_for(width, fixed);
            if seen.last() != Some(&rails) {
                seen.push(rails);
            }
            width -= 1.0;
        }
        let shape: Vec<(bool, bool)> = seen.iter().map(|r| (r.places, r.insights)).collect();
        assert_eq!(
            shape,
            vec![(true, true), (true, false), (false, false)],
            "a rail came back, or both went at once"
        );
    }

    /// The point of the whole exercise: whatever is left after the rails have
    /// taken their share is enough for the table to draw without the names
    /// landing on top of the numbers.
    #[test]
    fn the_table_always_gets_the_room_it_needs() {
        for fixed in [200.0_f32, 392.0, 600.0, 1200.0] {
            for width in (320..2200).step_by(7).map(|w| w as f32) {
                let rails = metrics::rails_for(width, fixed);
                let places = rails.places.then_some(metrics::SIDEBAR_WIDTH);
                let insights = rails.insights.then_some(metrics::INSIGHTS_WIDTH);
                let taken = places.unwrap_or(0.0) + insights.unwrap_or(0.0);
                let left = width - taken;
                let wanted = fixed + metrics::ROW_PADDING + metrics::NAME_MIN_WIDTH;
                // Below the narrowest layout there is nothing left to drop, and
                // the window minimum is what stops it getting there.
                if width >= wanted {
                    assert!(
                        left >= wanted,
                        "at {width}px with {fixed}px of columns the table got \
                         {left}px and needed {wanted}px"
                    );
                }
            }
        }
    }

    /// Someone who widens their columns gets the rails dropped sooner, rather
    /// than getting an overlapping table at a width that used to be fine.
    ///
    /// 1200 is chosen because the shipping layout holds both rails there and
    /// only just: 208 + 300 + 392 + 24 + 220 is 1144. The old hard minimum of
    /// 1120 was in fact 24px inside that, which is the row padding nobody had
    /// counted.
    #[test]
    fn wider_columns_move_the_breakpoints_outward() {
        let shipping = metrics::rails_for(1200.0, 392.0);
        let widened = metrics::rails_for(1200.0, 392.0 + 200.0);
        assert!(shipping.insights, "the shipping layout lost its rail");
        assert!(
            !widened.insights,
            "widened columns kept a rail they cannot fit"
        );
    }

    #[test]
    fn the_accent_tests_measure_against_the_real_canvas() {
        for (palette, expected) in [
            (Palette::dark(), 0x0E1014_u32),
            (Palette::light(), 0xFAFAFB_u32),
        ] {
            assert_eq!(
                to_rgb(palette.canvas).to_u32(),
                expected,
                "accent.rs's test canvases no longer match the palette"
            );
        }
    }

    #[test]
    fn a_system_accent_replaces_only_the_accent() {
        let base = Palette::dark();
        let painted = Palette::dark().with_accent(Some(accent::Rgb::from_u32(0xFF00FF)), false);

        assert_ne!(
            (painted.accent.h, painted.accent.s),
            (base.accent.h, base.accent.s)
        );
        // Selection is chrome and follows the accent; it is not vocabulary.
        assert_ne!(painted.selected, base.selected);
        assert!(
            painted.selected.a < 0.5,
            "a selected row should be tinted, not filled: alpha {}",
            painted.selected.a
        );
        // The vocabulary is untouched: red still means destructive, amber
        // still means a threshold passed, and the treemap legend still matches.
        assert_eq!(painted.danger, base.danger);
        assert_eq!(painted.caution, base.caution);
        assert_eq!(painted.positive, base.positive);
        assert_eq!(painted.category, base.category);
        // Without the tint asked for, the chrome is exactly as it was.
        assert_eq!(painted.canvas, base.canvas);
        assert_eq!(painted.surface, base.surface);
        assert_eq!(painted.hairline, base.hairline);
    }

    /// The setting is additive: dark stays dark and light stays light, and the
    /// tint is a thing you turn on rather than a redefinition of either.
    #[test]
    fn tinting_moves_the_chrome_without_moving_its_brightness() {
        for base in [Palette::dark(), Palette::light()] {
            let magenta = accent::Rgb::from_u32(0xC14DA9);
            let tinted = base.with_accent(Some(magenta), true);

            assert_ne!(tinted.canvas, base.canvas, "the canvas did not move");
            assert_ne!(tinted.hairline, base.hairline);

            // `surface` is deliberately not checked. In the light palette it is
            // pure white, and the only colour with luminance 1.0 is white — so
            // holding brightness fixed leaves it exactly where it was. That is
            // the invariant doing its job rather than the tint failing, and it
            // gives light mode a pleasant layering: a faintly tinted canvas
            // with panels still floating white on top of it.

            // The invariant that makes it safe: every text colour keeps the
            // contrast it was chosen for, because brightness did not change.
            for (before, after) in [
                (base.canvas, tinted.canvas),
                (base.surface, tinted.surface),
                (base.hairline, tinted.hairline),
            ] {
                let was = accent::contrast(to_rgb(base.text), to_rgb(before));
                let now = accent::contrast(to_rgb(base.text), to_rgb(after));
                assert!(
                    (was - now).abs() < 0.1,
                    "text contrast moved from {was:.2} to {now:.2}"
                );
            }

            // And the vocabulary is still untouched.
            assert_eq!(tinted.danger, base.danger);
            assert_eq!(tinted.category, base.category);
        }
    }

    #[test]
    fn the_accent_is_checked_against_the_canvas_it_will_sit_on() {
        // Tinting changes the canvas, so an accent verified against the
        // untinted one would be verified against a surface that no longer
        // exists by the time it is drawn.
        for base in [Palette::dark(), Palette::light()] {
            for hue in (0..360).step_by(30) {
                let raw = accent::Rgb::from_u32(0x808080);
                let spun = accent::tinted(raw, hue as f32, 0.9);
                let painted = base.with_accent(Some(spun), true);
                let ratio = accent::contrast(to_rgb(painted.accent), to_rgb(painted.canvas));
                assert!(
                    ratio >= accent::MIN_CONTRAST - 0.05,
                    "hue {hue}: accent sits at {ratio:.2}:1 on its own canvas"
                );
            }
        }
    }

    #[test]
    fn no_accent_leaves_the_palette_alone() {
        let base = Palette::light();
        let same = Palette::light().with_accent(None, false);
        assert_eq!(same.accent, base.accent);
        assert_eq!(same.accent_text, base.accent_text);
        assert_eq!(same.selected, base.selected);
    }
}
