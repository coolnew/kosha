// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! kosha's own icons, served beside the component library's.
//!
//! The window needs three things drawn as icons: the settings gear, the theme
//! toggle, and the minimise/maximise/close controls the component library
//! draws for itself. Two of those come from the shipped icon set — `sun` and
//! `moon` are exactly what a theme toggle should be, and reusing them keeps
//! kosha's window looking like every other application on the desktop.
//!
//! The gear does not. The set's gear is the familiar eight-lobed cog, which is
//! what every settings button in the world already is; kosha's is a plain ring
//! with six short spokes, which reads as the same thing from a metre away
//! while being recognisably *this* application's. It is the only icon here,
//! and it lives in `assets/icons/` beside the others.
//!
//! An asset source is a lookup from path to bytes, and GPUI takes exactly one
//! per application. So this one answers for kosha's files and hands everything
//! else to the library's, rather than kosha having to choose between its own
//! icons and the ones its window controls are drawn from.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// The settings gear. Named rather than spelled out at the call site: the
/// string is a path into this source and a typo in it is a silently invisible
/// button, not a compile error.
pub const GEAR: &str = "icons/kosha-gear.svg";

/// Every icon kosha brings of its own, embedded so the binary is self-contained.
const OWN: [(&str, &str); 1] = [(GEAR, include_str!("../assets/icons/gear.svg"))];

/// kosha's icons, then the component library's.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, body)) = OWN.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(body.as_bytes())));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut found = gpui_kit::assets::Assets.list(path)?;
        found.extend(
            OWN.iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn koshas_own_icons_are_served() {
        let bytes = Assets
            .load(GEAR)
            .expect("the gear is embedded")
            .expect("the gear is embedded");
        assert!(bytes.starts_with(b"<svg"));
    }

    /// The reason this source exists at all: adding kosha's icons must not
    /// take away the library's, because the window's own minimise, maximise
    /// and close controls are drawn from those.
    #[test]
    fn the_librarys_icons_still_are() {
        for path in ["icons/window-close.svg", "icons/sun.svg", "icons/moon.svg"] {
            let bytes = Assets
                .load(path)
                .unwrap_or_else(|error| panic!("{path}: {error}"))
                .unwrap_or_else(|| panic!("{path}: no bytes"));
            assert!(bytes.starts_with(b"<svg"), "{path} is not an SVG");
        }
    }

    #[test]
    fn listing_finds_both() {
        let found = Assets.list("icons/").expect("the icon directory lists");
        assert!(found.iter().any(|name| name.as_ref() == GEAR));
        assert!(found.iter().any(|name| name.as_ref() == "icons/sun.svg"));
    }
}
