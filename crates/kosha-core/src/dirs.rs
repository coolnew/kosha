// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Where this application keeps its own files.
//!
//! Two of them, and they are different kinds of thing. Settings are
//! configuration: small, hand-editable, and on Windows worth roaming with the
//! user's profile. The audit log is data: append-only, potentially large, and
//! machine-local. Every platform draws that line somewhere and they do not
//! agree on where, which is the whole reason this module exists.
//!
//! It lives in `kosha-core` because both `kosha-gui` and `kosha-fsops` need it
//! and the answer must not be able to differ between them. Two copies of a
//! directory name is two chances for one of them to be wrong, and the one that
//! is wrong is the one nobody notices — it just quietly reads an empty file.
//!
//! ## The name
//!
//! `kosha` publishes at `kosha.coolnew.app`, and the namespace follows from
//! that. What the namespace *looks like* does not: each platform spells this
//! its own way, and the convention is not decoration but the thing that makes
//! a directory findable by somebody who goes looking.
//!
//! | | Configuration | Data |
//! | --- | --- | --- |
//! | Windows | `%APPDATA%\coolnew\kosha` | `%LOCALAPPDATA%\coolnew\kosha` |
//! | macOS | `~/Library/Application Support/app.coolnew.kosha` | the same |
//! | Linux | `$XDG_CONFIG_HOME/kosha` | `$XDG_DATA_HOME/kosha` |
//!
//! Windows namespaces by `Vendor\Product`: `%APPDATA%` holds `Microsoft`,
//! `JetBrains`, `npm`. A folder named `app.coolnew.kosha` there would be a
//! reverse-DNS identifier in a place that has never used one.
//!
//! macOS is the opposite: reverse-DNS *is* the convention, because it is what
//! a bundle identifier is, and `~/Library/Application Support` is full of them.
//!
//! Linux keeps the bare name. XDG directories are `~/.config/nvim`,
//! `~/.config/git` — the vendor is not part of the path, and adding one would
//! make kosha the odd entry in the listing.

use std::path::PathBuf;

/// The vendor segment, used only where a platform namespaces by vendor.
const VENDOR: &str = "coolnew";
/// The bare application name, used where a platform namespaces by name alone.
const APP: &str = "kosha";
/// The reverse-DNS identifier, from the `kosha.coolnew.app` domain.
///
/// macOS uses it as the directory name, because that is what a bundle
/// identifier is. Wayland wants the same string as the window's `app_id`, so
/// the compositor can match the window to `app.coolnew.kosha.desktop` and find
/// its icon. Two uses, one string, and they must not drift apart — which is
/// why this is public rather than a second copy in the GUI.
pub const BUNDLE_ID: &str = "app.coolnew.kosha";

/// Where configuration belongs: small, hand-edited, roaming where that exists.
pub fn config_dir() -> Option<PathBuf> {
    Some(config_base()?.join(leaf()))
}

/// Where application data belongs: append-only, possibly large, never roaming.
pub fn data_dir() -> Option<PathBuf> {
    Some(data_base()?.join(leaf()))
}

/// The last segment: what this application calls itself on this platform.
fn leaf() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(VENDOR).join(APP)
    } else if cfg!(target_os = "macos") {
        PathBuf::from(BUNDLE_ID)
    } else {
        PathBuf::from(APP)
    }
}

fn config_base() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home().map(|h| h.join(".config")))
    }
}

fn data_base() -> Option<PathBuf> {
    if cfg!(windows) {
        // Local rather than roaming: an append-only log of everything ever
        // deleted is not something to copy between machines at login.
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        // macOS has no separate data directory for this; Application Support
        // is where both belong, and pretending otherwise would put kosha's
        // files somewhere no other Mac application looks.
        home().map(|h| h.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home().map(|h| h.join(".local/share")))
    }
}

fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment decides these, and a machine with none of the variables
    /// set is a machine where `None` is the honest answer rather than a guess
    /// at somebody's home directory.
    #[test]
    fn a_directory_is_either_found_or_admitted_missing() {
        for dir in [config_dir(), data_dir()].into_iter().flatten() {
            assert!(dir.is_absolute(), "{} is not absolute", dir.display());
        }
    }

    /// Configuration and data are different kinds of thing and the platforms
    /// that separate them must keep them separate. macOS does not separate
    /// them, and that is its convention rather than an oversight.
    #[test]
    fn config_and_data_are_only_the_same_place_on_macos() {
        let (Some(config), Some(data)) = (config_dir(), data_dir()) else {
            return; // Nothing set in this environment; nothing to compare.
        };
        if cfg!(target_os = "macos") {
            assert_eq!(config, data);
        } else {
            assert_ne!(
                config, data,
                "an append-only log does not belong beside a settings file"
            );
        }
    }

    #[test]
    fn the_name_follows_the_platform_rather_than_one_house_style() {
        let Some(dir) = config_dir() else { return };
        let text = dir.to_string_lossy().replace('\\', "/");

        if cfg!(windows) {
            assert!(text.ends_with("coolnew/kosha"), "{text}");
        } else if cfg!(target_os = "macos") {
            assert!(text.ends_with(BUNDLE_ID), "{text}");
        } else {
            assert!(text.ends_with("/kosha"), "{text}");
            assert!(
                !text.contains("coolnew"),
                "XDG directories carry the name alone: {text}"
            );
        }
    }
}
