// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Which columns the table shows, in what order, and how wide.
//!
//! Pure data and arithmetic, deliberately: every rule here — the minimum
//! width, what a drag does at the limit, which column absorbs the leftover
//! space — is a thing that can be got wrong invisibly in a layout tree, and
//! all of it is testable without a window.
//!
//! The widths used to live as bare `px(96.0)` literals in two places, the
//! header and the row, which had to agree for the numbers to sit under their
//! headings and had nothing keeping them agreeing. There is one definition
//! now, and both read it.

use serde::{Deserialize, Serialize};

use crate::state::SortKey;

/// A column of the results table.
///
/// `Name` is special and stays first: it is the flexible one that absorbs
/// whatever width the others do not take, and a table whose labels are not the
/// leftmost thing is not a file listing any more. Everything else can be
/// moved, resized, or hidden.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Column {
    Allocated,
    Logical,
    Items,
    Modified,
}

impl Column {
    /// Every movable column, in the order they ship in.
    pub const ALL: [Column; 4] = [
        Column::Allocated,
        Column::Logical,
        Column::Items,
        Column::Modified,
    ];

    /// The heading, and the same string the command palette uses to name it.
    pub fn label(self) -> &'static str {
        match self {
            Column::Allocated => "ON DISK",
            Column::Logical => "APPARENT",
            Column::Items => "ITEMS",
            Column::Modified => "MODIFIED",
        }
    }

    /// Title case, for prose: menu entries and confirmations.
    pub fn title(self) -> &'static str {
        match self {
            Column::Allocated => "On disk",
            Column::Logical => "Apparent",
            Column::Items => "Items",
            Column::Modified => "Modified",
        }
    }

    /// What clicking this heading sorts by.
    pub fn sort_key(self) -> SortKey {
        match self {
            Column::Allocated => SortKey::Allocated,
            Column::Logical => SortKey::Logical,
            Column::Items => SortKey::Items,
            Column::Modified => SortKey::Modified,
        }
    }

    /// Shipping width in logical pixels.
    pub fn default_width(self) -> f32 {
        match self {
            Column::Allocated | Column::Logical => 96.0,
            Column::Items => 80.0,
            Column::Modified => 120.0,
        }
    }

    /// Narrowest useful width.
    ///
    /// Not zero, and not the same for every column: these hold right-aligned
    /// text that is clipped from the left, so a column narrower than its
    /// heading shows a heading with its first letters missing, which reads as
    /// a rendering bug rather than as a deliberately narrow column. 56 px is
    /// about where "MODIFIED" stops fitting; below that the user is better
    /// served by hiding the column, which they can do.
    pub const MIN_WIDTH: f32 = 56.0;

    /// Widest, so one column cannot squeeze the names out of existence.
    pub const MAX_WIDTH: f32 = 400.0;

    /// Position in [`Column::ALL`], used to keep the width list canonical.
    fn index(self) -> usize {
        Column::ALL
            .iter()
            .position(|&c| c == self)
            .expect("ALL lists every variant")
    }
}

/// The user's table layout: order, widths, and what is hidden.
///
/// `order` holds every column exactly once — hiding is a separate flag rather
/// than removal, so a column that comes back returns to where it was and at
/// the width it had. Losing a hand-set width because a column was hidden for a
/// moment would make hiding feel destructive.
///
/// `widths` is always kept in [`Column::ALL`] order, which is not the display
/// order and deliberately so: it carries no meaning, and if its order were
/// allowed to drift then two layouts that draw identically would compare
/// unequal. `is_default` is that comparison, so the drift would show up as a
/// "Reset columns" command offered when there is nothing to reset. Caught by
/// the round-trip test, which is the only place the two orders differ.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    order: Vec<Column>,
    widths: Vec<(Column, f32)>,
    hidden: Vec<Column>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            order: Column::ALL.to_vec(),
            widths: Column::ALL
                .iter()
                .map(|&column| (column, column.default_width()))
                .collect(),
            hidden: Vec::new(),
        }
    }
}

impl Layout {
    /// Discard anything a hand-edited or older settings file got wrong.
    ///
    /// Settings are a file a user can open, and this is deserialised straight
    /// from it, so "the file says something impossible" is a case that happens
    /// rather than one that cannot. A duplicated column would render twice and
    /// a missing one would be unreachable with no way to bring it back, so
    /// both are repaired in place: unknown-to-this-version entries drop out,
    /// absent ones are appended in their shipping order, and every width is
    /// clamped. Repaired rather than reset, because throwing away a layout
    /// somebody arranged is a worse answer to one bad field.
    pub fn sanitised(mut self) -> Self {
        let mut order = Vec::with_capacity(Column::ALL.len());
        for column in self.order.drain(..) {
            if !order.contains(&column) {
                order.push(column);
            }
        }
        for column in Column::ALL {
            if !order.contains(&column) {
                order.push(column);
            }
        }

        // Built from `Column::ALL`, not from `order`: see the type's docs.
        let widths = Column::ALL
            .iter()
            .map(|&column| {
                let width = self
                    .widths
                    .iter()
                    .find(|(c, _)| *c == column)
                    .map(|(_, w)| *w)
                    .filter(|w| w.is_finite())
                    .unwrap_or_else(|| column.default_width());
                (column, width.clamp(Column::MIN_WIDTH, Column::MAX_WIDTH))
            })
            .collect();

        let mut hidden: Vec<Column> = Vec::new();
        for column in self.hidden.drain(..) {
            if !hidden.contains(&column) {
                hidden.push(column);
            }
        }
        // Never all of them: a table of names with no numbers is not a size
        // analyser, and there would be no heading left to right-click on to
        // get one back.
        if hidden.len() >= Column::ALL.len() {
            hidden.clear();
        }

        Layout {
            order,
            widths,
            hidden,
        }
    }

    /// The columns to draw, left to right.
    pub fn visible(&self) -> impl Iterator<Item = (Column, f32)> + '_ {
        self.order
            .iter()
            .filter(|column| !self.hidden.contains(column))
            .map(|&column| (column, self.width(column)))
    }

    /// What the visible fixed-width columns cost altogether.
    ///
    /// The name column takes whatever is left, so this is the number the
    /// window layout has to work around — and it moves, because widths are
    /// draggable and columns can be hidden. See `metrics::rails_for`.
    pub fn fixed_width(&self) -> f32 {
        self.visible().map(|(_, width)| width).sum()
    }

    /// Every column in order with its visibility, for building a menu.
    pub fn all(&self) -> impl Iterator<Item = (Column, bool)> + '_ {
        self.order
            .iter()
            .map(|&column| (column, !self.hidden.contains(&column)))
    }

    pub fn width(&self, column: Column) -> f32 {
        self.widths
            .iter()
            .find(|(c, _)| *c == column)
            .map(|(_, w)| *w)
            .unwrap_or_else(|| column.default_width())
    }

    /// Set a column's width, clamped to its limits.
    ///
    /// Clamped rather than rejected: a drag that runs past the minimum should
    /// stop there and stay responsive, not stop applying and leave the pointer
    /// detached from the edge it is holding.
    pub fn set_width(&mut self, column: Column, width: f32) {
        let width = if width.is_finite() {
            width.clamp(Column::MIN_WIDTH, Column::MAX_WIDTH)
        } else {
            column.default_width()
        };
        match self.widths.iter_mut().find(|(c, _)| *c == column) {
            Some(entry) => entry.1 = width,
            None => {
                // Inserted in `Column::ALL` order rather than appended, so the
                // list stays canonical and equality stays meaningful.
                let at = self
                    .widths
                    .iter()
                    .position(|(c, _)| c.index() > column.index())
                    .unwrap_or(self.widths.len());
                self.widths.insert(at, (column, width));
            }
        }
    }

    /// Move a column one place towards the names.
    ///
    /// Returns whether anything moved, so a command can be offered only when
    /// it would do something. Steps over hidden columns rather than trading
    /// places with one — otherwise the first press of "move left" appears to
    /// do nothing at all.
    pub fn move_left(&mut self, column: Column) -> bool {
        let Some(from) = self.order.iter().position(|&c| c == column) else {
            return false;
        };
        let Some(to) = self.order[..from]
            .iter()
            .rposition(|c| !self.hidden.contains(c))
        else {
            return false;
        };
        self.order.remove(from);
        self.order.insert(to, column);
        true
    }

    /// Move a column one place away from the names.
    pub fn move_right(&mut self, column: Column) -> bool {
        let Some(from) = self.order.iter().position(|&c| c == column) else {
            return false;
        };
        let Some(offset) = self.order[from + 1..]
            .iter()
            .position(|c| !self.hidden.contains(c))
        else {
            return false;
        };
        let to = from + 1 + offset;
        self.order.remove(from);
        self.order.insert(to, column);
        true
    }

    /// Show or hide a column. Refuses to hide the last visible one.
    pub fn set_hidden(&mut self, column: Column, hidden: bool) -> bool {
        if hidden {
            if self.hidden.contains(&column) {
                return false;
            }
            if self.hidden.len() + 1 >= Column::ALL.len() {
                return false;
            }
            self.hidden.push(column);
            true
        } else {
            let before = self.hidden.len();
            self.hidden.retain(|&c| c != column);
            self.hidden.len() != before
        }
    }

    /// Back to the shipping layout.
    pub fn reset(&mut self) {
        *self = Layout::default();
    }

    /// Whether this differs from the shipping layout, so "Reset columns" can
    /// be offered only when it would change something.
    pub fn is_default(&self) -> bool {
        *self == Layout::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipping total, which the window's breakpoints are calibrated
    /// against. If a default width changes, that calibration changes with it.
    #[test]
    fn the_fixed_columns_cost_what_the_layout_thinks_they_do() {
        let layout = Layout::default();
        assert_eq!(layout.fixed_width(), 96.0 + 96.0 + 80.0 + 120.0);
    }

    /// Hiding a column gives its width back to the names, and the window
    /// layout has to see that or it keeps reserving room for nothing.
    #[test]
    fn hiding_a_column_makes_the_fixed_width_smaller() {
        let mut layout = Layout::default();
        let before = layout.fixed_width();
        layout.set_hidden(Column::Modified, true);
        assert_eq!(layout.fixed_width(), before - 120.0);
    }

    #[test]
    fn ships_with_every_column_visible_in_order() {
        let layout = Layout::default();
        let shown: Vec<Column> = layout.visible().map(|(column, _)| column).collect();
        assert_eq!(shown, Column::ALL.to_vec());
        assert!(layout.is_default());
    }

    /// The reason this module exists: the header and the row have to agree,
    /// and now they read the same number instead of each holding a literal.
    #[test]
    fn default_widths_match_what_the_table_shipped_with() {
        let layout = Layout::default();
        assert_eq!(layout.width(Column::Allocated), 96.0);
        assert_eq!(layout.width(Column::Logical), 96.0);
        assert_eq!(layout.width(Column::Items), 80.0);
        assert_eq!(layout.width(Column::Modified), 120.0);
        assert_eq!(
            layout.visible().map(|(_, width)| width).sum::<f32>(),
            96.0 + 96.0 + 80.0 + 120.0,
            "and the row's cells add up to what the header lays out"
        );
    }

    #[test]
    fn widths_clamp_rather_than_refuse() {
        let mut layout = Layout::default();

        layout.set_width(Column::Items, 5.0);
        assert_eq!(layout.width(Column::Items), Column::MIN_WIDTH);

        layout.set_width(Column::Items, 10_000.0);
        assert_eq!(layout.width(Column::Items), Column::MAX_WIDTH);

        layout.set_width(Column::Items, 140.0);
        assert_eq!(layout.width(Column::Items), 140.0);
    }

    /// A drag delta arrives as a float, and a zero-width window or a divide
    /// somewhere upstream can make it NaN. `clamp` on a NaN returns NaN, which
    /// would poison the width and lay the row out as nothing at all.
    #[test]
    fn a_width_that_is_not_a_number_falls_back_rather_than_propagating() {
        let mut layout = Layout::default();
        layout.set_width(Column::Modified, f32::NAN);
        assert_eq!(layout.width(Column::Modified), 120.0);
        layout.set_width(Column::Modified, f32::INFINITY);
        assert_eq!(layout.width(Column::Modified), 120.0);
    }

    #[test]
    fn moving_walks_a_column_across_and_stops_at_the_edge() {
        let mut layout = Layout::default();

        assert!(!layout.move_left(Column::Allocated), "already leftmost");
        assert!(layout.move_right(Column::Allocated));
        assert_eq!(
            layout.visible().map(|(c, _)| c).collect::<Vec<_>>(),
            vec![
                Column::Logical,
                Column::Allocated,
                Column::Items,
                Column::Modified
            ]
        );

        assert!(layout.move_left(Column::Allocated));
        assert!(layout.is_default(), "a move and its reverse cancel out");

        assert!(!layout.move_right(Column::Modified), "already rightmost");
    }

    /// Trading places with something invisible looks exactly like a dead
    /// keybinding.
    #[test]
    fn moving_steps_over_a_hidden_column() {
        let mut layout = Layout::default();
        assert!(layout.set_hidden(Column::Logical, true));

        assert!(layout.move_right(Column::Allocated));
        assert_eq!(
            layout.visible().map(|(c, _)| c).collect::<Vec<_>>(),
            vec![Column::Items, Column::Allocated, Column::Modified],
            "one press has to move it past the hidden column, not into it"
        );
    }

    #[test]
    fn hiding_keeps_the_width_and_the_place() {
        let mut layout = Layout::default();
        layout.set_width(Column::Items, 150.0);

        assert!(layout.set_hidden(Column::Items, true));
        assert_eq!(
            layout.all().find(|(c, _)| *c == Column::Items),
            Some((Column::Items, false)),
            "still listed, just not shown"
        );
        assert_eq!(
            layout.visible().map(|(_, width)| width).sum::<f32>(),
            96.0 + 96.0 + 120.0
        );

        assert!(layout.set_hidden(Column::Items, false));
        assert_eq!(
            layout.visible().map(|(c, _)| c).collect::<Vec<_>>(),
            Column::ALL.to_vec(),
            "it came back where it was"
        );
        assert_eq!(layout.width(Column::Items), 150.0, "and as wide as it was");
    }

    #[test]
    fn the_last_column_cannot_be_hidden() {
        let mut layout = Layout::default();
        assert!(layout.set_hidden(Column::Allocated, true));
        assert!(layout.set_hidden(Column::Logical, true));
        assert!(layout.set_hidden(Column::Items, true));
        assert!(
            !layout.set_hidden(Column::Modified, true),
            "hiding the last one leaves no heading to bring any of them back"
        );
        assert_eq!(layout.visible().count(), 1);
    }

    #[test]
    fn reset_returns_the_shipping_layout() {
        let mut layout = Layout::default();
        layout.set_width(Column::Items, 200.0);
        layout.move_right(Column::Allocated);
        layout.set_hidden(Column::Modified, true);
        assert!(!layout.is_default());

        layout.reset();
        assert!(layout.is_default());
    }

    // ----------------------------------------------------- hand-edited files

    #[test]
    fn a_duplicated_column_is_repaired_not_rendered_twice() {
        let broken = Layout {
            order: vec![
                Column::Items,
                Column::Items,
                Column::Allocated,
                Column::Logical,
                Column::Modified,
            ],
            widths: vec![(Column::Items, 90.0)],
            hidden: Vec::new(),
        };

        let fixed = broken.sanitised();
        assert_eq!(
            fixed.visible().map(|(c, _)| c).collect::<Vec<_>>(),
            vec![
                Column::Items,
                Column::Allocated,
                Column::Logical,
                Column::Modified
            ]
        );
        assert_eq!(fixed.width(Column::Items), 90.0, "the stated width is kept");
        assert_eq!(
            fixed.width(Column::Modified),
            120.0,
            "and the unstated ones default"
        );
    }

    #[test]
    fn a_missing_column_is_appended_rather_than_lost() {
        let partial = Layout {
            order: vec![Column::Modified],
            widths: Vec::new(),
            hidden: Vec::new(),
        };

        let fixed = partial.sanitised();
        assert_eq!(fixed.visible().count(), Column::ALL.len());
        assert_eq!(
            fixed.visible().next().map(|(c, _)| c),
            Some(Column::Modified),
            "what the file did say is respected"
        );
    }

    #[test]
    fn hand_written_widths_are_clamped_on_the_way_in() {
        let silly = Layout {
            order: Column::ALL.to_vec(),
            widths: vec![(Column::Items, -40.0), (Column::Modified, f32::NAN)],
            hidden: Vec::new(),
        };

        let fixed = silly.sanitised();
        assert_eq!(fixed.width(Column::Items), Column::MIN_WIDTH);
        assert_eq!(fixed.width(Column::Modified), 120.0);
    }

    #[test]
    fn a_file_hiding_everything_gets_its_columns_back() {
        let blank = Layout {
            order: Column::ALL.to_vec(),
            widths: Vec::new(),
            hidden: Column::ALL.to_vec(),
        };
        assert_eq!(blank.sanitised().visible().count(), Column::ALL.len());
    }

    #[test]
    fn every_column_maps_to_the_sort_key_its_heading_implies() {
        assert_eq!(Column::Allocated.sort_key(), SortKey::Allocated);
        assert_eq!(Column::Logical.sort_key(), SortKey::Logical);
        assert_eq!(Column::Items.sort_key(), SortKey::Items);
        assert_eq!(Column::Modified.sort_key(), SortKey::Modified);
    }

    #[test]
    fn round_trips_through_the_settings_file() {
        let mut layout = Layout::default();
        layout.set_width(Column::Modified, 160.0);
        layout.move_right(Column::Allocated);
        layout.set_hidden(Column::Items, true);

        let json = serde_json::to_string(&layout).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sanitised(), layout);
    }
}
