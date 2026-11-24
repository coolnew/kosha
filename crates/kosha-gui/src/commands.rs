// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The command palette: every primary action, reachable by typing a word for it.
//!
//! The app must be fully usable without a mouse. Bindings alone do not
//! deliver that — they are only
//! usable by somebody who already knows them, and there is nowhere to find out
//! what they are. `⌘/Ctrl+K` is that place.
//!
//! Three kinds of thing are in here, in this order:
//!
//! - **Actions** — rescan, go up, find duplicates, switch mode.
//! - **Places** — the same locations the sidebar offers, so scanning somewhere
//!   is one gesture rather than reaching for the rail.
//! - **Queries** — the help catalogue's recipes, which are complete runnable
//!   queries. Someone who wants "the big files" should not have to know that
//!   this is a search rather than a command.
//!
//! Matching is `help::match_score`, the same function the help panel searches
//! with. Looking something up by a word other than its name is one question,
//! and answering it two different ways in one window would mean `⌘K` and the
//! help panel disagreeing about what "dupes" finds.
//!
//! Building the list is a pure function of the situation, so what the palette
//! offers can be tested without a window — including the part that matters
//! most, which is that it never offers something inert. A palette listing "Go
//! up one level" at the root of a tree teaches people to distrust it.

use std::path::PathBuf;

use gpui_kit::base::{Input, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use kosha_query::{TopicKind, help};

use crate::app::KoshaApp;
use crate::columns::{Column, Layout as ColumnLayout};
use crate::theme::{Palette, metrics};
use crate::ui;

/// The most palette rows to rank and show at once.
///
/// The whole table is a few dozen entries, so this is not a performance guard;
/// it is a promise that the list stays scannable rather than something you
/// scroll through looking for the thing you already named.
pub const LIMIT: usize = 40;

/// The modifier this platform writes bindings with.
///
/// Public because the welcome screen names this binding too, and a window that
/// writes it one way here and another way there gets one of them wrong on
/// every machine.
#[cfg(target_os = "macos")]
pub const MOD: &str = "\u{2318}";
#[cfg(not(target_os = "macos"))]
pub const MOD: &str = "Ctrl+";

/// What running a command does.
///
/// Data rather than a closure: a table of closures cannot be compared, printed
/// or tested, and the effectful half belongs in one place in `app.rs` where the
/// window and the context are already in hand.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Scan(PathBuf),
    /// Ask for a folder, then scan it.
    ///
    /// The window could reach the handful of places in the sidebar and nothing
    /// else — no picker, no drag and drop — so a folder that was not one of
    /// them could only be scanned by starting kosha again with it on the
    /// command line.
    ChooseFolder,
    /// Ask for a disk image, then read it.
    ///
    /// Separate from [`ChooseFolder`](Action::ChooseFolder) because a native
    /// picker is one or the other: on Windows asking for directories sets
    /// `FOS_PICKFOLDERS`, which is a folder picker and shows no files at all.
    /// Two offers that each do one thing beats one that does neither.
    ChooseImage,
    ImportListing,
    #[cfg(feature = "remote-client")]
    ScanSsh,
    InstallRecipePack,
    SyncMarketplace,
    DownloadRecipePack(String),
    MarketplaceUnavailable(String),
    RemoveRecipePack {
        id: String,
        title: String,
    },
    Rescan,
    GoUp,
    OpenSearch,
    RunQuery(&'static str),
    /// A query the user kept. Owned, unlike the catalogue's static recipes.
    RunSaved(String),
    SaveQuery(String),
    ForgetQuery(String),
    Export(kosha_core::export::Format),
    ExportComparison(crate::comparison_export::Format),
    PinScope(PathBuf),
    UnpinScope(PathBuf),
    OpenHelp,
    CleanupGuide,
    AskAi,
    /// Show the log of everything this application has changed.
    ///
    /// The safety model calls the audit log one of its layers and says it is
    /// "openable from the UI"; the README repeats it. Nothing opened it, and
    /// the only mention of it anywhere in this crate was the line that writes
    /// to it — so the answer to "what did this thing do to my machine last
    /// Tuesday" was a file the user had to know the name of.
    ShowAuditLog,
    FindDuplicates,
    CheckFragmentation,
    CancelFragmentation,
    DeepScanFragmentation,
    CancelDeepScan,
    HideDeepLayout,
    StopDuplicates,
    /// Stop a hunt that is still reading file contents.
    StopHunt,
    /// Keep the tree on screen, so a later scan can be compared with it.
    TakeSnapshot,
    /// Hold the most recent snapshot of this folder against what is on screen.
    CompareWithLatest(std::path::PathBuf),
    /// Put a comparison away.
    CloseComparison,
    /// Leave the snapshot and scan the folder it was taken of.
    ScanAgainForReal(std::path::PathBuf),
    ToggleSettings,
    /// Open the panel that adds up disk use per account.
    ShowOwners,
    ToggleTheme,
    /// Show or hide quick look for the selected entry.
    ToggleDetails,
    /// Move a table column one place towards or away from the names.
    MoveColumn(Column, Direction),
    /// Show or hide a table column.
    SetColumnHidden(Column, bool),
    ResetColumns,
    ToggleMode,
    /// Start kosha again with an elevated token, and close this window.
    RestartAsAdministrator,
    Quit,
}

/// Which way a column moves.
///
/// Named rather than a `bool`, because `MoveColumn(Items, true)` at a call site
/// says nothing about which way it goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Left,
    Right,
}

/// One offer in the palette.
pub struct Command {
    pub label: String,
    /// The line under the label: a path, a query, or what the action will do.
    pub detail: String,
    /// Shown right-aligned, so the palette is where bindings are learnt.
    pub binding: Option<String>,
    /// Words somebody might type instead of the label.
    pub keywords: &'static [&'static str],
    pub action: Action,
}

pub(crate) fn download_recipe_pack(pack: &kosha_search::RecipePackMetadata) -> Command {
    Command {
        label: format!("Get marketplace pack {}", pack.title),
        detail: format!(
            "{} · {} · {} · {}",
            pack.qualified_id(),
            pack.version,
            pack.description,
            pack.tags.join(", ")
        ),
        binding: None,
        keywords: &["marketplace recipe insight packs", "download"],
        action: Action::DownloadRecipePack(pack.qualified_id()),
    }
}

pub(crate) fn marketplace_entry(entry: &kosha_marketplace::catalog::Entry) -> Command {
    if let Some(reason) = entry.unavailable_reason() {
        let pack = &entry.metadata;
        Command {
            label: format!("Unavailable package {}", pack.title),
            detail: format!(
                "{} ? {} ? {}",
                pack.qualified_id(),
                reason,
                pack.description
            ),
            binding: None,
            keywords: &["marketplace", "packages", "unavailable"],
            action: Action::MarketplaceUnavailable(format!("{}: {reason}", pack.qualified_id())),
        }
    } else {
        download_recipe_pack(&entry.metadata)
    }
}

pub(crate) fn remove_recipe_pack(pack: &kosha_marketplace::InstalledPackage) -> Command {
    Command {
        label: format!("Remove {} {}", pack.kind.replace('-', " "), pack.title),
        detail: format!("{} · version {}", pack.key(), pack.version),
        binding: None,
        keywords: &["installed recipe packs", "marketplace", "uninstall"],
        action: Action::RemoveRecipePack {
            id: pack.key(),
            title: pack.title.clone(),
        },
    }
}

/// Everything the palette needs to know about the app to decide what to offer.
///
/// A plain struct of answers rather than a borrow of the app, so `all` stays a
/// pure function and the tests do not need a window.
// Not `Eq`: the column layout carries widths, and a width is a float.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Situation {
    pub have_tree: bool,
    /// The tree on screen came out of a snapshot rather than off a disk.
    ///
    /// Changes what several rows mean and removes one of them: there is nothing
    /// to rescan when what is on screen is a record, and nothing to snapshot
    /// when it already is one.
    pub viewing_snapshot: bool,
    /// Original date of an automatic drive record that may be kept explicitly.
    pub automatic_snapshot_date: Option<String>,
    /// How many snapshots are kept, so the palette can offer to open one
    /// without being handed the list.
    pub snapshots_kept: usize,
    /// The newest snapshot of the folder on screen, if there is one that could
    /// be compared against it. Carried as the path because the command has to
    /// name which one it means.
    pub comparable: Option<std::path::PathBuf>,
    /// A comparison is on screen, so the offer is to put it away.
    pub showing_comparison: bool,
    /// The root of whatever is on screen, for the row that goes back to it.
    pub root: String,
    /// False at the root of the scan: there is nowhere above it to go.
    pub can_go_up: bool,
    pub showing_duplicates: bool,
    pub finding_duplicates: bool,
    pub read_write: bool,
    /// This process holds an elevated token, so the MFT strategy is available.
    pub elevated: bool,
    /// Restarting elevated is something this platform can actually do.
    pub can_elevate: bool,
    pub dark: bool,
    /// Whatever is in the query box. Empty when there is nothing to keep.
    ///
    /// The text itself rather than a "there is one" flag, because the command
    /// that keeps it has to carry it: a row labelled "Save this search" that
    /// does not know which search is a row that will save the wrong thing the
    /// first time the two get out of step.
    pub query: String,
    /// That query is already kept, so the offer is to forget it instead.
    pub query_is_saved: bool,
    /// The directory currently in view, which is what pinning acts on.
    ///
    /// The folder being *looked at*, not the scan root: somebody four levels
    /// into a project pins the level they are standing on, and offering to pin
    /// the root instead would pin the wrong thing without saying so.
    pub folder: Option<PathBuf>,
    pub folder_pinned: bool,
    /// The table is showing results or duplicates rather than a directory, so
    /// an export would cover those rows rather than the whole tree.
    pub showing_rows: bool,
    /// Something is selected, so there is an entry for quick look to describe.
    pub has_selection: bool,
    pub details_open: bool,
    /// The table's current column layout, which decides which of the column
    /// commands would actually do something.
    pub columns: ColumnLayout,
}

/// Every command worth offering right now.
///
/// Only what can actually be done: no disabled rows, no entries that do
/// nothing when chosen. Everything returned here is safe to run.
///
/// `distributions` and `volumes` are places too — the WSL ones and the mounted
/// drives — and are separate parameters rather than part of `places` because
/// joining the lists would mean an allocation, and this runs on every
/// keystroke.
pub fn all(
    situation: &Situation,
    places: &[crate::places::Place],
    distributions: &[crate::places::Distribution],
    volumes: &[crate::places::Volume],
    saved: &[crate::settings::SavedQuery],
) -> Vec<Command> {
    let mut commands = Vec::with_capacity(48);

    // ------------------------------------------------------------- actions
    //
    // Offered whatever is on screen, unlike almost everything below: these are
    // how somebody reaches a folder or an image the sidebar does not list, so
    // a window with nothing scanned yet is exactly when they are wanted.
    commands.push(Command {
        label: "Scan a folder…".into(),
        detail: "Choose any folder, not only the ones listed".into(),
        binding: Some(format!("{MOD}O")),
        keywords: &["open", "choose", "pick", "browse", "directory", "other"],
        action: Action::ChooseFolder,
    });
    commands.push(Command {
        label: "Scan a disk image…".into(),
        detail: "Look inside an .iso, a .vhdx, a dd, or a file mkfs was run on".into(),
        binding: None,
        keywords: &[
            "image",
            "vhdx",
            "vhd",
            "iso",
            "img",
            "disk",
            "virtual",
            "wsl",
            "ext4",
            "ntfs",
            "xfs",
            "fat",
            "fat32",
            "exfat",
            "efi",
            "iso9660",
            "udf",
            "cd",
            "dvd",
            "bluray",
            "installer",
            "partition",
            "mount",
        ],
        action: Action::ChooseImage,
    });

    commands.push(Command {
        label: "Import a listing…".into(),
        detail: "Preview and save an ncdu, rclone or QDirStat listing".into(),
        binding: None,
        keywords: &["import", "listing", "ncdu", "rclone", "qdirstat"],
        action: Action::ImportListing,
    });

    #[cfg(feature = "remote-client")]
    commands.push(Command {
        label: "Scan a folder over SSH…".into(),
        detail: "Open a read-only record from a remote folder".into(),
        binding: None,
        keywords: &["ssh", "remote", "network", "folder"],
        action: Action::ScanSsh,
    });

    if situation.have_tree {
        commands.push(Command {
            label: "Cleanup guide…".into(),
            detail: "Review development, browser and AI session locations; use the owning tool"
                .into(),
            binding: None,
            keywords: &[
                "persona", "workflow", "cache", "history", "uv", "npm", "browser", "claude",
                "codex",
            ],
            action: Action::CleanupGuide,
        });
        commands.push(Command {
            label: "Ask AI about the selection…".into(),
            detail: "Preview and copy a metadata-only prompt; nothing is sent automatically".into(),
            binding: None,
            keywords: &["assistant", "claude", "codex", "chatgpt", "prompt", "copy"],
            action: Action::AskAi,
        });
        commands.push(Command {
            label: "Rescan this folder".into(),
            detail: "Read it again from disk".into(),
            binding: Some(format!("{MOD}R")),
            keywords: &["refresh", "reload", "again", "update"],
            action: Action::Rescan,
        });
    }

    if situation.can_go_up {
        commands.push(Command {
            label: "Go up one level".into(),
            detail: "Show the parent folder".into(),
            binding: Some("Backspace".into()),
            keywords: &["parent", "back", "out", "above"],
            action: Action::GoUp,
        });
    }

    // Only with something selected: quick look describes an entry, and a
    // panel that opens to say "nothing selected" is the inert row this list
    // exists to avoid.
    if situation.has_selection {
        commands.push(Command {
            label: if situation.details_open {
                "Hide the details".into()
            } else {
                "Show details for the selection".into()
            },
            detail: "Where its bytes go, and what the numbers do not say".into(),
            binding: Some("Space".into()),
            keywords: &[
                "details",
                "quick",
                "look",
                "info",
                "inspect",
                "about",
                "properties",
                "path",
            ],
            action: Action::ToggleDetails,
        });
    }

    if situation.have_tree {
        commands.push(Command {
            label: "Search".into(),
            detail: "Filter this folder, or ask a question about the whole tree".into(),
            binding: Some(format!("{MOD}F")),
            keywords: &["find", "filter", "query", "look", "where"],
            action: Action::OpenSearch,
        });
    }

    // A hunt in flight is the one state where the palette's duplicate entry is
    // neither "find" nor "stop showing" but "stop reading". It used to offer
    // nothing at all here, which left the palette — the place that is supposed
    // to list everything the window can do — silent about the only thing
    // actually happening.
    if situation.finding_duplicates {
        commands.push(Command {
            label: "Stop the duplicate hunt".into(),
            detail: "Keep whatever it has confirmed so far".into(),
            binding: Some("Esc".into()),
            keywords: &["duplicates", "dupes", "stop", "cancel", "abort", "halt"],
            action: Action::StopHunt,
        });
    }

    // Offered whether or not a hunt has run: the one that stops it has to be
    // reachable by the same gesture that started it.
    //
    // Never over a snapshot. A snapshot records how big each file was, not what
    // was in it, so there is nothing in one to hash — the hunt would have to
    // read the disk as it is now and report today's answer under a window that
    // says it is showing the past.
    if situation.have_tree && !situation.finding_duplicates && !situation.viewing_snapshot {
        if situation.showing_duplicates {
            commands.push(Command {
                label: "Stop showing duplicates".into(),
                detail: "Go back to the folder listing".into(),
                binding: None,
                keywords: &["duplicates", "dupes", "copies", "close", "back"],
                action: Action::StopDuplicates,
            });
        } else {
            commands.push(Command {
                label: "Find duplicate files".into(),
                detail: "Read file contents to find identical copies".into(),
                binding: None,
                keywords: &["duplicates", "dupes", "copies", "same", "identical"],
                action: Action::FindDuplicates,
            });
        }
    }

    // Offered only with something to save, and it turns into forgetting once
    // saved: one row that says what will happen, rather than two that leave
    // the user to work out which one applies.
    if !situation.query.trim().is_empty() {
        commands.push(if situation.query_is_saved {
            Command {
                label: "Forget this search".into(),
                detail: situation.query.clone(),
                binding: None,
                keywords: &["forget", "unsave", "remove", "delete", "star"],
                action: Action::ForgetQuery(situation.query.clone()),
            }
        } else {
            Command {
                label: "Save this search".into(),
                detail: situation.query.clone(),
                binding: None,
                keywords: &["save", "keep", "bookmark", "star", "remember"],
                action: Action::SaveQuery(situation.query.clone()),
            }
        });
    }

    // The label says which of the two it will be. "Export…" sitting over a
    // filtered table is the kind of ambiguity somebody only notices after
    // opening the file.
    if situation.have_tree && !situation.showing_comparison {
        for format in [
            kosha_core::export::Format::Txt,
            kosha_core::export::Format::Csv,
            kosha_core::export::Format::Json,
            kosha_core::export::Format::Jsonl,
        ] {
            let extension = format.extension().to_uppercase();
            commands.push(Command {
                label: if situation.showing_rows {
                    format!("Export these results as {extension}\u{2026}")
                } else {
                    format!("Export everything as {extension}\u{2026}")
                },
                detail: if format == kosha_core::export::Format::Txt {
                    "Every matching full path, one per line".into()
                } else {
                    "Every matching row, including results beyond the display limit".into()
                },
                binding: None,
                keywords: &[
                    "export",
                    "save",
                    "csv",
                    "json",
                    "jsonl",
                    "txt",
                    "file",
                    "spreadsheet",
                    "dump",
                ],
                action: Action::Export(format),
            });
        }
    }

    if situation.showing_comparison {
        for format in [
            crate::comparison_export::Format::Markdown,
            crate::comparison_export::Format::Jsonl,
        ] {
            commands.push(Command {
                label: format!("Export comparison as {}…", format.label()),
                detail: "Visible changes, both observations, filters, and comparison warnings"
                    .into(),
                binding: None,
                keywords: &[
                    "export",
                    "comparison",
                    "report",
                    "markdown",
                    "jsonl",
                    "save",
                ],
                action: Action::ExportComparison(format),
            });
        }
    }

    if let Some(folder) = &situation.folder {
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| folder.display().to_string());
        commands.push(if situation.folder_pinned {
            Command {
                label: format!("Unpin {name} from the sidebar"),
                detail: folder.display().to_string(),
                binding: None,
                keywords: &["unpin", "remove", "scope", "sidebar", "forget"],
                action: Action::UnpinScope(folder.clone()),
            }
        } else {
            Command {
                label: format!("Pin {name} to the sidebar"),
                detail: folder.display().to_string(),
                binding: None,
                keywords: &["pin", "keep", "scope", "sidebar", "bookmark", "favourite"],
                action: Action::PinScope(folder.clone()),
            }
        });
    }

    // Comparing, and the way back out of a comparison. Never both: one of
    // them is always the wrong offer, and a palette that lists a row which
    // does nothing is one people learn to distrust.
    if situation.showing_comparison {
        commands.push(Command {
            label: "Stop comparing".into(),
            detail: "Back to the tree".into(),
            binding: Some("Esc".into()),
            keywords: &["compare", "comparison", "changed", "close", "back", "stop"],
            action: Action::CloseComparison,
        });
    } else if let Some(path) = &situation.comparable {
        commands.push(Command {
            label: "What changed since the last snapshot".into(),
            detail: "Hold the most recent snapshot of this folder against what is here".into(),
            binding: None,
            keywords: &[
                "compare",
                "changed",
                "diff",
                "since",
                "snapshot",
                "grew",
                "difference",
            ],
            action: Action::CompareWithLatest(path.clone()),
        });
    }

    // An automatic record may be kept explicitly with its historical date.
    // Explicit snapshots are already kept and do not need a duplicate offer.
    if situation.have_tree
        && (!situation.viewing_snapshot || situation.automatic_snapshot_date.is_some())
    {
        commands.push(Command {
            label: if situation.automatic_snapshot_date.is_some() {
                "Keep this saved scan as a snapshot".into()
            } else {
                "Keep a snapshot of this scan".into()
            },
            detail: situation.automatic_snapshot_date.as_ref().map_or_else(
                || "So a later scan can say what changed since today".into(),
                |date| format!("From {date}; keeps the original scan date without rescanning"),
            ),
            binding: None,
            keywords: &[
                "snapshot", "keep", "save", "record", "baseline", "before", "compare",
            ],
            action: Action::TakeSnapshot,
        });
    }

    // The way back. A snapshot has a root and that root is a real folder, so
    // "scan it now" is the obvious next thing somebody wants after seeing what
    // it looked like then — and without this row the way out of the past is to
    // find the folder in the rail again.
    if situation.viewing_snapshot && !situation.root.is_empty() {
        commands.push(Command {
            label: format!("Scan {} as it is now", situation.root),
            detail: "Leave the snapshot and look at the disk".into(),
            binding: None,
            keywords: &[
                "scan", "now", "today", "current", "leave", "snapshot", "back",
            ],
            action: Action::ScanAgainForReal(std::path::PathBuf::from(&situation.root)),
        });
    }

    commands.push(Command {
        label: "Query help".into(),
        detail: "What you can search for, with examples".into(),
        binding: Some(format!("{MOD}/")),
        keywords: &["help", "syntax", "grammar", "how", "examples", "docs"],
        action: Action::OpenHelp,
    });

    commands.push(Command {
        label: if situation.read_write {
            "Switch to read-only mode".into()
        } else {
            "Switch to read-write mode".into()
        },
        detail: if situation.read_write {
            "Stop this window from being able to change anything".into()
        } else {
            "Allow actions that delete or move files (always confirms)".into()
        },
        binding: Some(format!("{MOD}\u{21e7}M")),
        keywords: &[
            "mode",
            "readonly",
            "readwrite",
            "safety",
            "delete",
            "unlock",
        ],
        action: Action::ToggleMode,
    });

    // Only where it would do something: already elevated, or a platform with
    // no such notion, and this row would be an offer that changes nothing.
    if situation.can_elevate && !situation.elevated {
        commands.push(Command {
            label: "Restart as administrator".into(),
            detail: "Reads the volume directly: much faster on a whole drive, \
                     exact on compressed files, and can see folders this \
                     window cannot"
                .into(),
            binding: None,
            keywords: &[
                "administrator",
                "admin",
                "elevate",
                "elevated",
                "privilege",
                "uac",
                "mft",
                "fast",
                "restart",
            ],
            action: Action::RestartAsAdministrator,
        });
    }

    commands.push(Command {
        label: if situation.dark {
            "Use the light theme".into()
        } else {
            "Use the dark theme".into()
        },
        detail: "Saved between runs".into(),
        binding: None,
        keywords: &["theme", "dark", "light", "colour", "color", "appearance"],
        action: Action::ToggleTheme,
    });

    // Offered whether or not this scan collected owners: the panel's empty
    // state is the only place that explains what the setting does and offers to
    // turn it on, so hiding the command until the setting is already on would
    // make it undiscoverable. The same argument as the administrator row, in
    // reverse.
    commands.push(Command {
        label: "Who is using the disk".into(),
        detail: "Add up what each account on this machine occupies".into(),
        binding: None,
        keywords: &[
            "owner", "owners", "user", "users", "account", "who", "whose", "quota", "shared", "sid",
        ],
        action: Action::ShowOwners,
    });

    commands.push(Command {
        label: "Settings".into(),
        detail: "Thresholds, exclusions, and where they are saved".into(),
        binding: None,
        keywords: &["settings", "preferences", "options", "config"],
        action: Action::ToggleSettings,
    });

    // Offered whether or not this window has changed anything: the log is a
    // record of every run, and the question it answers — "what did this do to
    // my machine?" — is asked most by somebody who does not remember.
    commands.push(Command {
        label: "Show the audit log".into(),
        detail: "Every change kosha has made, oldest first".into(),
        binding: None,
        keywords: &[
            "audit", "log", "history", "deleted", "record", "what", "changed", "jsonl",
        ],
        action: Action::ShowAuditLog,
    });
    commands.push(Command {
        label: "Install a marketplace pack…".into(),
        detail: "Choose a local recipe or review-only insight pack JSON file".into(),
        binding: None,
        keywords: &["recipe", "marketplace", "import", "install"],
        action: Action::InstallRecipePack,
    });

    commands.push(Command {
        label: "Sync recipe marketplace".into(),
        detail: "Refresh the public GitHub recipe catalog".into(),
        binding: None,
        keywords: &["marketplace", "recipe packs", "sync"],
        action: Action::SyncMarketplace,
    });

    // --------------------------------------------------------------- columns
    // The table's columns can be dragged and reordered directly, but a drag is
    // not discoverable and not available to somebody working from the
    // keyboard. The same rule as everywhere else applies: only offer the moves
    // that would move something, so nothing here is inert.
    if situation.have_tree {
        for (column, visible) in situation.columns.all().collect::<Vec<_>>() {
            if !visible {
                commands.push(Command {
                    label: format!("Show the {} column", column.title().to_lowercase()),
                    detail: "Put it back where it was".into(),
                    binding: None,
                    keywords: &["column", "show", "add", "table", "unhide"],
                    action: Action::SetColumnHidden(column, false),
                });
                continue;
            }

            let mut moved = situation.columns.clone();
            if moved.move_left(column) {
                commands.push(Command {
                    label: format!("Move the {} column left", column.title().to_lowercase()),
                    detail: "Towards the names".into(),
                    binding: None,
                    keywords: &["column", "move", "left", "order", "reorder", "table"],
                    action: Action::MoveColumn(column, Direction::Left),
                });
            }

            let mut moved = situation.columns.clone();
            if moved.move_right(column) {
                commands.push(Command {
                    label: format!("Move the {} column right", column.title().to_lowercase()),
                    detail: "Away from the names".into(),
                    binding: None,
                    keywords: &["column", "move", "right", "order", "reorder", "table"],
                    action: Action::MoveColumn(column, Direction::Right),
                });
            }

            let mut hidden = situation.columns.clone();
            if hidden.set_hidden(column, true) {
                commands.push(Command {
                    label: format!("Hide the {} column", column.title().to_lowercase()),
                    detail: "Its width is remembered".into(),
                    binding: None,
                    keywords: &["column", "hide", "remove", "table"],
                    action: Action::SetColumnHidden(column, true),
                });
            }
        }

        if !situation.columns.is_default() {
            commands.push(Command {
                label: "Reset the columns".into(),
                detail: "Back to the original order and widths".into(),
                binding: None,
                keywords: &["column", "reset", "default", "restore", "table", "width"],
                action: Action::ResetColumns,
            });
        }
    }

    // -------------------------------------------------------------- places
    // Passed in rather than resolved here: finding them touches the disk, and
    // the palette is rebuilt on every keystroke. The window holds the lists.
    //
    // Volumes last, because a rail of drive letters is what the sidebar
    // deliberately keeps below the directories anybody actually scans, and the
    // palette should not reverse that. They belong here all the same: this
    // module's own doc says the palette carries "the same locations the
    // sidebar offers", and until now it carried all of them except the drives
    // — so `Ctrl+K` then "C:" found nothing while the rail had had a row for
    // it the whole time.
    for place in places
        .iter()
        .chain(distributions.iter().map(|found| &found.place))
        .chain(volumes.iter().map(|volume| &volume.place))
    {
        commands.push(Command {
            label: format!("Scan {}", place.label),
            detail: place.path.display().to_string(),
            binding: None,
            keywords: &["scan", "open", "folder", "drive", "volume", "measure"],
            action: Action::Scan(place.path.clone()),
        });
    }

    // -------------------------------------------------------- saved queries
    // Above the catalogue's recipes on purpose: one of these is a query this
    // person chose to keep, the other is one we thought of for them.
    if situation.have_tree {
        for keep in saved {
            commands.push(Command {
                label: keep.name.clone(),
                detail: keep.query.clone(),
                binding: None,
                keywords: &["saved", "kept", "mine", "starred"],
                action: Action::RunSaved(keep.query.clone()),
            });
        }
    }

    // ------------------------------------------------------------- queries
    // Only with a tree: running a search against nothing would open the box
    // over an empty window and look broken.
    if situation.have_tree {
        for topic in help::catalogue()
            .iter()
            .filter(|topic| topic.kind == TopicKind::Recipe)
        {
            commands.push(Command {
                label: topic.label.to_string(),
                detail: topic.example.to_string(),
                binding: None,
                keywords: topic.keywords,
                action: Action::RunQuery(topic.example),
            });
        }
    }

    commands.push(Command {
        label: "Quit kosha".into(),
        detail: "Close the window".into(),
        binding: None,
        keywords: &["quit", "exit", "close", "leave"],
        action: Action::Quit,
    });

    commands
}

/// The commands matching `needle`, best first, as indices into `commands`.
///
/// Empty needle returns everything in table order, which is the order somebody
/// opening the palette to browse would want: what you do most, first.
pub fn search(commands: &[Command], needle: &str, limit: usize) -> Vec<usize> {
    let needle = needle.trim();
    if needle.is_empty() {
        return (0..commands.len().min(limit)).collect();
    }

    let mut scored: Vec<(u32, usize)> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| score(command, needle).map(|s| (s, index)))
        .collect();

    // Score first, then table order, so equally good matches keep the order
    // the list was deliberately written in.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// How well one command answers this search, if at all.
///
/// Weighted exactly like `help::score`: the name is worth most, then the words
/// somebody might reach for instead, then the line underneath.
fn score(command: &Command, needle: &str) -> Option<u32> {
    let mut best = None;
    let mut consider = |text: &str, weight: u32| {
        if let Some(score) = help::match_score(text, needle) {
            let scaled = score.saturating_add(weight);
            best = Some(best.map_or(scaled, |current: u32| current.max(scaled)));
        }
    };

    consider(&command.label, 400);
    for keyword in command.keywords {
        consider(keyword, 300);
    }
    consider(&command.detail, 0);
    best
}

// ---------------------------------------------------------------------- view

/// The palette overlay.
pub fn panel(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> AnyElement {
    let commands = app.commands(cx);
    let needle = app.command_search_text(cx);
    let matches = search(&commands, &needle, LIMIT);
    let selected = app.command_selected().min(matches.len().saturating_sub(1));

    let mut rows = Vec::with_capacity(matches.len());
    for (position, &index) in matches.iter().enumerate() {
        rows.push(row(
            &commands[index],
            index,
            position == selected,
            palette,
            cx,
        ));
    }

    ui::scrim("palette-scrim", palette)
        .flex_col()
        .items_center()
        .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
        .child(
            ui::dialog("palette", palette)
                // Near the top rather than centred: it is a thing you type
                // through, and the eye should not have to travel to it.
                .mt(px(96.0))
                .w(px(560.0))
                .max_h(px(520.0))
                .child(
                    h_flex()
                        .px(px(10.0))
                        .py(px(6.0))
                        .gap(px(8.0))
                        .items_center()
                        .rounded(px(metrics::RADIUS_SM))
                        .bg(palette.surface)
                        .border_1()
                        .border_color(palette.hairline)
                        .child(
                            div()
                                .flex_none()
                                .text_color(palette.text_faint)
                                .child("\u{203a}"),
                        )
                        .child(div().flex_1().child(Input::new(app.command_search()))),
                )
                .child(
                    v_flex()
                        .id("palette-list")
                        .flex_1()
                        .min_h(px(0.0))
                        .gap(px(1.0))
                        .overflow_y_scroll()
                        .track_scroll(app.command_scroll())
                        .when(rows.is_empty(), |this| {
                            this.child(
                                div()
                                    .p(px(12.0))
                                    .text_size(metrics::scaled(metrics::TEXT_SM))
                                    .text_color(palette.text_faint)
                                    .child("Nothing here matches that."),
                            )
                        })
                        .children(rows),
                ),
        )
        .into_any_element()
}

fn row(
    command: &Command,
    index: usize,
    selected: bool,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let label = SharedString::from(command.label.clone());
    let detail = SharedString::from(command.detail.clone());
    let action = command.action.clone();
    let binding = command.binding.clone().map(SharedString::from);
    // A query reads as a query: it is the thing that will land in the box.
    let mono = matches!(command.action, Action::RunQuery(_) | Action::RunSaved(_));

    h_flex()
        .id(("command", index))
        .cursor_pointer()
        .px(px(10.0))
        .py(px(6.0))
        .gap(px(10.0))
        .items_center()
        .rounded(px(metrics::RADIUS_SM))
        .when(selected, |this| this.bg(palette.selected))
        .when(!selected, |this| this.hover(|s| s.bg(palette.hover)))
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(1.0))
                .child(
                    div()
                        .text_size(metrics::scaled(metrics::TEXT))
                        .text_color(palette.text)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(metrics::scaled(metrics::TEXT_XS))
                        .text_color(palette.text_faint)
                        .when(mono, |this| this.text_color(palette.accent))
                        .child(detail),
                ),
        )
        .children(binding.map(|binding| {
            div()
                .flex_none()
                .px(px(6.0))
                .py(px(1.0))
                .rounded(px(metrics::RADIUS_XS))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.hairline)
                .text_size(metrics::scaled(metrics::TEXT_XS))
                .text_color(palette.text_faint)
                .child(binding)
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.run_command(action.clone(), window, cx);
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_pack_is_findable_by_listing_phrase_or_id() {
        let pack = kosha_search::RecipePackMetadata {
            publisher: None,
            id: "test-workflows".into(),
            title: "Example Workflows".into(),
            description: "Workflow files".into(),
            version: "1.0".into(),
            tags: vec![],
        };
        let commands = [remove_recipe_pack(&kosha_marketplace::InstalledPackage {
            kind: "recipe-pack",
            metadata: pack.clone(),
        })];
        for query in [
            "installed recipe packs",
            "test-workflows",
            "Example Workflows",
        ] {
            assert_eq!(search(&commands, query, LIMIT), [0]);
        }
        assert_eq!(
            commands[0].action,
            Action::RemoveRecipePack {
                id: format!("recipe-pack:{}", pack.qualified_id()),
                title: pack.title
            }
        );
    }

    fn browsing() -> Situation {
        Situation {
            have_tree: true,
            can_go_up: true,
            has_selection: true,
            ..Situation::default()
        }
    }

    #[test]
    fn cleanup_and_ai_handoffs_require_a_recorded_tree_and_are_discoverable() {
        let empty = all(&Situation::default(), &[], &[], &[], &[]);
        assert!(
            !empty
                .iter()
                .any(|command| matches!(command.action, Action::CleanupGuide | Action::AskAi))
        );
        let commands = all(&browsing(), &[], &[], &[], &[]);
        for (query, action) in [
            ("cleanup guide", Action::CleanupGuide),
            ("Ask AI", Action::AskAi),
        ] {
            let found = search(&commands, query, LIMIT);
            assert!(found.iter().any(|&index| commands[index].action == action));
        }
    }

    fn searching(query: &str) -> Situation {
        Situation {
            query: query.into(),
            ..browsing()
        }
    }

    fn kept(query: &str) -> Vec<crate::settings::SavedQuery> {
        vec![crate::settings::SavedQuery {
            name: query.into(),
            query: query.into(),
        }]
    }

    /// Two locations, so the place rows are exercised without touching disk.
    fn places() -> Vec<crate::places::Place> {
        vec![
            crate::places::Place {
                label: "Home".into(),
                glyph: "⌂",
                path: PathBuf::from("/home/someone"),
            },
            crate::places::Place {
                label: "Downloads".into(),
                glyph: "↓",
                path: PathBuf::from("/home/someone/Downloads"),
            },
        ]
    }

    fn labels(situation: &Situation) -> Vec<String> {
        all(situation, &places(), &[], &[], &[])
            .into_iter()
            .map(|command| command.label)
            .collect()
    }

    fn best(situation: &Situation, needle: &str) -> String {
        let commands = all(situation, &places(), &[], &[], &[]);
        let found = search(&commands, needle, 5);
        assert!(!found.is_empty(), "`{needle}` matched nothing");
        commands[found[0]].label.clone()
    }

    /// The audit log can be reached from the palette, by every word for it.
    ///
    /// The safety model lists the log as a layer and says it is "openable from
    /// the UI"; the README repeats it. Nothing opened it — the only mention of
    /// it in the whole window was the line that writes to it — so the answer
    /// to "what did this thing do to my machine last Tuesday" was a file whose
    /// name you had to know.
    ///
    /// From a cold start too: the log is a record of every run, and the person
    /// asking is usually the one who does not remember what happened.
    #[test]
    fn the_audit_log_is_reachable_from_the_palette() {
        let cold = Situation::default();
        assert!(
            labels(&cold)
                .iter()
                .any(|label| label == "Show the audit log"),
            "the log cannot be reached before a scan"
        );
        for word in ["audit", "log", "history", "deleted", "jsonl"] {
            assert_eq!(
                best(&cold, word),
                "Show the audit log",
                "`{word}` does not find it"
            );
        }
    }

    #[test]
    fn nothing_inert_is_ever_offered() {
        let cold = Situation::default();
        let names = labels(&cold);

        // With no tree there is nothing to rescan, search, or go up out of.
        for absent in ["Rescan this folder", "Go up one level", "Search"] {
            assert!(
                !names.iter().any(|label| label == absent),
                "`{absent}` offered with no tree loaded: {names:?}"
            );
        }
        // What is left works from a cold start.
        for present in ["Query help", "Settings", "Quit kosha"] {
            assert!(
                names.iter().any(|label| label == present),
                "`{present}` missing from a cold start: {names:?}"
            );
        }
    }

    // ------------------------------------------------------- administrator

    /// The same rule as everywhere else in this list: only offer what would do
    /// something. A window that is already elevated has nothing to restart
    /// into, and a platform with no such notion has nowhere to go.
    #[test]
    fn restarting_elevated_is_offered_only_where_it_would_change_anything() {
        const LABEL: &str = "Restart as administrator";

        let could = Situation {
            can_elevate: true,
            elevated: false,
            ..browsing()
        };
        assert!(labels(&could).iter().any(|label| label == LABEL));
        assert_eq!(best(&could, "admin"), LABEL);
        assert_eq!(best(&could, "uac"), LABEL);

        let already = Situation {
            elevated: true,
            ..could.clone()
        };
        assert!(
            !labels(&already).iter().any(|label| label == LABEL),
            "offered to a window that is already elevated"
        );

        let cannot = Situation {
            can_elevate: false,
            ..could
        };
        assert!(
            !labels(&cannot).iter().any(|label| label == LABEL),
            "offered where kosha cannot restart itself elevated"
        );
    }

    /// Elevation and read-write are separate powers, and the palette has to
    /// keep them separate: neither offer may imply or grant the other.
    #[test]
    fn elevation_and_read_write_are_two_different_offers() {
        let elevated_read_only = Situation {
            elevated: true,
            read_write: false,
            ..browsing()
        };
        let names = labels(&elevated_read_only);
        assert!(
            names.iter().any(|l| l == "Switch to read-write mode"),
            "an elevated window is still read-only until somebody says otherwise: {names:?}"
        );
    }

    // ------------------------------------------------------------- columns

    fn with_columns(columns: ColumnLayout) -> Situation {
        Situation {
            columns,
            ..browsing()
        }
    }

    #[test]
    fn a_column_at_the_edge_is_not_offered_a_move_that_would_do_nothing() {
        let names = labels(&browsing());

        assert!(
            !names.iter().any(|l| l == "Move the on disk column left"),
            "the leftmost column was offered a move left: {names:?}"
        );
        assert!(
            !names.iter().any(|l| l == "Move the modified column right"),
            "the rightmost column was offered a move right: {names:?}"
        );
        // And the ones in the middle get both.
        assert!(names.iter().any(|l| l == "Move the items column left"));
        assert!(names.iter().any(|l| l == "Move the items column right"));
    }

    #[test]
    fn hiding_and_showing_are_offered_as_one_or_the_other() {
        let mut layout = ColumnLayout::default();
        assert!(layout.set_hidden(Column::Items, true));
        let names = labels(&with_columns(layout));

        assert!(names.iter().any(|l| l == "Show the items column"));
        assert!(
            !names.iter().any(|l| l == "Hide the items column"),
            "offered to hide something already hidden: {names:?}"
        );
        assert!(
            !names.iter().any(|l| l.contains("items column left")),
            "a hidden column has no place to be moved to: {names:?}"
        );
    }

    #[test]
    fn the_last_visible_column_is_not_offered_for_hiding() {
        let mut layout = ColumnLayout::default();
        for column in [Column::Allocated, Column::Logical, Column::Items] {
            assert!(layout.set_hidden(column, true));
        }
        let names = labels(&with_columns(layout));

        assert!(
            !names.iter().any(|l| l == "Hide the modified column"),
            "offered to hide the only column left: {names:?}"
        );
        assert!(names.iter().any(|l| l == "Show the items column"));
    }

    #[test]
    fn resetting_is_offered_only_once_there_is_something_to_reset() {
        assert!(
            !labels(&browsing()).iter().any(|l| l == "Reset the columns"),
            "offered a reset of an untouched table"
        );

        let mut layout = ColumnLayout::default();
        layout.set_width(Column::Items, 200.0);
        assert!(
            labels(&with_columns(layout))
                .iter()
                .any(|l| l == "Reset the columns")
        );
    }

    #[test]
    fn columns_are_not_offered_before_there_is_a_table() {
        let names = labels(&Situation::default());
        assert!(
            !names.iter().any(|l| l.contains("column")),
            "column commands offered with no tree loaded: {names:?}"
        );
    }

    /// The palette is where somebody looks after failing to find a menu.
    #[test]
    fn columns_are_reachable_by_the_words_people_use() {
        let mut layout = ColumnLayout::default();
        layout.set_width(Column::Items, 200.0);
        let situation = with_columns(layout);

        assert_eq!(best(&situation, "reset column"), "Reset the columns");
        assert!(best(&situation, "hide modified").contains("modified"));
    }

    #[test]
    fn quick_look_is_offered_only_with_something_to_look_at() {
        assert!(
            labels(&browsing())
                .iter()
                .any(|l| l == "Show details for the selection")
        );

        let nothing_selected = Situation {
            has_selection: false,
            ..browsing()
        };
        assert!(
            !labels(&nothing_selected)
                .iter()
                .any(|l| l.contains("details")),
            "offered a panel with no entry to describe"
        );
    }

    #[test]
    fn quick_look_says_which_way_it_will_go() {
        let open = Situation {
            details_open: true,
            ..browsing()
        };
        assert!(labels(&open).iter().any(|l| l == "Hide the details"));
        assert!(!labels(&open).iter().any(|l| l.starts_with("Show details")));
    }

    #[test]
    fn the_root_of_a_scan_has_nowhere_above_it() {
        let at_root = Situation {
            can_go_up: false,
            ..browsing()
        };
        assert!(!labels(&at_root).iter().any(|l| l == "Go up one level"));
        assert!(labels(&browsing()).iter().any(|l| l == "Go up one level"));
    }

    /// A snapshot holds no file contents, so there is nothing in one to hunt
    /// through. The palette is the one place that lists everything the window
    /// can do, and a row there that reports on today under a window showing the
    /// past is worse than a row that is absent.
    #[test]
    fn no_duplicate_hunt_over_a_snapshot() {
        let past = Situation {
            viewing_snapshot: true,
            ..browsing()
        };
        let names = labels(&past);
        assert!(
            !names.iter().any(|l| l == "Find duplicate files"),
            "offered to hash files a snapshot does not contain"
        );
        // And the way out of the past is still there, which is the row
        // somebody in this state actually wants.
        assert!(names.iter().any(|l| l.starts_with("Scan ")), "{names:?}");
    }

    #[test]
    fn automatic_records_can_be_kept_with_their_date_and_snapshot_guards() {
        let situation = Situation {
            viewing_snapshot: true,
            automatic_snapshot_date: Some("18 Nov 09:30".into()),
            ..browsing()
        };
        let commands = all(&situation, &[], &[], &[], &[]);
        let keep = commands
            .iter()
            .find(|c| c.action == Action::TakeSnapshot)
            .unwrap();
        assert!(keep.detail.contains("18 Nov 09:30"));
        assert!(keep.detail.contains("original scan date"));
        assert!(!commands.iter().any(|c| c.action == Action::FindDuplicates));
        let explicit = Situation {
            automatic_snapshot_date: None,
            ..situation
        };
        assert!(
            !all(&explicit, &[], &[], &[], &[])
                .iter()
                .any(|c| c.action == Action::TakeSnapshot)
        );
    }

    #[test]
    fn the_duplicate_command_says_what_it_will_do() {
        assert!(
            labels(&browsing())
                .iter()
                .any(|l| l == "Find duplicate files")
        );

        let showing = Situation {
            showing_duplicates: true,
            ..browsing()
        };
        let names = labels(&showing);
        assert!(names.iter().any(|l| l == "Stop showing duplicates"));
        assert!(
            !names.iter().any(|l| l == "Find duplicate files"),
            "both directions offered at once"
        );

        // Mid-hunt it is neither of those two — starting a second one does
        // nothing and there is no view yet to stop showing — but it is not
        // *nothing*, which is what this used to assert. A hunt reads file
        // contents and can run for minutes, so the one place that lists
        // everything the window can do has to offer the way out of it.
        let running = Situation {
            finding_duplicates: true,
            ..browsing()
        };
        let names = labels(&running);
        assert!(names.iter().any(|l| l == "Stop the duplicate hunt"));
        assert!(
            !names.iter().any(|l| l == "Find duplicate files"),
            "offered a second hunt on top of the running one"
        );
        assert!(
            !names.iter().any(|l| l == "Stop showing duplicates"),
            "offered to close a view that does not exist yet"
        );
    }

    /// The owners panel is reachable before anything has collected owners, and
    /// on a window with no tree at all, because its empty state is the only
    /// place that explains the setting and offers to turn it on. A command that
    /// appeared only once the setting was already on would be one nobody found.
    #[test]
    fn who_is_using_the_disk_is_always_offered() {
        for situation in [browsing(), Situation::default()] {
            let commands = all(&situation, &[], &[], &[], &[]);
            let found = search(&commands, "who", 5);
            assert!(
                found
                    .iter()
                    .any(|&at| commands[at].action == Action::ShowOwners),
                "the owners panel was not reachable"
            );
        }
    }

    /// The words somebody would actually type. "owner" is the obvious one and
    /// "whose" is what the question sounds like in a person's head.
    #[test]
    fn the_owners_command_answers_to_the_words_people_use() {
        let commands = all(&browsing(), &[], &[], &[], &[]);
        for word in ["owner", "users", "whose", "account", "sid"] {
            let found = search(&commands, word, 6);
            assert!(
                found
                    .iter()
                    .any(|&at| commands[at].action == Action::ShowOwners),
                "`{word}` did not find the owners panel"
            );
        }
    }

    #[test]
    fn the_mode_and_theme_commands_name_the_destination() {
        let names = labels(&browsing());
        assert!(names.iter().any(|l| l == "Switch to read-write mode"));
        assert!(names.iter().any(|l| l == "Use the dark theme"));

        let other = Situation {
            read_write: true,
            dark: true,
            ..browsing()
        };
        let names = labels(&other);
        assert!(names.iter().any(|l| l == "Switch to read-only mode"));
        assert!(names.iter().any(|l| l == "Use the light theme"));
    }

    #[test]
    fn a_place_is_offered_as_something_to_scan() {
        let commands = all(&browsing(), &places(), &[], &[], &[]);
        let found = search(&commands, "downloads", 5);
        assert!(
            !found.is_empty(),
            "a location the sidebar lists was not offered"
        );
        assert_eq!(commands[found[0]].label, "Scan Downloads");
        assert_eq!(
            commands[found[0]].action,
            Action::Scan(PathBuf::from("/home/someone/Downloads")),
            "the row would scan somewhere other than the one it names"
        );
    }

    /// A stripped environment with no HOME and no readable volumes — and the
    /// one where being able to *choose* a folder matters most, because there
    /// is nothing else to offer.
    #[test]
    fn no_places_is_not_an_error() {
        let commands = all(&browsing(), &[], &[], &[], &[]);
        assert!(!commands.is_empty(), "the palette emptied itself");
        assert!(
            !commands.iter().any(|c| matches!(c.action, Action::Scan(_))),
            "a row that scans a place, with no places to scan"
        );
        assert!(
            commands.iter().any(|c| c.action == Action::ChooseFolder),
            "with nothing listed, choosing a folder is the only way in"
        );
    }

    /// An image is reached by neither the sidebar nor a folder picker: on
    /// Windows asking for directories sets `FOS_PICKFOLDERS`, which shows no
    /// files at all. Without its own row there is no way to open one from the
    /// window.
    #[test]
    fn choosing_a_disk_image_is_offered_too() {
        let commands = all(&browsing(), &[], &[], &[], &[]);
        let found = commands
            .iter()
            .find(|c| c.action == Action::ChooseImage)
            .expect("no way to open a disk image");
        assert!(
            found.keywords.contains(&"vhdx") && found.keywords.contains(&"image"),
            "somebody types what they have, not what the row is called"
        );
    }

    /// Both are offered before anything has been scanned, unlike almost
    /// everything else in the palette — which is the point: a window with
    /// nothing in it is exactly when somebody needs a way to put something
    /// there.
    #[test]
    fn choosing_is_offered_with_nothing_on_screen_yet() {
        let commands = all(&Situation::default(), &[], &[], &[], &[]);
        for wanted in [Action::ChooseFolder, Action::ChooseImage] {
            assert!(
                commands.iter().any(|c| c.action == wanted),
                "{wanted:?} is missing from an empty window"
            );
        }
    }

    /// The gap this closed. The module doc promises "the same locations the
    /// sidebar offers", and the drives — the one part of the rail that is
    /// always populated — were the part it did not carry.
    #[test]
    fn a_volume_is_offered_like_any_other_place() {
        let volumes = vec![crate::places::Volume {
            place: crate::places::Place {
                label: "C:\\".into(),
                glyph: "▣",
                path: PathBuf::from("C:\\"),
            },
            usage: None,
        }];
        let commands = all(&browsing(), &places(), &[], &volumes, &[]);

        let found = search(&commands, "C:", 5);
        assert!(!found.is_empty(), "a drive the rail lists was not offered");
        assert_eq!(commands[found[0]].label, "Scan C:\\");
        assert_eq!(
            commands[found[0]].action,
            Action::Scan(PathBuf::from("C:\\")),
            "the row would scan somewhere other than the one it names"
        );
    }

    /// Order matters: the sidebar deliberately keeps a row of drive letters
    /// below the directories anybody actually scans, and the palette should
    /// not reverse that when both match.
    #[test]
    fn the_directories_people_scan_come_before_the_drives() {
        let volumes = vec![crate::places::Volume {
            place: crate::places::Place {
                label: "D:\\".into(),
                glyph: "▣",
                path: PathBuf::from("D:\\"),
            },
            usage: None,
        }];
        let commands = all(&browsing(), &places(), &[], &volumes, &[]);

        let home = commands.iter().position(|c| c.label == "Scan Home");
        let drive = commands.iter().position(|c| c.label == "Scan D:\\");
        assert!(home.is_some() && drive.is_some(), "one of them is missing");
        assert!(home < drive, "the drives displaced the directories");
    }

    /// A WSL distribution is offered the same way a folder is. It arrives in
    /// its own parameter and has its own heading in the rail, and the palette
    /// is the one place where that distinction would be a nuisance: somebody
    /// typing "arch" wants to scan it, not to learn which list it came from.
    #[test]
    fn a_wsl_distribution_is_offered_like_any_other_place() {
        let distributions = vec![crate::places::Distribution {
            place: crate::places::Place {
                label: "archlinux".into(),
                glyph: "◱",
                path: kosha_core::wsl::path_of("archlinux"),
            },
            // The palette does not show the image size — it shows where a row
            // would scan — so a distribution whose image could not be found is
            // still offered exactly like one whose could.
            image: None,
        }];
        let commands = all(&browsing(), &places(), &distributions, &[], &[]);

        let found = search(&commands, "archlinux", 5);
        assert!(!found.is_empty(), "a distribution was not offered");
        assert_eq!(commands[found[0]].label, "Scan archlinux");
        assert_eq!(
            commands[found[0]].action,
            Action::Scan(kosha_core::wsl::path_of("archlinux")),
            "the row would scan somewhere other than the one it names"
        );

        // And it has not displaced the ordinary places.
        assert!(!search(&commands, "downloads", 5).is_empty());
    }

    #[test]
    fn saving_is_offered_only_with_something_to_save() {
        let commands = all(&browsing(), &places(), &[], &[], &[]);
        assert!(
            !commands.iter().any(|c| c.label.starts_with("Save this")),
            "offered to save an empty box"
        );

        let commands = all(&searching("size > 1gb"), &places(), &[], &[], &[]);
        let save = commands
            .iter()
            .find(|c| c.label == "Save this search")
            .expect("nothing offered to save the query with");
        assert_eq!(
            save.action,
            Action::SaveQuery("size > 1gb".into()),
            "the row would save something other than what is in the box"
        );
        assert_eq!(save.detail, "size > 1gb", "the row does not name it");
    }

    #[test]
    fn an_already_saved_search_is_offered_back_as_forgetting() {
        let situation = Situation {
            query_is_saved: true,
            ..searching("size > 1gb")
        };
        let commands = all(&situation, &places(), &[], &[], &kept("size > 1gb"));

        assert!(
            !commands.iter().any(|c| c.label == "Save this search"),
            "offered to save something already saved"
        );
        let forget = commands
            .iter()
            .find(|c| c.label == "Forget this search")
            .expect("no way to undo a save");
        assert_eq!(forget.action, Action::ForgetQuery("size > 1gb".into()));
    }

    #[test]
    fn a_kept_query_can_be_run_again() {
        let commands = all(&browsing(), &places(), &[], &[], &kept("ext in (mp4, mkv)"));
        let found = search(&commands, "mp4", 5);
        assert!(!found.is_empty(), "a kept query could not be found again");
        assert_eq!(
            commands[found[0]].action,
            Action::RunSaved("ext in (mp4, mkv)".into())
        );
    }

    #[test]
    fn kept_queries_come_before_the_ones_we_thought_of() {
        // Both match "big"; the one the user kept should win.
        let commands = all(&browsing(), &places(), &[], &[], &kept("big and old"));
        let saved_at = commands
            .iter()
            .position(|c| matches!(c.action, Action::RunSaved(_)))
            .unwrap();
        let recipe_at = commands
            .iter()
            .position(|c| matches!(c.action, Action::RunQuery(_)))
            .unwrap();
        assert!(
            saved_at < recipe_at,
            "a recipe outranked the user's own saved query"
        );
    }

    #[test]
    fn nothing_saved_adds_no_rows() {
        let bare = all(&browsing(), &places(), &[], &[], &[]);
        let with_one = all(&browsing(), &places(), &[], &[], &kept("size > 1gb"));
        assert_eq!(with_one.len(), bare.len() + 1);
    }

    #[test]
    fn the_export_rows_say_what_they_will_contain() {
        let whole = all(&browsing(), &places(), &[], &[], &[]);
        assert!(
            whole
                .iter()
                .any(|c| c.label == "Export everything as CSV\u{2026}"),
            "no CSV export offered while browsing"
        );

        let filtered = Situation {
            showing_rows: true,
            ..browsing()
        };
        let subset = all(&filtered, &places(), &[], &[], &[]);
        assert!(
            subset
                .iter()
                .any(|c| c.label == "Export these results as JSON\u{2026}"),
            "the label does not say it will export only the results"
        );
        assert!(
            !subset.iter().any(|c| c.label.contains("everything")),
            "offered to export everything while showing a subset"
        );
    }

    #[test]
    fn export_is_not_offered_with_nothing_to_export() {
        let commands = all(&Situation::default(), &places(), &[], &[], &[]);
        assert!(!commands.iter().any(|c| c.label.starts_with("Export")));
    }

    #[test]
    fn pinning_is_offered_for_the_folder_in_view() {
        let looking_at = Situation {
            folder: Some(PathBuf::from("/home/someone/Projects")),
            ..browsing()
        };
        let commands = all(&looking_at, &places(), &[], &[], &[]);
        let pin = commands
            .iter()
            .find(|c| c.label == "Pin Projects to the sidebar")
            .expect("no way to pin the folder in view");
        assert_eq!(
            pin.action,
            Action::PinScope(PathBuf::from("/home/someone/Projects")),
            "the row would pin somewhere other than the one it names"
        );

        // Already pinned: the offer is the other way round.
        let pinned = Situation {
            folder_pinned: true,
            ..looking_at
        };
        let commands = all(&pinned, &places(), &[], &[], &[]);
        assert!(
            commands
                .iter()
                .any(|c| c.label == "Unpin Projects from the sidebar")
        );
        assert!(!commands.iter().any(|c| c.label.starts_with("Pin ")));
    }

    #[test]
    fn pinning_is_not_offered_with_nothing_in_view() {
        let commands = all(&browsing(), &places(), &[], &[], &[]);
        assert!(!commands.iter().any(|c| c.label.contains("in the sidebar")));
    }

    #[test]
    fn typing_a_word_for_it_finds_it() {
        let at = browsing();
        assert_eq!(best(&at, "dupes"), "Find duplicate files");
        assert_eq!(best(&at, "refresh"), "Rescan this folder");
        assert_eq!(best(&at, "theme"), "Use the dark theme");
        assert_eq!(best(&at, "exit"), "Quit kosha");
        assert_eq!(best(&at, "parent"), "Go up one level");
        // A typo is still the word.
        assert_eq!(best(&at, "settigns"), "Settings");
    }

    #[test]
    fn recipes_from_the_help_are_runnable_commands() {
        let commands = all(&browsing(), &places(), &[], &[], &[]);
        let queries: Vec<&Command> = commands
            .iter()
            .filter(|command| matches!(command.action, Action::RunQuery(_)))
            .collect();

        assert!(!queries.is_empty(), "no recipes reached the palette");
        for command in queries {
            let Action::RunQuery(query) = command.action else {
                unreachable!()
            };
            // The palette must not offer a query that fails when chosen; the
            // help catalogue pins the same property for the same strings.
            assert!(
                kosha_query::Query::parse(query).is_ok(),
                "`{}` offers a query that does not parse: {query}",
                command.label
            );
            assert_eq!(command.detail, query, "the row shows what it will run");
        }
    }

    #[test]
    fn an_empty_search_lists_everything_in_order() {
        let commands = all(&browsing(), &places(), &[], &[], &[]);
        let found = search(&commands, "", 100);
        assert_eq!(found, (0..commands.len()).collect::<Vec<_>>());

        // Blank-but-not-empty counts as empty; a stray space is not a query.
        assert_eq!(search(&commands, "   ", 100), found);
    }

    #[test]
    fn the_limit_is_respected_and_the_best_survive_it() {
        let commands = all(&browsing(), &places(), &[], &[], &[]);
        let found = search(&commands, "s", 3);
        assert!(found.len() <= 3);

        let all_found = search(&commands, "s", 100);
        assert_eq!(found, all_found[..found.len()], "the cap dropped the best");
    }

    #[test]
    fn a_search_matching_nothing_returns_nothing() {
        let commands = all(&browsing(), &places(), &[], &[], &[]);
        assert!(search(&commands, "zzzqqqxxx", 40).is_empty());
    }

    #[test]
    fn every_command_has_something_to_read() {
        for situation in [Situation::default(), browsing()] {
            for command in all(&situation, &places(), &[], &[], &[]) {
                assert!(!command.label.is_empty());
                assert!(
                    !command.detail.is_empty(),
                    "{} has no detail",
                    command.label
                );
            }
        }
    }
}
