// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Squarified treemap layout.
//!
//! A treemap is the fastest way for a person to see "one thing is 80% of this".
//! The squarified variant (Bruls, Huizing and van Wijk, 2000) trades exact
//! ordering for tiles that are close to square, which is what makes them
//! comparable by eye and clickable by hand — long thin slivers are neither.
//!
//! The layout is a pure function of areas and a bounding box, so it is tested
//! here without a window, a tree, or a GPU.

use kosha_core::{NodeId, SizeTree};

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    /// Equal to within half a pixel.
    ///
    /// For comparing a measured size against the stored one. Exact equality
    /// would be wrong here twice over: a measurement is a float that need not
    /// reproduce bit for bit, and storing it notifies, which re-renders, which
    /// measures again — so a comparison that is never quite true is a render
    /// loop rather than a stale value.
    pub fn approx_eq(&self, other: Rect) -> bool {
        const EPSILON: f32 = 0.5;
        (self.x - other.x).abs() < EPSILON
            && (self.y - other.y).abs() < EPSILON
            && (self.w - other.w).abs() < EPSILON
            && (self.h - other.h).abs() < EPSILON
    }
}

/// Lay out `areas` inside `bounds`, largest first.
///
/// `areas` must be sorted descending; the algorithm relies on it to know that
/// the first element of a row is its largest and the last is its smallest.
/// Returns one rect per input, in input order. Zero areas get zero rects.
pub fn squarify(areas: &[f64], bounds: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::default(); areas.len()];
    let total: f64 = areas.iter().sum();
    if total <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return out;
    }

    // Work in pixel-area units so a row's thickness is just sum / side.
    let scale = (f64::from(bounds.w) * f64::from(bounds.h)) / total;
    let scaled: Vec<f64> = areas.iter().map(|a| a * scale).collect();

    let mut rect = bounds;
    let mut start = 0;

    while start < scaled.len() {
        if scaled[start] <= 0.0 {
            start += 1;
            continue;
        }
        let side = f64::from(rect.w.min(rect.h));
        if side <= 0.0 {
            break;
        }

        // Extend the row while doing so makes its worst tile *more* square.
        let mut end = start + 1;
        let mut sum = scaled[start];
        let mut best = worst_ratio(scaled[start], scaled[start], sum, side);
        while end < scaled.len() && scaled[end] > 0.0 {
            let next_sum = sum + scaled[end];
            // The input is sorted, so the row's max is its first element and
            // its min is the candidate we are considering adding.
            let candidate = worst_ratio(scaled[start], scaled[end], next_sum, side);
            if candidate > best {
                break;
            }
            best = candidate;
            sum = next_sum;
            end += 1;
        }

        // The strip is taken off the longer dimension and runs along the
        // shorter one, which is what keeps tiles near-square.
        let thickness = (sum / side) as f32;
        let along_height = rect.w >= rect.h;
        let mut offset = 0.0f32;

        for (index, item) in scaled[start..end].iter().enumerate() {
            let portion = ((item / sum) * side) as f32;
            out[start + index] = if along_height {
                Rect::new(rect.x, rect.y + offset, thickness, portion)
            } else {
                Rect::new(rect.x + offset, rect.y, portion, thickness)
            };
            offset += portion;
        }

        if along_height {
            rect.x += thickness;
            rect.w -= thickness;
        } else {
            rect.y += thickness;
            rect.h -= thickness;
        }
        start = end;
    }

    out
}

/// The worst aspect ratio in a row, which is the quantity squarification
/// minimises. Lower is squarer; 1.0 is a perfect square.
fn worst_ratio(max: f64, min: f64, sum: f64, side: f64) -> f64 {
    if min <= 0.0 || sum <= 0.0 || side <= 0.0 {
        return f64::MAX;
    }
    let sum2 = sum * sum;
    let side2 = side * side;
    ((side2 * max) / sum2).max(sum2 / (side2 * min))
}

/// One drawable cell.
pub struct Tile {
    pub id: NodeId,
    pub rect: Rect,
    /// 0 for a child of the directory in view, 1 for its children, and so on.
    pub depth: u8,
}

/// The finished layout for one directory.
pub struct Layout {
    /// Parents before their children, so painting in order puts the nested
    /// tiles on top of the tile they sit inside.
    pub tiles: Vec<Tile>,
    /// Top-level children too small to draw. Reported rather than silently
    /// dropped, so the UI can say "and 412 smaller" instead of quietly lying
    /// about what is in the directory.
    pub pooled: usize,
}

/// Padding between a tile's edge and the tiles nested inside it.
const NEST_PAD: f32 = 3.0;

/// Height reserved at the top of a nested parent for its own label.
const LABEL_STRIP: f32 = 30.0;

/// The smallest interior worth subdividing.
///
/// Below this a nested layout is a handful of unreadable slivers on top of a
/// tile that was perfectly legible before, which is a worse picture than the
/// one it replaced.
const MIN_NEST: f32 = 56.0;

/// A tile with room for its name and size.
///
/// Shared with the renderer so the space reserved for a label and the decision
/// to draw one cannot disagree — a label drawn where no room was reserved
/// overlaps the tiles beneath it.
pub fn labelled(rect: Rect) -> bool {
    rect.w > 96.0 && rect.h > 34.0
}

/// Build the tiles for a directory, nesting up to `max_depth` levels inside.
///
/// `max_depth` of 0 is the flat layout. Nesting stops on its own wherever a
/// tile's interior is too small to be worth subdividing, so a deep budget costs
/// nothing on a directory that cannot use it.
///
/// Anything that would render smaller than `min_side` is dropped rather than
/// drawn: a hundred sub-pixel slivers are visual noise that cannot be clicked,
/// read, or told apart.
pub fn layout(
    tree: &SizeTree,
    children: &[NodeId],
    bounds: Rect,
    min_side: f32,
    max_depth: u8,
) -> Layout {
    let mut tiles = Vec::with_capacity(children.len() * 2);
    let pooled = place(tree, children, bounds, min_side, 0, max_depth, &mut tiles);
    Layout { tiles, pooled }
}

/// Lay out one level, and recurse into whichever tiles can carry it.
///
/// Returns how many of `children` were too small to draw.
fn place(
    tree: &SizeTree,
    children: &[NodeId],
    bounds: Rect,
    min_side: f32,
    depth: u8,
    max_depth: u8,
    out: &mut Vec<Tile>,
) -> usize {
    // `squarify` documents that it needs descending areas, and the caller's
    // order is whatever the user sorted the *table* by — which may be name.
    // Sorting here rather than trusting the caller keeps the contract local,
    // and a treemap is size-ordered by nature anyway.
    // The same order `SizeTree::children_by_size` produces, deliberately: the
    // treemap and the table must not disagree about which of two equal-sized
    // things comes first.
    let mut order: Vec<NodeId> = children.to_vec();
    order.sort_unstable_by(|&a, &b| {
        let (na, nb) = (tree.node(a), tree.node(b));
        nb.allocated
            .cmp(&na.allocated)
            .then_with(|| nb.logical.cmp(&na.logical))
            .then_with(|| tree.name(a).cmp(tree.name(b)))
    });

    let areas: Vec<f64> = order
        .iter()
        .map(|&id| tree.node(id).allocated as f64)
        .collect();
    let rects = squarify(&areas, bounds);
    let mut pooled = 0;

    for (&id, rect) in order.iter().zip(rects) {
        if rect.area() <= 0.0 || rect.w < min_side || rect.h < min_side {
            // Everything after this is smaller still, but a row can end with a
            // sliver while the next row starts wide again, so keep scanning.
            pooled += 1;
            continue;
        }
        out.push(Tile { id, rect, depth });

        if depth < max_depth
            && tree.node(id).is_dir()
            && let Some(inner) = interior(rect)
        {
            let kids: Vec<NodeId> = tree.children(id).collect();
            if !kids.is_empty() {
                // Nested pooling is not reported: "and 12 smaller" about the
                // inside of a tile the user has not opened is noise about a
                // level they did not ask to see.
                place(tree, &kids, inner, min_side, depth + 1, max_depth, out);
            }
        }
    }
    pooled
}

/// The area inside a tile that its children may use, if there is one.
///
/// Leaves the label strip alone when the tile is showing one, so nesting never
/// draws over the name of the thing being nested into.
fn interior(rect: Rect) -> Option<Rect> {
    let top = if labelled(rect) {
        LABEL_STRIP
    } else {
        NEST_PAD
    };
    let inner = Rect::new(
        rect.x + NEST_PAD,
        rect.y + top,
        rect.w - NEST_PAD * 2.0,
        rect.h - top - NEST_PAD,
    );
    (inner.w >= MIN_NEST && inner.h >= MIN_NEST).then_some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 600.0,
        h: 400.0,
    };

    fn covered(rects: &[Rect]) -> f32 {
        rects.iter().map(Rect::area).sum()
    }

    #[test]
    fn fills_the_bounds() {
        let rects = squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], BOUNDS);
        let expected = BOUNDS.area();
        assert!(
            (covered(&rects) - expected).abs() < expected * 0.01,
            "covered {} of {expected}",
            covered(&rects)
        );
    }

    #[test]
    fn area_is_proportional_to_value() {
        let rects = squarify(&[75.0, 25.0], BOUNDS);
        let ratio = rects[0].area() / rects[1].area();
        assert!((ratio - 3.0).abs() < 0.05, "ratio was {ratio}");
    }

    #[test]
    fn tiles_do_not_overlap() {
        let rects = squarify(&[9.0, 7.0, 5.0, 4.0, 3.0, 2.0, 2.0, 1.0, 1.0], BOUNDS);
        for (i, a) in rects.iter().enumerate() {
            for b in rects.iter().skip(i + 1) {
                let disjoint = a.x + a.w <= b.x + 0.01
                    || b.x + b.w <= a.x + 0.01
                    || a.y + a.h <= b.y + 0.01
                    || b.y + b.h <= a.y + 0.01;
                assert!(disjoint, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn tiles_stay_inside_the_bounds() {
        let rects = squarify(&[10.0, 8.0, 6.0, 4.0, 2.0, 1.0], BOUNDS);
        for r in &rects {
            assert!(r.x >= -0.01 && r.y >= -0.01, "{r:?}");
            assert!(r.x + r.w <= BOUNDS.w + 0.01, "{r:?}");
            assert!(r.y + r.h <= BOUNDS.h + 0.01, "{r:?}");
        }
    }

    #[test]
    fn tiles_are_reasonably_square() {
        // The entire point of squarifying. A naive slice-and-dice layout would
        // produce ratios in the hundreds for the smallest items.
        let values: Vec<f64> = (1..=24).map(|n| f64::from(25 - n)).collect();
        let rects = squarify(&values, BOUNDS);
        for r in rects.iter().filter(|r| r.area() > 1.0) {
            let ratio = (r.w / r.h).max(r.h / r.w);
            assert!(ratio < 12.0, "sliver {r:?} has ratio {ratio}");
        }
    }

    #[test]
    fn one_item_takes_everything() {
        let rects = squarify(&[1.0], BOUNDS);
        assert!((rects[0].w - BOUNDS.w).abs() < 0.01);
        assert!((rects[0].h - BOUNDS.h).abs() < 0.01);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert!(squarify(&[], BOUNDS).is_empty());
        assert_eq!(squarify(&[0.0, 0.0], BOUNDS).len(), 2);
        assert_eq!(squarify(&[1.0], Rect::new(0.0, 0.0, 0.0, 0.0)).len(), 1);
        // A zero among real values must not consume area or wedge the loop.
        let rects = squarify(&[10.0, 0.0, 5.0], BOUNDS);
        assert_eq!(rects[1].area(), 0.0);
        assert!(rects[0].area() > rects[2].area());
    }

    // ------------------------------------------------------------- nesting

    /// ```text
    /// root/
    ///   big/     one 8 MiB file and one 2 MiB file
    ///   mid/     one 3 MiB file
    ///   small.txt
    /// ```
    fn nested_fixture() -> (tempfile::TempDir, kosha_core::SizeTree) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("big")).unwrap();
        std::fs::create_dir_all(root.join("mid")).unwrap();
        std::fs::write(root.join("big/huge.bin"), vec![0u8; 8 << 20]).unwrap();
        std::fs::write(root.join("big/less.bin"), vec![0u8; 2 << 20]).unwrap();
        std::fs::write(root.join("mid/some.bin"), vec![0u8; 3 << 20]).unwrap();
        std::fs::write(root.join("small.txt"), b"tiny").unwrap();

        let tree = kosha_core::Scanner::new(kosha_core::ScanOptions::default())
            .scan(root)
            .unwrap();
        (dir, tree)
    }

    fn parent_of(tree: &kosha_core::SizeTree, id: NodeId) -> NodeId {
        NodeId::from_index(tree.node(id).parent as usize)
    }

    fn contains(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x - 0.01
            && inner.y >= outer.y - 0.01
            && inner.x + inner.w <= outer.x + outer.w + 0.01
            && inner.y + inner.h <= outer.y + outer.h + 0.01
    }

    #[test]
    fn a_depth_of_zero_is_the_flat_layout() {
        let (_dir, tree) = nested_fixture();
        let rows: Vec<NodeId> = tree.children(tree.root()).collect();

        let flat = layout(&tree, &rows, BOUNDS, 4.0, 0);
        assert!(flat.tiles.iter().all(|tile| tile.depth == 0));
        assert!(flat.tiles.len() <= rows.len());
    }

    #[test]
    fn nested_tiles_stay_inside_the_tile_they_belong_to() {
        let (_dir, tree) = nested_fixture();
        let rows: Vec<NodeId> = tree.children(tree.root()).collect();
        let out = layout(&tree, &rows, BOUNDS, 4.0, 2);

        assert!(
            out.tiles.iter().any(|tile| tile.depth > 0),
            "nothing nested at all: {:?}",
            out.tiles.iter().map(|t| t.depth).collect::<Vec<_>>()
        );

        for tile in &out.tiles {
            if tile.depth == 0 {
                continue;
            }
            let parent = parent_of(&tree, tile.id);
            let outer = out
                .tiles
                .iter()
                .find(|candidate| candidate.id == parent)
                .unwrap_or_else(|| panic!("a nested tile has no parent tile drawn"));
            assert!(
                contains(outer.rect, tile.rect),
                "a child escaped its parent: {:?} not inside {:?}",
                tile.rect,
                outer.rect
            );
        }
    }

    #[test]
    fn a_parent_is_always_drawn_before_its_children() {
        let (_dir, tree) = nested_fixture();
        let rows: Vec<NodeId> = tree.children(tree.root()).collect();
        let out = layout(&tree, &rows, BOUNDS, 4.0, 2);

        // Painting is in order, so a parent appearing after its child would
        // cover the child completely.
        let mut seen = Vec::new();
        for tile in &out.tiles {
            if tile.depth > 0 {
                let parent = parent_of(&tree, tile.id);
                assert!(
                    seen.contains(&parent),
                    "a child would be painted over by its own parent"
                );
            }
            seen.push(tile.id);
        }
    }

    #[test]
    fn siblings_do_not_overlap_at_any_depth() {
        let (_dir, tree) = nested_fixture();
        let rows: Vec<NodeId> = tree.children(tree.root()).collect();
        let out = layout(&tree, &rows, BOUNDS, 4.0, 2);

        for (i, a) in out.tiles.iter().enumerate() {
            for b in out.tiles.iter().skip(i + 1) {
                if parent_of(&tree, a.id) != parent_of(&tree, b.id) || a.depth != b.depth {
                    continue;
                }
                let disjoint = a.rect.x + a.rect.w <= b.rect.x + 0.01
                    || b.rect.x + b.rect.w <= a.rect.x + 0.01
                    || a.rect.y + a.rect.h <= b.rect.y + 0.01
                    || b.rect.y + b.rect.h <= a.rect.y + 0.01;
                assert!(disjoint, "siblings overlap: {:?} and {:?}", a.rect, b.rect);
            }
        }
    }

    #[test]
    fn nesting_never_covers_the_space_reserved_for_a_label() {
        let (_dir, tree) = nested_fixture();
        let rows: Vec<NodeId> = tree.children(tree.root()).collect();
        let out = layout(&tree, &rows, BOUNDS, 4.0, 2);

        for tile in &out.tiles {
            if tile.depth == 0 || !out.tiles.iter().any(|t| t.id == parent_of(&tree, tile.id)) {
                continue;
            }
            let parent = out
                .tiles
                .iter()
                .find(|t| t.id == parent_of(&tree, tile.id))
                .unwrap();
            if !labelled(parent.rect) {
                continue;
            }
            assert!(
                tile.rect.y >= parent.rect.y + LABEL_STRIP - 0.01,
                "a nested tile was drawn over its parent's name"
            );
        }
    }

    #[test]
    fn a_tile_too_small_to_subdivide_is_left_alone() {
        // Below MIN_NEST in either direction there is nothing worth drawing.
        assert!(interior(Rect::new(0.0, 0.0, 40.0, 400.0)).is_none());
        assert!(interior(Rect::new(0.0, 0.0, 400.0, 40.0)).is_none());
        assert!(interior(Rect::new(0.0, 0.0, 400.0, 400.0)).is_some());
    }

    /// The guard on the measurement feedback loop. Too tight and every frame
    /// stores a size and asks for another frame; too loose and the map stops
    /// following the window.
    #[test]
    fn sizes_compare_equal_within_half_a_pixel() {
        let measured = Rect::new(0.0, 0.0, 756.0, 302.0);

        assert!(measured.approx_eq(Rect::new(0.0, 0.0, 756.2, 301.8)));
        assert!(!measured.approx_eq(Rect::new(0.0, 0.0, 757.0, 302.0)));
        assert!(!measured.approx_eq(Rect::new(0.0, 0.0, 756.0, 366.0)));
        assert!(measured.approx_eq(measured));
    }

    #[test]
    fn the_layout_does_not_depend_on_the_order_it_is_given() {
        // The table can be sorted by name, but `squarify` needs descending
        // areas — so the layout sorts for itself, and the same directory must
        // produce the same picture whichever way the rows arrived.
        let (_dir, tree) = nested_fixture();
        let mut rows: Vec<NodeId> = tree.children(tree.root()).collect();
        let sorted = layout(&tree, &rows, BOUNDS, 4.0, 2);

        rows.reverse();
        let reversed = layout(&tree, &rows, BOUNDS, 4.0, 2);

        assert_eq!(sorted.tiles.len(), reversed.tiles.len());
        for (a, b) in sorted.tiles.iter().zip(&reversed.tiles) {
            assert_eq!(a.id, b.id, "the tiles came out in a different order");
            assert_eq!(a.rect, b.rect, "the same tile landed somewhere else");
        }
    }
}
