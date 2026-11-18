// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Where the window was left, and whether that is still a place it can go.
//!
//! An application that forgets how you arranged it is one you have to arrange
//! every day. That is worth fixing now rather than before, because the layout
//! only recently became worth arranging: until the rails collapsed, every
//! window was at least 1120 wide and there was nothing much to remember.
//!
//! The whole difficulty is that a saved position can become impossible. A
//! laptop is undocked and the second monitor is gone; the dock moves from the
//! bottom of the screen to the side; a settings file roams to a machine with a
//! different display. Restoring blindly puts the window somewhere the user
//! cannot reach it, and a window you cannot reach is indistinguishable from an
//! application that did not start.
//!
//! So the decision is a pure function of the saved numbers and the displays
//! that actually exist, and it is tested against the awkward cases rather than
//! trusted. Anything it cannot vouch for falls back to the centred default,
//! which is always reachable.

use gpui_kit::{Bounds, Pixels, Point, Size, px};
use serde::{Deserialize, Serialize};

/// How much of the window has to land on a display for the position to be
/// usable.
///
/// Not "any overlap at all": one pixel of a title bar showing at the edge of a
/// screen is technically on it and practically lost. This is roughly a corner
/// of the title bar — enough to see it, aim at it, and drag it back.
const MUST_SHOW: Size<f32> = Size {
    width: 120.0,
    height: 40.0,
};

/// A window's last size and position, as written to the settings file.
///
/// Plain numbers rather than GPUI's `Bounds`, because this is in a file people
/// are invited to edit and `{"x": 100, "y": 100, ...}` is a thing anybody can
/// read. The numbers are logical pixels in the desktop's coordinate space,
/// which may be negative: a display to the left of the primary one has
/// negative x, and that is a position, not an error.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Geometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Maximized when it was last closed, so it should come back maximized.
    /// The size is kept anyway: it is what the window restores *to*, and
    /// throwing it away would mean un-maximizing to a default.
    #[serde(default)]
    pub maximized: bool,
}

impl Geometry {
    /// Take the current geometry from a window's reported bounds.
    pub fn of(bounds: gpui_kit::WindowBounds) -> Self {
        // Fullscreen is deliberately recorded as maximized rather than as
        // itself. Coming back up fullscreen with no chrome, because that is
        // how it was closed, is a way to make an application look broken —
        // and the restore size is the same number either way.
        let maximized = !matches!(bounds, gpui_kit::WindowBounds::Windowed(_));
        let inner = bounds.get_bounds();
        Geometry {
            x: f32::from(inner.origin.x),
            y: f32::from(inner.origin.y),
            width: f32::from(inner.size.width),
            height: f32::from(inner.size.height),
            maximized,
        }
    }

    fn bounds(self) -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: px(self.x),
                y: px(self.y),
            },
            size: Size {
                width: px(self.width),
                height: px(self.height),
            },
        }
    }
}

/// Where to open, given what was saved and which displays exist.
///
/// `None` means "no opinion": the caller centres the window at its default
/// size, which is what a first run does and what any unusable saved value
/// falls back to.
///
/// `displays` are the visible areas — excluding the taskbar and the dock —
/// because a window positioned under the taskbar is another way to be
/// unreachable.
pub fn restore(
    saved: Option<Geometry>,
    displays: &[Bounds<Pixels>],
    minimum: Size<f32>,
) -> Option<gpui_kit::WindowBounds> {
    let saved = saved?;

    // A hand-edited file, or a serialisation from a platform that reported
    // something strange. Neither is worth a panic or an invisible window.
    for value in [saved.x, saved.y, saved.width, saved.height] {
        if !value.is_finite() {
            return None;
        }
    }

    // Never smaller than the window will allow anyway. Saving a size below the
    // minimum should not be possible, but a hand-edited file can hold one, and
    // the result would be a window the compositor resizes on the user's behalf
    // to somewhere they did not ask for.
    let size = Size {
        width: saved.width.max(minimum.width),
        height: saved.height.max(minimum.height),
    };

    let wanted = Geometry {
        width: size.width,
        height: size.height,
        ..saved
    }
    .bounds();
    if !displays
        .iter()
        .any(|display| shows_enough(wanted, *display))
    {
        return None;
    }

    Some(if saved.maximized {
        gpui_kit::WindowBounds::Maximized(wanted)
    } else {
        gpui_kit::WindowBounds::Windowed(wanted)
    })
}

/// Does enough of `window` land on `display` to find it and grab it?
fn shows_enough(window: Bounds<Pixels>, display: Bounds<Pixels>) -> bool {
    let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| (a1.min(b1) - a0.max(b0)).max(0.0);

    let (wx, wy) = (f32::from(window.origin.x), f32::from(window.origin.y));
    let (dx, dy) = (f32::from(display.origin.x), f32::from(display.origin.y));
    let horizontal = overlap(
        wx,
        wx + f32::from(window.size.width),
        dx,
        dx + f32::from(display.size.width),
    );
    let vertical = overlap(
        wy,
        wy + f32::from(window.size.height),
        dy,
        dy + f32::from(display.size.height),
    );

    horizontal >= MUST_SHOW.width && vertical >= MUST_SHOW.height
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Size<f32> = Size {
        width: 680.0,
        height: 480.0,
    };

    fn display(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: Point { x: px(x), y: px(y) },
            size: Size {
                width: px(w),
                height: px(h),
            },
        }
    }

    fn saved(x: f32, y: f32, w: f32, h: f32) -> Option<Geometry> {
        Some(Geometry {
            x,
            y,
            width: w,
            height: h,
            maximized: false,
        })
    }

    fn windowed(restored: Option<gpui_kit::WindowBounds>) -> Bounds<Pixels> {
        match restored {
            Some(gpui_kit::WindowBounds::Windowed(bounds)) => bounds,
            other => panic!("expected a windowed restore, got {other:?}"),
        }
    }

    /// A first run has nothing to restore and should say so rather than
    /// inventing a position.
    #[test]
    fn nothing_saved_is_no_opinion() {
        assert!(restore(None, &[display(0.0, 0.0, 1920.0, 1080.0)], MIN).is_none());
    }

    #[test]
    fn an_ordinary_position_comes_back_exactly() {
        let bounds = windowed(restore(
            saved(100.0, 80.0, 1280.0, 820.0),
            &[display(0.0, 0.0, 1920.0, 1080.0)],
            MIN,
        ));
        assert_eq!(f32::from(bounds.origin.x), 100.0);
        assert_eq!(f32::from(bounds.origin.y), 80.0);
        assert_eq!(f32::from(bounds.size.width), 1280.0);
        assert_eq!(f32::from(bounds.size.height), 820.0);
    }

    /// The bug this function exists for. A laptop is undocked and the window
    /// was on the monitor that is now gone; restoring it puts the window
    /// somewhere the user cannot see, click or drag, which is indistinguishable
    /// from the application failing to start.
    #[test]
    fn a_window_on_a_monitor_that_is_gone_is_not_restored() {
        let laptop = display(0.0, 0.0, 1920.0, 1080.0);
        // The second monitor used to sit to the right.
        let on_the_missing_one = saved(2400.0, 200.0, 1280.0, 820.0);
        assert!(restore(on_the_missing_one, &[laptop], MIN).is_none());

        // Plugged back in, the same position is fine again.
        let docked = [laptop, display(1920.0, 0.0, 1920.0, 1080.0)];
        assert!(restore(on_the_missing_one, &docked, MIN).is_some());
    }

    /// A display to the left of the primary one has negative coordinates.
    /// That is a position, not corruption.
    #[test]
    fn a_display_left_of_the_primary_one_works() {
        let displays = [
            display(-1920.0, 0.0, 1920.0, 1080.0),
            display(0.0, 0.0, 1920.0, 1080.0),
        ];
        let bounds = windowed(restore(
            saved(-1800.0, 100.0, 1280.0, 820.0),
            &displays,
            MIN,
        ));
        assert_eq!(f32::from(bounds.origin.x), -1800.0);
    }

    /// A sliver at the screen edge is technically on the display and
    /// practically lost, so it is treated as off it.
    #[test]
    fn a_sliver_hanging_off_the_edge_does_not_count_as_visible() {
        let screen = [display(0.0, 0.0, 1920.0, 1080.0)];
        // Ten pixels of the left edge showing.
        assert!(restore(saved(1910.0, 100.0, 1280.0, 820.0), &screen, MIN).is_none());
        // A title bar's worth is enough to aim at and drag back.
        assert!(restore(saved(1700.0, 100.0, 1280.0, 820.0), &screen, MIN).is_some());
    }

    /// Partly off the bottom is the same problem in the other axis, and it is
    /// the one a moved taskbar causes.
    #[test]
    fn a_window_below_the_visible_area_is_not_restored() {
        let screen = [display(0.0, 0.0, 1920.0, 1080.0)];
        assert!(restore(saved(200.0, 1060.0, 1280.0, 820.0), &screen, MIN).is_none());
    }

    /// A hand-edited file can say anything. None of it should produce a window
    /// that cannot be used.
    #[test]
    fn a_hand_edited_file_cannot_make_an_unusable_window() {
        let screen = [display(0.0, 0.0, 1920.0, 1080.0)];

        // Smaller than the window will allow: raised to the minimum rather
        // than left for the compositor to fix somewhere unexpected.
        let tiny = windowed(restore(saved(100.0, 100.0, 200.0, 150.0), &screen, MIN));
        assert_eq!(f32::from(tiny.size.width), MIN.width);
        assert_eq!(f32::from(tiny.size.height), MIN.height);

        for broken in [
            saved(f32::NAN, 100.0, 1280.0, 820.0),
            saved(100.0, f32::INFINITY, 1280.0, 820.0),
            saved(100.0, 100.0, f32::NAN, 820.0),
            saved(100.0, 100.0, 1280.0, f32::NEG_INFINITY),
        ] {
            assert!(
                restore(broken, &screen, MIN).is_none(),
                "a non-finite value was restored"
            );
        }
    }

    /// No displays at all is not a crash. It happens between a monitor
    /// disconnecting and the next one being noticed.
    #[test]
    fn no_displays_is_no_opinion() {
        assert!(restore(saved(100.0, 100.0, 1280.0, 820.0), &[], MIN).is_none());
    }

    #[test]
    fn a_maximized_window_comes_back_maximized_with_its_restore_size() {
        let restored = restore(
            Some(Geometry {
                maximized: true,
                ..saved(100.0, 80.0, 1280.0, 820.0).unwrap()
            }),
            &[display(0.0, 0.0, 1920.0, 1080.0)],
            MIN,
        );
        match restored {
            Some(gpui_kit::WindowBounds::Maximized(bounds)) => {
                assert_eq!(f32::from(bounds.size.width), 1280.0);
            }
            other => panic!("expected maximized, got {other:?}"),
        }
    }

    /// Fullscreen is recorded as maximized. Coming back up with no chrome
    /// because that is how it was closed is a way to look broken, and the
    /// restore size is the same number either way.
    #[test]
    fn fullscreen_is_remembered_as_maximized() {
        let bounds = Bounds {
            origin: Point {
                x: px(0.0),
                y: px(0.0),
            },
            size: Size {
                width: px(1280.0),
                height: px(820.0),
            },
        };
        let from_fullscreen = Geometry::of(gpui_kit::WindowBounds::Fullscreen(bounds));
        assert!(from_fullscreen.maximized);
        assert_eq!(from_fullscreen.width, 1280.0);

        assert!(!Geometry::of(gpui_kit::WindowBounds::Windowed(bounds)).maximized);
    }

    /// What is written is what comes back, on a display that can hold it.
    #[test]
    fn a_round_trip_through_the_settings_file_changes_nothing() {
        let original = Geometry {
            x: -1800.5,
            y: 64.0,
            width: 1444.0,
            height: 900.0,
            maximized: false,
        };
        let text = serde_json::to_string(&original).unwrap();
        let back: Geometry = serde_json::from_str(&text).unwrap();
        assert_eq!(original, back);
    }

    /// An older settings file has no `maximized` key. Reading one must not
    /// fail, and the window should come up windowed.
    #[test]
    fn a_file_written_before_this_existed_still_reads() {
        let older: Geometry =
            serde_json::from_str(r#"{"x":10.0,"y":20.0,"width":1280.0,"height":820.0}"#).unwrap();
        assert!(!older.maximized);
        assert_eq!(older.x, 10.0);
    }
}
