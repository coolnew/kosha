// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `kosha` — a cross-platform disk usage explorer.
//!
//! GPU-composited through GPUI, read-only until the user explicitly enables writes.

// Never allocate a console. A GUI application that opens a black window behind
// itself looks broken, and it looked broken in debug builds specifically —
// which is the build people run while developing, so it was on almost all the
// time.
//
// Text-only commands attach to an existing parent console when needed. A GUI
// session stays detached: sharing a terminal's console also shares its close
// events, so closing that terminal could terminate an otherwise independent
// window. Explicit output redirection is still available for diagnostics.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod accent;
mod app;
mod columns;
mod commands;
mod comparison;
mod comparison_export;
mod confirm;
mod details;
mod drive_history;
mod elevate;
mod geometry;
mod handoff;
mod icons;
mod imported_text;
mod native_menu;
mod owners;
mod places;
mod query_help;
mod scan_timing;
mod search_bar;
mod search_recipes;
mod settings;
mod settings_panel;
mod snapshots;
mod state;
mod theme;
mod treemap;
mod treemap_preview;
mod ui;
mod window_chrome;

const USAGE: &str = "\
kosha-gui — see what is using your disk, and what to do about it

USAGE:
    kosha-gui [PATH] [OPTIONS]

ARGS:
    PATH             Start scanning this directory immediately, or open a
                     snapshot to browse what it recorded

OPTIONS:
    --query Q        Open with a search already applied, e.g. \"size > 1gb\"
    --duplicates     Hunt for identical file contents once the scan finishes
    --since FILE     Compare the scan with a snapshot and open on what changed
    --dark           Use the dark theme for this run
    --light          Use the light theme for this run
    -h, --help       Show this message
    -V, --version    Show the version

Everything else lives in the window. Settings are saved between runs; the
panel behind the gear names the file they live in.
";

/// `kosha-gui [PATH] [--dark|--light]`.
///
/// A path starts a scan immediately. The theme flags override the OS setting
/// for this run only — a flag is for one launch, and the persisted preference
/// belongs in the settings panel.
struct Args {
    path: Option<std::path::PathBuf>,
    theme: Option<gpui_kit::component::ThemeMode>,
    /// Applied once the scan finishes. Makes a particular view a thing you can
    /// link to and script, the same way `--query` works in `kosha`.
    query: Option<String>,
    /// Start the duplicate hunt as soon as there is a tree to hunt in.
    ///
    /// Matches `kosha --duplicates`, and makes the duplicate view something
    /// a shortcut can open rather than only something you can click to.
    duplicates: bool,
    /// A snapshot to hold the scan against, once there is a scan.
    ///
    /// Matches `kosha --since`. "What changed since Friday" is a thing worth
    /// putting in a shortcut or a scheduled task, and until this existed the
    /// window could only be told to do it by hand.
    since: Option<std::path::PathBuf>,
}

/// What to do once the command line has been read.
enum Start {
    Window(Args),
    /// Print this and stop, having answered what was asked.
    Said(String),
    /// Print this and stop, having refused: the command line was wrong.
    ///
    /// **Separate from [`Said`](Start::Said) because the exit code is.**
    /// `kosha-gui --nonsense` printed "unknown option" and exited **0**, and
    /// so did `--query` with nothing after it — so a launcher, a script or a
    /// desktop entry could not tell a window that opened from one that
    /// refused to. The command line's own front end has always exited 1 for
    /// exactly these two mistakes.
    Refused(String),
}

fn parse_args() -> Start {
    use gpui_kit::component::ThemeMode;

    let mut args = Args {
        path: None,
        theme: None,
        query: None,
        duplicates: false,
        since: None,
    };
    let mut expecting_query = false;
    let mut expecting_since = false;
    for arg in std::env::args_os().skip(1) {
        if expecting_query {
            expecting_query = false;
            args.query = arg.to_str().map(str::to_string);
            continue;
        }
        if expecting_since {
            expecting_since = false;
            args.since = Some(std::path::PathBuf::from(arg));
            continue;
        }
        match arg.to_str() {
            Some("--query") => expecting_query = true,
            Some("--since") => expecting_since = true,
            Some("--duplicates") => args.duplicates = true,
            Some("--dark") => args.theme = Some(ThemeMode::Dark),
            Some("--light") => args.theme = Some(ThemeMode::Light),
            Some("-h" | "--help") => return Start::Said(USAGE.to_string()),
            Some("-V" | "--version") => {
                // The same line the CLI prints, from the same function. This
                // used to be `CARGO_PKG_VERSION` alone, so the window answered
                // `kosha 1.0.0` while the scanner answered with the commit and
                // the target as well — two front ends of one program giving
                // different answers to "which build is this?", which is the
                // exact failure `kosha_core::version` was written to prevent.
                return Start::Said(format!("{}\n", kosha_core::version::long()));
            }
            Some(other) if other.starts_with('-') => {
                return Start::Refused(format!("kosha-gui: unknown option `{other}`\n\n{USAGE}"));
            }
            _ if args.path.is_none() => args.path = Some(std::path::PathBuf::from(arg)),
            _ => {}
        }
    }
    if expecting_query {
        return Start::Refused("kosha-gui: --query needs a query\n".to_string());
    }
    if expecting_since {
        return Start::Refused(
            "kosha-gui: --since needs a snapshot to compare against\n".to_string(),
        );
    }
    Start::Window(args)
}

/// Adopt the console of whatever launched us, if anything did and we have
/// nowhere else to write.
///
/// A `windows` subsystem binary starts with no console at all, so `println!`
/// and panic messages go nowhere. Attaching to the parent gives them somewhere
/// to land when the app was started from a shell, and fails harmlessly when it
/// was double-clicked — which is the case where a console would be noise.
///
/// The check first is not optional. `AttachConsole` *replaces* the standard
/// handles with the console's, so calling it unconditionally throws away a
/// redirection the shell already set up: `kosha-gui --help > out.txt` wrote an
/// empty file. Attaching only when there is no handle leaves a redirect alone.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    };

    // SAFETY: both take integers and return a handle or a status, and neither
    // reads or writes memory we own. A null or invalid handle means output has
    // nowhere to go, which is the only case we want to change.
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if stdout.is_null() || stdout == INVALID_HANDLE_VALUE {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// A window must not share the launching terminal's console lifetime.
///
/// Do not allocate a replacement console: this is a windows-subsystem binary
/// in every profile, and a normal Explorer launch must never flash a terminal.
/// FreeConsole also covers launchers that explicitly attached/inherited one;
/// it succeeds harmlessly if this process is already detached. Redirected file
/// and pipe handles are independent of the console and remain usable.
#[cfg(windows)]
fn detach_gui_console() {
    // SAFETY: this function takes no pointers and detaches only this process.
    // Other processes sharing the console are not detached or terminated.
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
}

#[cfg(not(windows))]
fn detach_gui_console() {}

/// GPUI's switch for presenting through a plain swap chain rather than
/// DirectComposition.
#[cfg(windows)]
const DISABLE_DIRECT_COMPOSITION: &str = "GPUI_DISABLE_DIRECT_COMPOSITION";

/// Start kosha again, once, with DirectComposition off. Returns whether it did.
///
/// In Windows Sandbox with the host's GPU shared in (its default), creating
/// the swap chain for DirectComposition failed with E_OUTOFMEMORY and no
/// window opened: "Creating DirectX renderer: Creating DirectX resources: Not
/// enough memory resources". With it off, the same build opened and scanned.
/// GPUI reads the switch once, when
/// the platform starts, so the only place left to change it by the time a
/// window fails is a new process. The variable being set already is what
/// stops a second attempt from starting a third.
#[cfg(windows)]
fn restart_without_direct_composition() -> bool {
    if std::env::var_os(DISABLE_DIRECT_COMPOSITION).is_some() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(DISABLE_DIRECT_COMPOSITION, "1")
        .spawn()
        .is_ok()
}

#[cfg(not(windows))]
fn restart_without_direct_composition() -> bool {
    false
}

fn main() -> std::process::ExitCode {
    let args = match parse_args() {
        Start::Window(args) => {
            detach_gui_console();
            args
        }
        // `say!`, not `print!`. `kosha-gui --version` is the documented way to
        // get a build for a bug report, and in PowerShell — which does not
        // wait for a windows-subsystem process and closes the pipe out from
        // under it — `print!` panicked instead of printing. See
        // `kosha_core::say`.
        Start::Said(text) => {
            attach_parent_console();
            kosha_core::say!("{text}");
            return std::process::ExitCode::SUCCESS;
        }
        Start::Refused(text) => {
            attach_parent_console();
            kosha_core::say!("{text}");
            // The same code the command line gives for the same mistake.
            return std::process::ExitCode::FAILURE;
        }
    };

    // The component library draws its icons — including the window's own
    // minimise, maximise and close controls — from embedded SVGs, which are
    // only reachable once an asset source is registered. Without this they
    // render as nothing at all: the controls are still there and still
    // clickable, but invisible, which reads as a contrast bug rather than a
    // missing asset.
    //
    // `icons::Assets` is the library's set plus kosha's own; see that module
    // for why there is only one of these to register.
    gpui_kit::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            native_menu::init(cx);
            // The flag is handed to the window rather than applied here: the
            // saved preference is not known until the settings are read, and
            // deciding between the two in one place is the only way the flag
            // reliably wins.
            if let Err(error) = app::open(
                app::Start {
                    path: args.path,
                    query: args.query,
                    duplicates: args.duplicates,
                    since: args.since,
                },
                args.theme,
                cx,
            ) {
                use std::io::Write;

                // `{:#}`: the whole cause chain, not just the outermost
                // context. Plain `{}` printed "Creating DirectX renderer" and
                // nothing else, which names the stage and hides the reason.
                // Diagnostic output is optional for a detached GUI. A closed
                // terminal/pipe must not make logging panic before the retry.
                let _ = writeln!(
                    std::io::stderr(),
                    "kosha-gui: could not open a window: {error:#}"
                );
                if restart_without_direct_composition() {
                    let _ = writeln!(
                        std::io::stderr(),
                        "kosha-gui: trying again without DirectComposition"
                    );
                }
                cx.quit();
            }
        });

    // The window ran. A failure to open one is said above and restarts itself
    // where it can; the process still leaves by the ordinary door, because a
    // launcher that took a non-zero exit as "kosha is broken" would be wrong
    // about the retry that is already under way.
    std::process::ExitCode::SUCCESS
}
