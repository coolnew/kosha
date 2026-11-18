// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Taking the desktop's accent colour and making it usable.
//!
//! Windows 11 and modern Linux desktops both publish an accent colour so that
//! applications can look like they belong. Adopting it is most of what makes an
//! app feel native rather than ported.
//!
//! Two things stop this from being a one-line change.
//!
//! **Contrast is not guaranteed.** A desktop accent is chosen against a
//! wallpaper, not against our canvas. Windows knows this and ships eight
//! shades; Linux hands over one colour and no guidance. Either way the colour
//! that arrives may be unreadable on the surface we would draw it on, so it is
//! adjusted until it is not. [`usable`] is that adjustment, and it is a pure
//! function so the guarantee can be tested rather than eyeballed.
//!
//! **Colour means things here.** Red is destructive — and read-write mode,
//! which is the mode in which destroying is possible — amber is a threshold,
//! and the six category colours are a data encoding the treemap legend depends
//! on. None of those follow the accent. Only chrome does — selection, focus, a
//! primary button, the query status line — which is what the palette's own doc
//! comment has said since it was written: *colour is reserved for data, so that
//! when something is coloured it means something.*
//!
//! The consequence is worth stating plainly: somebody whose desktop accent is
//! red gets a red *selection* and a red *destructive button*, which are then
//! harder to tell apart. That is why destructive actions never rely on colour
//! alone — they carry their own words, a border, and a confirmation dialog that
//! names what will happen.

/// A colour as the desktop reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// From `0x00RRGGBB`.
    pub const fn from_u32(packed: u32) -> Self {
        Rgb {
            r: ((packed >> 16) & 0xFF) as u8,
            g: ((packed >> 8) & 0xFF) as u8,
            b: (packed & 0xFF) as u8,
        }
    }

    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }
}

/// WCAG relative luminance.
///
/// The sRGB transfer function matters here: averaging the raw channels instead
/// would call a saturated blue and a saturated yellow equally bright, and they
/// are nowhere near.
fn luminance(c: Rgb) -> f32 {
    fn channel(v: u8) -> f32 {
        let v = f32::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

/// WCAG contrast ratio, 1.0 (identical) to 21.0 (black on white).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// The ratio an accent has to reach against the surface behind it.
///
/// 4.5:1 is WCAG AA for body text. The accent is used as text — the query
/// status line, the "showing duplicates" label — so the text threshold is the
/// one that binds, not the looser 3:1 for large shapes.
pub const MIN_CONTRAST: f32 = 4.5;

/// Adjust an accent until it is readable on `background`, keeping its hue.
///
/// Lightness is moved away from the background one step at a time and the hue
/// is left alone, because the hue is the whole point: somebody with a magenta
/// desktop wants a magenta app, not a blue one that happened to pass a contrast
/// check. Saturation is floored so a nearly-grey accent still reads as a
/// colour rather than as another shade of chrome.
///
/// Always terminates: pushed far enough, every hue reaches white or black, and
/// both clear 4.5:1 against any background that is itself readable.
pub fn usable(accent: Rgb, background: Rgb) -> Rgb {
    let (h, s, l) = to_hsl(accent);
    let s = s.max(0.35);

    // Away from the background: lighter on a dark surface, darker on a light
    // one. Going the other way would technically reach contrast too, at the
    // far end, having passed through every unreadable value in between.
    let toward_light = luminance(background) < 0.5;

    let mut best = from_hsl(h, s, l);
    if contrast(best, background) >= MIN_CONTRAST {
        return best;
    }

    // 1% steps: fine enough that the result still looks like the colour asked
    // for, coarse enough to settle in well under a hundred iterations.
    let mut step = 0.0;
    while step <= 1.0 {
        step += 0.01;
        let moved = if toward_light {
            (l + step).min(1.0)
        } else {
            (l - step).max(0.0)
        };
        best = from_hsl(h, s, moved);
        if contrast(best, background) >= MIN_CONTRAST {
            return best;
        }
    }
    best
}

/// The hue of a colour, for tinting a neutral toward it.
pub fn hue_of(c: Rgb) -> f32 {
    to_hsl(c).0
}

/// How much colour a tinted neutral carries.
///
/// Low on purpose. This is Mica, not a colour wash: enough that a magenta
/// desktop gives a faintly magenta window rather than the same blue-black one
/// every machine gets, and not so much that the chrome starts competing with
/// the treemap for attention.
pub const TINT_STRENGTH: f32 = 0.16;

/// Shift a neutral toward `hue` **without changing how bright it is**.
///
/// This is the whole safety argument for tinting the window. Every text colour
/// in the app was chosen against these neutrals, and their contrast ratios
/// depend only on relative luminance — so a tint that holds luminance fixed
/// preserves every one of those ratios exactly, and nothing else has to be
/// re-checked. Shifting lightness instead would move all of them at once.
///
/// Finding the lightness that matches takes a binary search rather than
/// arithmetic, because luminance is not linear in HSL lightness once the sRGB
/// transfer function is involved. It is monotonic, which is all a bisection
/// needs, and twenty-four steps put it well inside a rounding error.
pub fn tinted(neutral: Rgb, hue: f32, strength: f32) -> Rgb {
    let target = luminance(neutral);
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    let mut best = neutral;
    let mut best_error = f32::INFINITY;

    for _ in 0..24 {
        let mid = f32::midpoint(lo, hi);
        let candidate = from_hsl(hue, strength, mid);
        let found = luminance(candidate);

        // Keep the closest colour *visited*, not wherever the last bisection
        // happened to land. Colours are 8-bit, so near white the representable
        // luminances are about 0.008 apart and the exact target usually falls
        // between two of them; taking the final iterate picked whichever side
        // the search ended on, which was sometimes the further one.
        let error = (found - target).abs();
        if error < best_error {
            best_error = error;
            best = candidate;
        }

        if found < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    best
}

/// Black or white, whichever is readable *on* this accent.
///
/// For the one place the accent is a fill rather than a mark: the primary
/// button. Its label has to clear the same bar the accent itself does.
pub fn text_on(accent: Rgb) -> Rgb {
    let white = Rgb::from_u32(0xFFFFFF);
    // Not pure black: the palette's darkest ink, so a label on a light accent
    // matches the text everywhere else in the window.
    let black = Rgb::from_u32(0x0F1116);
    if contrast(white, accent) >= contrast(black, accent) {
        white
    } else {
        black
    }
}

// -------------------------------------------------------- asking the desktop

/// The desktop's accent colour, if it publishes one.
///
/// `None` where there is nothing to ask — a bare WSL session, a minimal window
/// manager, an older desktop — and the app keeps its own accent. That is a
/// normal outcome rather than a failure, so nothing is reported.
///
/// `dark` picks between the light and dark variants where the platform ships
/// both, which Windows does and Linux does not.
pub fn system(dark: bool) -> Option<Rgb> {
    platform::system(dark)
}

#[cfg(windows)]
mod platform {
    use super::Rgb;

    /// Windows publishes eight shades, ordered light to dark:
    /// `AccentLight3, AccentLight2, AccentLight1, Accent, AccentDark1, …`
    ///
    /// Taking the shade Windows itself uses for the mode is more native than
    /// deriving one: `AccentLight2` on dark chrome, `AccentDark1` on light.
    /// [`super::usable`] still runs afterwards, as a floor rather than a
    /// correction — on a normally configured desktop it changes nothing.
    const LIGHT_VARIANT_FOR_DARK_UI: usize = 1;
    const DARK_VARIANT_FOR_LIGHT_UI: usize = 4;

    pub fn system(dark: bool) -> Option<Rgb> {
        let palette = accent_palette()?;
        let index = if dark {
            LIGHT_VARIANT_FOR_DARK_UI
        } else {
            DARK_VARIANT_FOR_LIGHT_UI
        };
        let at = index * 4;
        // Stored BGRA, one shade per four bytes.
        palette
            .get(at..at + 3)
            .map(|bgr| Rgb::new(bgr[2], bgr[1], bgr[0]))
    }

    /// `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Accent`,
    /// value `AccentPalette`: 32 bytes.
    fn accent_palette() -> Option<Vec<u8>> {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW,
        };

        let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let name: Vec<u16> = "AccentPalette"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut buf = [0u8; 32];
        let mut len = buf.len() as u32;
        // SAFETY: both strings are NUL-terminated and outlive the call; `buf`
        // and `len` are a matched pair, and the API writes at most `len` bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_BINARY,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &raw mut len,
            )
        };
        // ERROR_SUCCESS. Anything else — no such value on an older Windows, a
        // policy-locked hive — means we simply do not know.
        (status == 0 && len as usize >= 32).then(|| buf.to_vec())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Rgb;
    use std::process::Command;

    /// The Freedesktop portal is the only interface both GNOME and KDE agree
    /// on, and it is asked through `gdbus` rather than a D-Bus crate.
    ///
    /// That is a deliberate trade. A real client means an async runtime and a
    /// sizeable dependency for one string read at startup; `gdbus` ships with
    /// glib, which is present anywhere a portal is. If it is missing, so is the
    /// portal, and the answer is `None` either way.
    ///
    /// `dark` is unused: Linux publishes one accent and no variants.
    pub fn system(_dark: bool) -> Option<Rgb> {
        portal().or_else(gnome_named)
    }

    fn portal() -> Option<Rgb> {
        let out = Command::new("gdbus")
            .args([
                "call",
                // `gdbus` waits 25 seconds by default. A desktop whose portal
                // is present but wedged would hold the window shut for that
                // long, which is a far worse bug than not knowing the accent.
                // Two seconds is longer than a working portal ever needs.
                "--timeout",
                "2",
                "--session",
                "--dest",
                "org.freedesktop.portal.Desktop",
                "--object-path",
                "/org/freedesktop/portal/desktop",
                "--method",
                "org.freedesktop.portal.Settings.ReadOne",
                "org.freedesktop.appearance",
                "accent-color",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_portal(&String::from_utf8_lossy(&out.stdout))
    }

    /// The reply is `(<<(0.2, 0.4, 0.9)>>,)` — three doubles, 0.0 to 1.0.
    fn parse_portal(reply: &str) -> Option<Rgb> {
        let open = reply.find('(')?;
        let inner = &reply[open..];
        let start = inner.rfind('(')?;
        let end = inner[start..].find(')')? + start;
        let numbers: Vec<f64> = inner[start + 1..end]
            .split(',')
            .filter_map(|part| part.trim().parse::<f64>().ok())
            .collect();
        if numbers.len() != 3 {
            return None;
        }
        let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some(Rgb::new(to8(numbers[0]), to8(numbers[1]), to8(numbers[2])))
    }

    /// GNOME 47 names its accents rather than publishing a colour. These are
    /// the values from its own stylesheet.
    fn gnome_named() -> Option<Rgb> {
        let out = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "accent-color"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        named(
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_matches('\''),
        )
    }

    fn named(name: &str) -> Option<Rgb> {
        Some(Rgb::from_u32(match name {
            "blue" => 0x3584E4,
            "teal" => 0x2190A4,
            "green" => 0x3A944A,
            "yellow" => 0xC88800,
            "orange" => 0xED5B00,
            "red" => 0xE62D42,
            "pink" => 0xD56199,
            "purple" => 0x9141AC,
            "slate" => 0x6F8396,
            _ => return None,
        }))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_portal_reply_is_read() {
            let reply = "(<<(0.20392156862745098, 0.51764705882352946, 0.89411764705882357)>>,)";
            assert_eq!(parse_portal(reply), Some(Rgb::new(52, 132, 228)));
        }

        #[test]
        fn a_reply_that_is_not_a_colour_is_declined() {
            // Better to keep our own accent than to invent one from noise.
            assert_eq!(parse_portal("(<<'prefer-dark'>>,)"), None);
            assert_eq!(parse_portal(""), None);
            assert_eq!(parse_portal("(<<(0.1, 0.2)>>,)"), None);
        }

        #[test]
        fn gnome_accent_names_map_to_its_own_colours() {
            assert_eq!(named("blue"), Some(Rgb::from_u32(0x3584E4)));
            assert_eq!(named("purple"), Some(Rgb::from_u32(0x9141AC)));
            assert_eq!(named("chartreuse"), None);
        }
    }
}

// ------------------------------------------------------------------ HSL

fn to_hsl(c: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(c.r) / 255.0,
        f32::from(c.g) / 255.0,
        f32::from(c.b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;

    if d.abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> Rgb {
    if s <= 0.0 {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return Rgb::new(v, v, v);
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to8 = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb::new(to8(r), to8(g), to8(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two canvases the app draws on. `theme.rs` owns these; they are
    /// repeated here only so the contrast tests have something concrete to
    /// measure against, and a test there asserts they still match.
    const DARK_CANVAS: Rgb = Rgb::from_u32(0x0E1014);
    const LIGHT_CANVAS: Rgb = Rgb::from_u32(0xFAFAFB);

    #[test]
    fn contrast_matches_the_known_extremes() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!((contrast(black, white) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.01);
        // Symmetric, by definition.
        assert!((contrast(black, white) - contrast(white, black)).abs() < 0.001);
    }

    /// The guarantee, over every hue and a spread of lightnesses.
    ///
    /// A desktop accent is picked against a wallpaper and owes us nothing. This
    /// is the test that says an unreadable one cannot reach the screen.
    #[test]
    fn any_accent_becomes_readable_on_either_canvas() {
        let mut checked = 0;
        for hue in (0..360).step_by(5) {
            for lightness in [0.05_f32, 0.2, 0.35, 0.5, 0.65, 0.8, 0.95] {
                for saturation in [0.0_f32, 0.3, 0.6, 1.0] {
                    let accent = from_hsl(hue as f32, saturation, lightness);
                    for canvas in [DARK_CANVAS, LIGHT_CANVAS] {
                        let fixed = usable(accent, canvas);
                        let ratio = contrast(fixed, canvas);
                        assert!(
                            ratio >= MIN_CONTRAST - 0.01,
                            "hue {hue} s {saturation} l {lightness} on {canvas:?} \
                             settled at {ratio:.2}:1"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 4000, "only {checked} combinations covered");
    }

    #[test]
    fn a_readable_accent_is_left_alone() {
        // Our own blue already clears the bar on the dark canvas, so adopting
        // the system accent must not disturb an accent that was already fine.
        let ours = Rgb::from_u32(0x4C8DFF);
        assert!(contrast(ours, DARK_CANVAS) >= MIN_CONTRAST);
        assert_eq!(usable(ours, DARK_CANVAS), ours);
    }

    #[test]
    fn the_hue_survives_the_adjustment() {
        // The point of adopting the desktop's accent is the hue. A correction
        // that reached contrast by drifting to another colour would defeat it.
        for hue in (0..360).step_by(15) {
            let accent = from_hsl(hue as f32, 0.8, 0.5);
            for canvas in [DARK_CANVAS, LIGHT_CANVAS] {
                let (before, _, _) = to_hsl(accent);
                let (after, _, _) = to_hsl(usable(accent, canvas));
                let drift = (after - before).abs().min(360.0 - (after - before).abs());
                assert!(drift < 6.0, "hue moved {drift:.1} degrees at {hue}");
            }
        }
    }

    #[test]
    fn a_grey_accent_still_reads_as_a_colour() {
        // A desktop set to a neutral accent would otherwise give us another
        // shade of chrome, and the selection would stop being visible as
        // anything but a slightly different grey.
        let grey = Rgb::new(0x80, 0x80, 0x80);
        let fixed = usable(grey, DARK_CANVAS);
        let (_, saturation, _) = to_hsl(fixed);
        assert!(saturation >= 0.3, "settled at saturation {saturation:.2}");
    }

    #[test]
    fn a_button_label_is_readable_on_its_own_button() {
        for hue in (0..360).step_by(10) {
            for lightness in [0.15_f32, 0.4, 0.6, 0.85] {
                let accent = from_hsl(hue as f32, 0.7, lightness);
                let label = text_on(accent);
                assert!(
                    contrast(label, accent) >= 4.0,
                    "hue {hue} l {lightness}: label at {:.2}:1",
                    contrast(label, accent)
                );
            }
        }
    }

    /// The invariant the whole feature rests on.
    ///
    /// Every text colour in the app was picked against these neutrals. If a
    /// tint holds their luminance, every one of those contrast ratios survives
    /// untouched and none of them need re-checking.
    #[test]
    fn tinting_never_changes_how_bright_a_neutral_is() {
        let neutrals = [
            Rgb::from_u32(0x0E1014), // canvas, dark
            Rgb::from_u32(0x15181E), // surface
            Rgb::from_u32(0x252A34), // hairline
            Rgb::from_u32(0xFAFAFB), // canvas, light
            Rgb::from_u32(0xFFFFFF),
            Rgb::from_u32(0xE6E8EC),
        ];
        for neutral in neutrals {
            for hue in (0..360).step_by(5) {
                let out = tinted(neutral, hue as f32, TINT_STRENGTH);
                let before = luminance(neutral);
                let after = luminance(out);
                // Half a step of 8-bit colour. Near white the representable
                // luminances are about 0.008 apart, so no exact match exists
                // and landing on the nearer neighbour is the best available.
                assert!(
                    (before - after).abs() < 0.005,
                    "hue {hue} moved {:06X} from luminance {before:.4} to {after:.4}",
                    neutral.to_u32()
                );
            }
        }
    }

    /// The consequence, stated in the terms that actually matter.
    #[test]
    fn text_stays_exactly_as_readable_on_a_tinted_canvas() {
        let text = Rgb::from_u32(0xE8EAF0);
        let faint = Rgb::from_u32(0x646C7E);
        let canvas = DARK_CANVAS;

        for hue in (0..360).step_by(10) {
            let tinted_canvas = tinted(canvas, hue as f32, TINT_STRENGTH);
            for ink in [text, faint] {
                let before = contrast(ink, canvas);
                let after = contrast(ink, tinted_canvas);
                assert!(
                    (before - after).abs() < 0.05,
                    "hue {hue}: contrast moved from {before:.2} to {after:.2}"
                );
            }
        }
    }

    #[test]
    fn a_tint_is_visible_but_still_a_tint() {
        let canvas = DARK_CANVAS;
        let out = tinted(canvas, 300.0, TINT_STRENGTH);
        assert_ne!(out, canvas, "the tint did nothing at all");

        let (hue, saturation, _) = to_hsl(out);
        assert!((hue - 300.0).abs() < 6.0, "settled on hue {hue}");
        assert!(
            saturation <= 0.25,
            "a chrome tint should stay quiet: saturation {saturation}"
        );
    }

    #[test]
    fn hsl_round_trips() {
        for packed in [0x4C8DFF, 0xA73A8E, 0x000000, 0xFFFFFF, 0x808080, 0x35C4D4] {
            let c = Rgb::from_u32(packed);
            let (h, s, l) = to_hsl(c);
            let back = from_hsl(h, s, l);
            let apart = (i32::from(c.r) - i32::from(back.r)).abs()
                + (i32::from(c.g) - i32::from(back.g)).abs()
                + (i32::from(c.b) - i32::from(back.b)).abs();
            assert!(
                apart <= 3,
                "{packed:06X} came back as {:06X}",
                back.to_u32()
            );
        }
    }

    #[test]
    fn packing_round_trips() {
        let c = Rgb::from_u32(0xA73A8E);
        assert_eq!((c.r, c.g, c.b), (0xA7, 0x3A, 0x8E));
        assert_eq!(c.to_u32(), 0xA73A8E);
    }
}
