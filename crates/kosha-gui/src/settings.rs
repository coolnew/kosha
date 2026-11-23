// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! What the user has chosen, and where it is kept.
//!
//! Settings live in a JSON file under the platform config directory, hand-
//! editable on purpose: the panel exposes the handful of options most people
//! want, and the file exposes all of them. Anything the analysis engine can be
//! tuned with is reachable without a new control being designed for it first.
//!
//! Loading never fails. A missing file means first run; an unreadable or
//! malformed one means something went wrong that must not stop a user from
//! looking at their disk, so we fall back to defaults and say so. The one thing
//! we do not do is overwrite a file we could not parse — that would turn a typo
//! into data loss.

use std::io::Write;
use std::path::PathBuf;

use kosha_core::ScanOptions;
use kosha_core::format::Units;
use serde::{Deserialize, Serialize};

/// Which units sizes are shown in.
///
/// Mirrored here rather than deriving `serde` on `kosha_core::format::Units`,
/// which would put a serialisation dependency into the scanner for the benefit
/// of one front end.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitPreference {
    /// `GiB`, powers of 1024.
    #[default]
    Binary,
    /// `GB`, powers of 1000.
    Decimal,
}

impl From<UnitPreference> for Units {
    fn from(preference: UnitPreference) -> Units {
        match preference {
            UnitPreference::Binary => Units::Binary,
            UnitPreference::Decimal => Units::Decimal,
        }
    }
}

/// Whether to follow the operating system's appearance or override it.
/// Where the accent colour comes from.
///
/// Only the accent: red still means destructive, amber still means a threshold
/// passed, and the treemap's category colours are a data encoding that nothing
/// is allowed to repaint. See `accent.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccentSource {
    /// Follow the desktop — Windows' accent palette, or the Freedesktop
    /// portal on Linux. Falls back to kosha's own where there is nothing to
    /// ask, which is the usual case on a bare window manager.
    #[default]
    System,
    /// kosha's own blue, regardless of what the desktop is doing.
    Kosha,
}

/// Which theme the window is in.
///
/// Dark by default, and deliberately not `System`. This is a window somebody
/// opens to look at a picture: the treemap is a field of saturated colour, and
/// the tiles hold their hue against the dark canvas in a way they do not
/// against a white one, where every one of them has to be lightened until the
/// six categories start to look alike. `System` is still here and still works
/// — it is one click in the panel — but the picture is the product, and it is
/// better dark.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreference {
    System,
    Light,
    #[default]
    Dark,
}

/// A query somebody wanted to keep.
///
/// The name is the query text when it is saved from the window, because a
/// naming dialog is a wall between "that was useful" and keeping it, and the
/// query is already the most accurate description of itself. The settings file
/// is hand-editable and the field is separate, so anybody who wants
/// "Downloads I forgot about" instead can have it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SavedQuery {
    pub name: String,
    pub query: String,
}

/// A folder somebody wants one click away.
///
/// The sidebar's built-in entries are the ones everybody has — home, downloads,
/// the volumes. A scope is the one *this* person keeps scanning: a projects
/// directory, a media library, the share that keeps filling up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SavedScope {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    pub units: UnitPreference,
    pub theme: ThemePreference,
    pub accent: AccentSource,
    /// Tint the window's neutrals toward the accent's hue.
    ///
    /// Additive: dark stays dark and light stays light. This only shifts the
    /// hue of the greys, never their brightness, so every text colour keeps
    /// exactly the contrast it was designed with. Off by default — it changes
    /// the whole window, and that should be a choice rather than a surprise.
    pub tint_chrome: bool,

    // --- the three panels ---------------------------------------------------
    //
    // Which of them the window shows. All three start open, and all three are
    // remembered: somebody who scans with the treemap full-window wants it
    // full-window tomorrow too.
    //
    // Not the same question as whether a rail *fits*. A window narrow enough
    // drops the rails on its own (see `metrics::rails_for`), and that is a
    // measurement rather than a preference — it must not be written down here,
    // or resizing the window once would silently throw away a choice.
    /// The volumes and pinned folders, on the left.
    pub show_places: bool,
    /// The results table, under the treemap.
    pub show_table: bool,
    /// Show the space-usage graph above the table. Map-only mode always draws it.
    pub show_treemap: bool,
    /// Quick look and the findings, on the right.
    pub show_insights: bool,
    /// Restrict the snapshot rail to the root of the scan on screen.
    pub snapshots_current_only: bool,

    /// Queries kept from the window, newest first.
    pub saved_queries: Vec<SavedQuery>,
    /// Folders pinned to the sidebar, newest first.
    pub saved_scopes: Vec<SavedScope>,
    /// Order, widths and visibility of the results table's columns.
    pub columns: crate::columns::Layout,
    /// Where the window was last left, if anywhere.
    ///
    /// `None` until the first window closes, and again whenever what was saved
    /// turns out to be somewhere the window can no longer go — see
    /// [`crate::geometry::restore`], which is where that is decided.
    ///
    /// This is the one setting that is about the machine rather than the
    /// person, so on Windows it roams with a profile it does not really belong
    /// to. That costs nothing: a position from a different machine's monitors
    /// simply fails the check and the window opens centred.
    pub window: Option<crate::geometry::Geometry>,

    // --- scanning -----------------------------------------------------------
    /// Descend into directories on other devices.
    pub cross_filesystems: bool,
    /// Query the exact on-disk size even where it costs a syscall per file.
    pub exact_allocated: bool,
    /// Pay to learn that a file is hardlinked during a folder walk.
    ///
    /// A cost question, like [`exact_allocated`](Self::exact_allocated), and
    /// hidden on the platform where the answer is free — which is why it is a
    /// `bool` here and an `Option<bool>` in [`ScanOptions`]: off means *wherever
    /// it costs nothing*, not *never*. An MFT scan and every disk-image reader
    /// count a hardlinked file once whether this is on or off, because they are
    /// not paying for it.
    pub dedup_hardlinks: bool,
    /// Read NTFS' Master File Table when kosha is running as administrator.
    ///
    /// Kept as a preference rather than acted on blindly: it needs Windows, an
    /// NTFS volume with a drive letter and an elevated process, and where any
    /// of those is missing the ordinary walk runs and the window says which
    /// one was missing. So leaving it on costs nothing on the days it cannot
    /// apply, which is what makes it a setting worth having rather than a
    /// switch that breaks scans.
    pub read_mft: bool,
    /// Read a WSL distribution's disk image instead of going through the
    /// redirector.
    ///
    /// Kept as a preference for the same reason `read_mft` is: where the path
    /// is not a WSL distribution, or its image cannot be read, the ordinary
    /// scan runs and the window says which. So leaving it on costs nothing on
    /// the days it cannot apply.
    pub read_image: bool,
    /// Record who owns each entry, so the window can say who is using the disk.
    ///
    /// Off by default because on a machine one person uses it answers a
    /// question nobody asked, and because on Windows the ordinary walk pays a
    /// call per entry for it. Free in an MFT scan, and nearly free on Unix.
    pub collect_owners: bool,
    /// Allow opt-in SSH scans. This setting is also read by the CLI.
    pub ssh_access: bool,

    // --- analysis -----------------------------------------------------------
    /// Findings smaller than this are not worth showing.
    pub min_reclaimable_mb: u64,
    pub duplicate_min_mib: u64,
    pub duplicate_budget_gib: u64,
    pub stale_installer_days: u32,
    pub cold_file_days: u32,
    /// Megabytes, to match `min_reclaimable_mb`: a settings file people edit
    /// should not make them count zeroes.
    pub cold_file_mb: u64,
    pub vm_image_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        let scan = ScanOptions::default();
        let analysis = kosha_analyze::Context::at(0);
        Settings {
            units: UnitPreference::default(),
            theme: ThemePreference::default(),
            accent: AccentSource::default(),
            tint_chrome: false,
            show_places: true,
            show_table: true,
            show_treemap: true,
            show_insights: true,
            snapshots_current_only: false,
            saved_queries: Vec::new(),
            saved_scopes: Vec::new(),
            columns: crate::columns::Layout::default(),
            window: None,
            cross_filesystems: scan.cross_filesystems,
            exact_allocated: scan.exact_allocated,
            dedup_hardlinks: scan.dedup_hardlinks.unwrap_or(false),
            read_mft: scan.use_mft,
            read_image: scan.use_image,
            collect_owners: scan.collect_owners,
            ssh_access: false,
            min_reclaimable_mb: analysis.min_reclaimable / (1024 * 1024),
            duplicate_min_mib: 1,
            duplicate_budget_gib: 8,
            stale_installer_days: analysis.stale_installer_days,
            cold_file_days: analysis.cold_file_days,
            cold_file_mb: analysis.cold_file_bytes / (1024 * 1024),
            vm_image_days: analysis.vm_image_days,
        }
    }
}

/// The most queries to keep.
///
/// A cap rather than unbounded, because these are saved with one click and
/// nothing prompts anybody to tidy them. Past this the oldest goes, which is
/// the one least likely to be missed.
pub const MAX_SAVED_QUERIES: usize = 40;

/// The most folders to pin.
///
/// Lower than the query cap because these take a row of the sidebar each, and a
/// rail you have to scroll to reach your volumes is worse than no pinning.
pub const MAX_SAVED_SCOPES: usize = 12;

impl Settings {
    pub fn duplicate_options(&self) -> kosha_analyze::duplicates::DuplicateOptions {
        kosha_analyze::duplicates::DuplicateOptions {
            min_size: self.duplicate_min_mib.saturating_mul(1 << 20),
            max_read_bytes: self.duplicate_budget_gib.saturating_mul(1 << 30),
        }
    }

    pub fn units(&self) -> Units {
        self.units.into()
    }

    /// Keep a query, newest first.
    ///
    /// Saving one already saved moves it back to the front rather than adding
    /// a second copy — the useful behaviour is the same either way, and a list
    /// with the same line in it twice looks broken.
    ///
    /// Returns false when there was nothing to save.
    pub fn save_query(&mut self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty() {
            return false;
        }
        self.saved_queries.retain(|saved| saved.query != query);
        self.saved_queries.insert(
            0,
            SavedQuery {
                name: query.to_string(),
                query: query.to_string(),
            },
        );
        self.saved_queries.truncate(MAX_SAVED_QUERIES);
        true
    }

    pub fn forget_query(&mut self, query: &str) {
        self.saved_queries.retain(|saved| saved.query != query);
    }

    pub fn has_saved_query(&self, query: &str) -> bool {
        let query = query.trim();
        self.saved_queries.iter().any(|saved| saved.query == query)
    }

    /// Pin a folder, newest first.
    ///
    /// Pinning one already pinned moves it to the front rather than adding a
    /// second row for the same path — the sidebar showing one folder twice
    /// looks broken, and the useful behaviour is identical either way.
    ///
    /// Returns false when there was nothing to pin.
    pub fn save_scope(&mut self, path: &std::path::Path) -> bool {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            // A volume root has no file name; its display form is the name
            // anybody would call it by.
            .unwrap_or_else(|| path.display().to_string());
        if name.is_empty() {
            return false;
        }
        self.saved_scopes.retain(|saved| saved.path != path);
        self.saved_scopes.insert(
            0,
            SavedScope {
                name,
                path: path.to_path_buf(),
            },
        );
        self.saved_scopes.truncate(MAX_SAVED_SCOPES);
        true
    }

    pub fn forget_scope(&mut self, path: &std::path::Path) {
        self.saved_scopes.retain(|saved| saved.path != path);
    }

    pub fn has_saved_scope(&self, path: &std::path::Path) -> bool {
        self.saved_scopes.iter().any(|saved| saved.path == path)
    }

    pub fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            cross_filesystems: self.cross_filesystems,
            // Off is `None`, not `Some(false)`: the window is asking whether
            // to pay during a walk, and answering "no" must not also switch it
            // off in the strategies that get it for nothing.
            dedup_hardlinks: self.dedup_hardlinks.then_some(true),
            exact_allocated: self.exact_allocated,
            max_depth: None,
            use_mft: self.read_mft,
            use_image: self.read_image,
            collect_owners: self.collect_owners,
        }
    }

    /// Build an analysis context from these settings and the current clock.
    pub fn analysis_context(&self) -> kosha_analyze::Context {
        kosha_analyze::Context {
            min_reclaimable: self.min_reclaimable_mb.saturating_mul(1024 * 1024),
            stale_installer_days: self.stale_installer_days,
            cold_file_days: self.cold_file_days,
            cold_file_bytes: self.cold_file_mb.saturating_mul(1024 * 1024),
            vm_image_days: self.vm_image_days,
            units: self.units(),
            ..kosha_analyze::Context::detect()
        }
    }
}

/// Which theme to apply when the window opens, if any.
///
/// A pure function of the two inputs so it can be tested without a window, and
/// because the rule is easy to get subtly wrong: the flag documents itself as
/// an override "for this run only", so it has to beat the saved preference,
/// and `System` has to mean *leave the framework alone* rather than force a
/// light theme onto somebody whose desktop is dark.
///
/// The bug this exists to prevent had run since the settings panel was built:
/// the preference was written to the file and applied on change, but never on
/// launch. Choosing dark and restarting put it back to the system theme, so the
/// setting looked like it was not being saved at all.
pub fn startup_theme(
    flag: Option<gpui_kit::component::ThemeMode>,
    preference: ThemePreference,
) -> Option<gpui_kit::component::ThemeMode> {
    use gpui_kit::component::ThemeMode;

    if flag.is_some() {
        return flag;
    }
    match preference {
        ThemePreference::Light => Some(ThemeMode::Light),
        ThemePreference::Dark => Some(ThemeMode::Dark),
        ThemePreference::System => None,
    }
}

/// A loaded settings file, and whether it came from disk.
pub struct Loaded {
    pub settings: Settings,
    /// Set when a file exists but could not be used.
    ///
    /// The window shows it and then keeps it, because it has to gate every
    /// later save: a file somebody hand-edited into something invalid still
    /// holds their saved searches, their pinned folders and their thresholds,
    /// and writing defaults over it destroys all of them. `save` itself does
    /// not know about this — it writes what it is given — so the refusal lives
    /// in `KoshaApp::persist_settings`, which is the only thing that calls it.
    pub problem: Option<String>,
}

/// Where the settings file belongs. See [`kosha_core::dirs`].
pub fn path() -> Option<PathBuf> {
    Some(kosha_core::dirs::config_dir()?.join(FILE))
}

const FILE: &str = "settings.json";

/// Read the settings file, falling back to defaults for anything missing.
pub fn load() -> Loaded {
    let Some(path) = path() else {
        return Loaded {
            settings: Settings::default(),
            problem: None,
        };
    };
    load_from(&path)
}

pub fn load_from(path: &std::path::Path) -> Loaded {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // Not existing is the normal first-run case, not a problem to report.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                problem: None,
            };
        }
        Err(e) => {
            return Loaded {
                settings: Settings::default(),
                problem: Some(format!("could not read {}: {e}", path.display())),
            };
        }
    };

    match serde_json::from_str::<Settings>(&text) {
        // Repaired rather than trusted. This file is one a user can open and
        // edit, and a column listed twice or missing would render twice or be
        // unreachable — neither of which is worth discarding a whole settings
        // file over, so the layout is fixed up in place and everything else is
        // taken as written.
        Ok(settings) => Loaded {
            settings: Settings {
                columns: settings.columns.sanitised(),
                ..settings
            },
            problem: None,
        },
        Err(e) => Loaded {
            settings: Settings::default(),
            problem: Some(format!("{} is not valid: {e}", path.display())),
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("no writable configuration directory on this system")]
    NoLocation,
    #[error("could not write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn save(settings: &Settings) -> Result<PathBuf, SaveError> {
    let path = path().ok_or(SaveError::NoLocation)?;
    save_to(&path, settings)?;
    Ok(path)
}

pub fn save_to(path: &std::path::Path, settings: &Settings) -> Result<(), SaveError> {
    let io = |source| SaveError::Io {
        path: path.to_path_buf(),
        source,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let mut text = serde_json::to_string_pretty(settings).map_err(|e| io(e.into()))?;
    text.push('\n');

    // Written to a sibling and renamed, so an interrupted save leaves the old
    // file intact rather than a truncated one. Losing a preference is trivial;
    // failing to start because the settings file is half-written is not.
    //
    // **The sibling is named for this save, not for the file.** It was
    // `settings.json.tmp` — one name, and one settings file per user, so two
    // koshas saving at the same moment used it as the same scratch space. One
    // renamed it away and the other's rename found nothing: measured with
    // three writers and 600 saves, 90 of them failed, every one a preference
    // that silently did not stick. The dangerous half never showed itself in
    // 600 tries and is the reason this is not just a retry — both writers
    // truncate and fill the same file, so the winner can publish a settings
    // file that is half one window's and half the other's, which is exactly
    // the half-written file the rename exists to prevent.
    //
    // The process id and a counter, because both are needed: the id separates
    // two koshas, and the counter separates two saves inside one. Truncating
    // rather than `create_new`, because a name that says "this save, in this
    // process" cannot belong to a live writer — so a leftover from a kosha
    // that was killed is something to reuse, not something to refuse over.
    static NTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = path.with_extension(format!("json.writing-{}-{nth}", std::process::id()));

    let write = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        // A rename that beats the data to the disk leaves a settings file of
        // zeros, which is the one outcome this whole dance is here to avoid.
        file.sync_all()
    })();
    match write.and_then(|()| std::fs::rename(&temporary, path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(io(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two windows saving at the same moment.
    ///
    /// There is one settings file per user and every window writes it — a
    /// second window is a second process, and a preference changed in one
    /// while the other is closing is not a contrived situation. The save
    /// wrote through a sibling called `settings.json.tmp` and renamed it into
    /// place, which is the right shape, but the name was the *file's* rather
    /// than the *writer's*: both wrote the same scratch file and both renamed
    /// it, so the loser's rename found nothing there. 90 of 600 saves failed
    /// that way, each one a preference that quietly did not stick.
    ///
    /// Worse and rarer, which is why this asserts on the reader too: the two
    /// writers were filling that one file at the same time, so a rename could
    /// publish something half from each — the half-written settings file the
    /// rename is there to prevent. It never appeared in 600 tries, and a
    /// unique name is the only thing that makes it impossible rather than
    /// unlikely.
    ///
    /// Threads rather than processes because that is what a test can start;
    /// they call the same function against the same path, which is what two
    /// windows do.
    #[test]
    fn duplicate_settings_default_and_round_trip() {
        let legacy: Settings = serde_json::from_str("{}").unwrap();
        let options = legacy.duplicate_options();
        assert_eq!(
            options.min_size,
            kosha_analyze::duplicates::DuplicateOptions::default().min_size
        );
        assert_eq!(
            options.max_read_bytes,
            kosha_analyze::duplicates::DuplicateOptions::default().max_read_bytes
        );

        let edited = Settings {
            duplicate_min_mib: 16,
            duplicate_budget_gib: 32,
            ..legacy
        };
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&edited).unwrap()).unwrap();
        let options = restored.duplicate_options();
        assert_eq!(options.min_size, 16 << 20);
        assert_eq!(options.max_read_bytes, 32 << 30);
    }

    #[test]
    fn two_windows_saving_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut writers = Vec::new();
        for who in 0..3u32 {
            let path = path.clone();
            writers.push(std::thread::spawn(move || {
                let settings = Settings {
                    saved_scopes: (0..40)
                        .map(|n| SavedScope {
                            path: std::path::PathBuf::from(format!("/w{who}/scope{n}")),
                            name: format!("writer {who} scope {n}"),
                        })
                        .collect(),
                    ..Settings::default()
                };
                let mut failed = 0;
                for _ in 0..200 {
                    if save_to(&path, &settings).is_err() {
                        failed += 1;
                    }
                }
                failed
            }));
        }

        let reader = {
            let path = path.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let (mut read, mut bad) = (0u32, 0u32);
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        read += 1;
                        if serde_json::from_str::<Settings>(&text).is_err() {
                            bad += 1;
                        }
                    }
                }
                (read, bad)
            })
        };

        let mut save_failures = 0;
        for w in writers {
            save_failures += w.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (read, bad) = reader.join().unwrap();
        assert_eq!(save_failures, 0, "{save_failures} of 600 saves failed");
        assert_eq!(
            bad, 0,
            "{bad} of {read} reads found a settings file that would not parse"
        );
        // The writers are fast enough that a reader spinning on the file gets
        // thousands of looks; if it got none, this test proved nothing.
        assert!(read > 0, "the reader never saw the file");

        // And nothing is left beside it.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "settings.json")
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    /// The guarantee `Loaded::problem` exists for, stated as a test because
    /// the doc comment claimed it for weeks while nothing implemented it.
    ///
    /// A settings file is a file people are invited to edit, so it can be
    /// hand-edited into something invalid — and it still holds their saved
    /// searches, their pinned folders and their thresholds. `load_from` must
    /// report the problem rather than quietly returning defaults that look
    /// like a fresh install, because the caller's whole reason for refusing to
    /// save is that flag.
    #[test]
    fn an_unreadable_file_is_reported_rather_than_read_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // One unparseable value among several good ones, which is what a
        // hand-edit actually looks like.
        std::fs::write(
            &path,
            r#"{"units":"binary","min-reclaimable-mb":777,"window":{"x":1.0,"y":1.0,"width":"oops","height":2.0}}"#,
        )
        .unwrap();

        let loaded = load_from(&path);
        assert!(
            loaded.problem.is_some(),
            "a file that would not parse was reported as fine, so a save \
             would overwrite it"
        );
        // Defaults stand in so the window still works; the point is that the
        // caller knows not to write them back.
        assert_eq!(loaded.settings, Settings::default());
    }

    /// The window geometry is the one setting the application writes by
    /// itself, on every ordinary close. A value it cannot read must not stop
    /// the rest of the file being understood any differently from any other
    /// bad value — it takes the same path, and the same refusal protects it.
    #[test]
    fn a_good_file_with_geometry_reads_back_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            window: Some(crate::geometry::Geometry {
                x: -1800.0,
                y: 64.0,
                width: 1444.0,
                height: 900.0,
                maximized: true,
            }),
            ..Settings::default()
        };
        save_to(&path, &settings).unwrap();

        let loaded = load_from(&path);
        assert!(loaded.problem.is_none());
        assert_eq!(loaded.settings.window, settings.window);
    }

    /// A settings file written before the window was remembered has no such
    /// key, and must still read.
    #[test]
    fn a_file_from_before_the_window_was_remembered_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"units":"binary","min-reclaimable-mb":777}"#).unwrap();

        let loaded = load_from(&path);
        assert!(loaded.problem.is_none(), "{:?}", loaded.problem);
        assert_eq!(loaded.settings.window, None);
        assert!(loaded.settings.show_treemap);
        assert_eq!(loaded.settings.min_reclaimable_mb, 777);
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");

        let settings = Settings {
            units: UnitPreference::Decimal,
            theme: ThemePreference::Dark,
            show_treemap: false,
            cross_filesystems: true,
            exact_allocated: true,
            min_reclaimable_mb: 7,
            ..Settings::default()
        };
        save_to(&path, &settings).unwrap();

        let loaded = load_from(&path);
        assert!(loaded.problem.is_none());
        assert_eq!(loaded.settings, settings);
    }

    #[test]
    fn saved_queries_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Settings::default();
        settings.save_query("size > 1gb");
        settings.save_query("ext in (mp4, mkv) and age > 1y");
        save_to(&path, &settings).unwrap();

        let loaded = load_from(&path);
        assert!(loaded.problem.is_none());
        assert_eq!(loaded.settings.saved_queries, settings.saved_queries);
        // Newest first.
        assert_eq!(
            loaded.settings.saved_queries[0].query,
            "ext in (mp4, mkv) and age > 1y"
        );
    }

    /// The setting has to reach the scan, or the toggle is a decoration. Both
    /// ways round, because a field defaulted in one direction and read in the
    /// other is exactly the shape of bug a round-trip test misses.
    #[test]
    fn recording_owners_reaches_the_scan_options() {
        let mut settings = Settings::default();
        assert!(!settings.scan_options().collect_owners);
        settings.collect_owners = true;
        assert!(settings.scan_options().collect_owners);
    }

    /// The hardlink toggle is the one setting that is deliberately *not*
    /// symmetrical, and the asymmetry is the whole point of it.
    ///
    /// Off must become `None` — "wherever it is free" — and not `Some(false)`.
    /// The window is asking whether to pay for the answer during a folder walk;
    /// answering no must not also switch it off in a disk-image scan or an MFT
    /// scan, which are not paying. `Some(false)` is `du -l`, which is a thing to
    /// ask for on the command line and not a thing to arrive at by leaving a box
    /// unticked.
    #[test]
    fn leaving_the_hardlink_toggle_off_is_not_the_same_as_asking_for_du_l() {
        let mut settings = Settings::default();
        assert_eq!(settings.scan_options().dedup_hardlinks, None);
        // And `None` still dedups in every strategy that has the answer.
        assert!(settings.scan_options().dedups_hardlinks(true));

        settings.dedup_hardlinks = true;
        assert_eq!(settings.scan_options().dedup_hardlinks, Some(true));
        // Ticked, it dedups even where the walk would have to pay.
        assert!(settings.scan_options().dedups_hardlinks(false));
    }

    #[test]
    fn ssh_access_is_off_for_old_settings_and_round_trips() {
        let old: Settings = serde_json::from_str(r#"{"units":"decimal"}"#).unwrap();
        assert!(!old.ssh_access);
        let enabled = Settings {
            ssh_access: true,
            ..old
        };
        let serialized = serde_json::to_value(&enabled).unwrap();
        assert_eq!(serialized["ssh-access"], true);
        let restored: Settings = serde_json::from_value(serialized).unwrap();
        assert!(restored.ssh_access);
    }

    #[test]
    fn a_settings_file_written_before_owners_existed_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"units":"decimal","read-mft":true}"#).unwrap();

        let loaded = load_from(&path);
        assert!(loaded.problem.is_none(), "{:?}", loaded.problem);
        assert!(loaded.settings.read_mft);
        assert!(
            !loaded.settings.collect_owners,
            "a file that never mentioned owners turned them on"
        );
    }

    #[test]
    fn a_settings_file_written_before_saved_queries_existed_still_loads() {
        // The field is `#[serde(default)]` along with the rest of the struct;
        // an older file has no key for it and must not fail to parse.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"units":"decimal","theme":"dark"}"#).unwrap();

        let loaded = load_from(&path);
        assert!(loaded.problem.is_none(), "{:?}", loaded.problem);
        assert_eq!(loaded.settings.units, UnitPreference::Decimal);
        assert!(loaded.settings.saved_queries.is_empty());
    }

    #[test]
    fn saving_the_same_query_twice_moves_it_rather_than_duplicating_it() {
        let mut settings = Settings::default();
        settings.save_query("size > 1gb");
        settings.save_query("dir and empty");
        assert_eq!(settings.saved_queries.len(), 2);

        settings.save_query("size > 1gb");
        assert_eq!(settings.saved_queries.len(), 2, "a duplicate was kept");
        assert_eq!(settings.saved_queries[0].query, "size > 1gb");
    }

    #[test]
    fn an_empty_query_is_not_saved() {
        let mut settings = Settings::default();
        assert!(!settings.save_query("   "));
        assert!(!settings.save_query(""));
        assert!(settings.saved_queries.is_empty());
    }

    #[test]
    fn a_query_is_matched_however_it_is_spaced() {
        let mut settings = Settings::default();
        settings.save_query("  size > 1gb  ");

        assert_eq!(settings.saved_queries[0].query, "size > 1gb");
        assert!(settings.has_saved_query("size > 1gb"));
        assert!(
            settings.has_saved_query("  size > 1gb "),
            "the star would read as unsaved for the query that saved it"
        );
    }

    #[test]
    fn forgetting_removes_exactly_one() {
        let mut settings = Settings::default();
        settings.save_query("size > 1gb");
        settings.save_query("dir and empty");

        settings.forget_query("size > 1gb");
        assert_eq!(settings.saved_queries.len(), 1);
        assert_eq!(settings.saved_queries[0].query, "dir and empty");

        // Forgetting something absent is not an error.
        settings.forget_query("never saved");
        assert_eq!(settings.saved_queries.len(), 1);
    }

    #[test]
    fn the_list_is_capped_and_drops_the_oldest() {
        let mut settings = Settings::default();
        for n in 0..MAX_SAVED_QUERIES + 5 {
            settings.save_query(&format!("size > {n}mb"));
        }

        assert_eq!(settings.saved_queries.len(), MAX_SAVED_QUERIES);
        // The most recent survived; the first ones did not.
        assert_eq!(
            settings.saved_queries[0].query,
            format!("size > {}mb", MAX_SAVED_QUERIES + 4)
        );
        assert!(!settings.has_saved_query("size > 0mb"));
    }

    #[test]
    fn a_saved_theme_is_applied_on_launch() {
        use gpui_kit::component::ThemeMode;

        assert_eq!(
            startup_theme(None, ThemePreference::Dark),
            Some(ThemeMode::Dark),
            "a saved preference was written to the file and then ignored"
        );
        assert_eq!(
            startup_theme(None, ThemePreference::Light),
            Some(ThemeMode::Light)
        );
    }

    #[test]
    fn following_the_system_means_touching_nothing() {
        assert_eq!(startup_theme(None, ThemePreference::System), None);
    }

    #[test]
    fn the_flag_beats_the_saved_preference() {
        use gpui_kit::component::ThemeMode;

        // `--light` and `--dark` are documented as overriding for one run.
        for preference in [
            ThemePreference::Dark,
            ThemePreference::Light,
            ThemePreference::System,
        ] {
            assert_eq!(
                startup_theme(Some(ThemeMode::Light), preference),
                Some(ThemeMode::Light),
                "--light lost to the saved {preference:?}"
            );
        }
    }

    #[test]
    fn a_pinned_folder_is_named_after_itself() {
        let mut settings = Settings::default();
        assert!(settings.save_scope(std::path::Path::new("/home/me/Projects")));

        assert_eq!(settings.saved_scopes[0].name, "Projects");
        assert!(settings.has_saved_scope(std::path::Path::new("/home/me/Projects")));
        assert!(!settings.has_saved_scope(std::path::Path::new("/home/me")));
    }

    #[test]
    fn a_volume_root_still_gets_a_name() {
        // `file_name` is None for a root, and a nameless row is not a row.
        let mut settings = Settings::default();
        let root = std::path::Path::new("/");
        assert!(settings.save_scope(root));
        assert!(!settings.saved_scopes[0].name.is_empty());
    }

    #[test]
    fn pinning_the_same_folder_twice_moves_it() {
        let mut settings = Settings::default();
        settings.save_scope(std::path::Path::new("/a"));
        settings.save_scope(std::path::Path::new("/b"));
        settings.save_scope(std::path::Path::new("/a"));

        assert_eq!(
            settings.saved_scopes.len(),
            2,
            "the sidebar showed one twice"
        );
        assert_eq!(settings.saved_scopes[0].path, std::path::Path::new("/a"));
    }

    #[test]
    fn pinned_folders_are_capped_and_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Settings::default();
        for n in 0..MAX_SAVED_SCOPES + 3 {
            settings.save_scope(&std::path::PathBuf::from(format!("/scope-{n}")));
        }
        assert_eq!(settings.saved_scopes.len(), MAX_SAVED_SCOPES);

        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.problem.is_none());
        assert_eq!(loaded.settings.saved_scopes, settings.saved_scopes);
    }

    #[test]
    fn unpinning_removes_exactly_one() {
        let mut settings = Settings::default();
        settings.save_scope(std::path::Path::new("/a"));
        settings.save_scope(std::path::Path::new("/b"));

        settings.forget_scope(std::path::Path::new("/a"));
        assert_eq!(settings.saved_scopes.len(), 1);
        assert_eq!(settings.saved_scopes[0].path, std::path::Path::new("/b"));

        // Unpinning something absent is not an error.
        settings.forget_scope(std::path::Path::new("/never"));
        assert_eq!(settings.saved_scopes.len(), 1);
    }

    #[test]
    fn a_missing_file_is_first_run_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_from(&dir.path().join("absent.json"));

        assert_eq!(loaded.settings, Settings::default());
        assert!(
            loaded.problem.is_none(),
            "not having settings yet is not a problem to report"
        );
    }

    #[test]
    fn a_malformed_file_falls_back_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.settings, Settings::default());
        assert!(
            loaded.problem.is_some(),
            "a broken file must be reported, not silently ignored"
        );
    }

    #[test]
    fn unknown_and_missing_keys_are_tolerated() {
        // Forward compatibility both ways: a file written by a newer version
        // must not break an older one, and a file written by an older version
        // must not lose the fields it never knew about.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"units": "decimal", "something-from-the-future": 42}"#,
        )
        .unwrap();

        let loaded = load_from(&path);
        assert!(loaded.problem.is_none(), "{:?}", loaded.problem);
        assert_eq!(loaded.settings.units, UnitPreference::Decimal);
        assert_eq!(
            loaded.settings.cold_file_days,
            Settings::default().cold_file_days,
            "an absent key should take its default"
        );
    }

    #[test]
    fn saving_does_not_leave_a_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save_to(&path, &Settings::default()).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn defaults_match_the_engines_own_defaults() {
        // The settings file is a mirror of what the scanner and the rule engine
        // would do untouched. If they drift, a first run stops matching a
        // default-configured run.
        let settings = Settings::default();
        assert_eq!(settings.scan_options(), ScanOptions::default());

        let context = settings.analysis_context();
        let reference = kosha_analyze::Context::at(context.now);
        assert_eq!(context.min_reclaimable, reference.min_reclaimable);
        assert_eq!(context.stale_installer_days, reference.stale_installer_days);
        assert_eq!(context.cold_file_days, reference.cold_file_days);
        assert_eq!(context.vm_image_days, reference.vm_image_days);
    }
}
