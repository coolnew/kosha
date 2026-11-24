// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use kosha_core::sayln;
use kosha_update::{Check, GUIDE, Package, RELEASES, REQUEST_TIMEOUT};

const USAGE: &str = "kosha update [check|open|download [setup|zip|dmg|archive] [--to DIR]|verify FILE|install|rollback [VERSION] (Linux)|automatic on|automatic off]";

pub fn run(args: &[String]) -> io::Result<()> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--help" | "-h"] => sayln!("{USAGE}"),
        ["automatic", mode @ ("on" | "off")] => {
            kosha_update::set_automatic(*mode == "on")?;
            sayln!("Automatic update checks are {mode}.");
        }
        [] | ["check"] => report(&kosha_update::check(true)),
        ["open"] => {
            let status = kosha_update::check(true);
            report(&status);
            let url = match status {
                Check::Latest(release) if release.is_newer() => release.release_url(),
                Check::Managed => kosha_update::STORE_UPDATES.to_owned(),
                _ => GUIDE.to_owned(),
            };
            open_browser(&url)?;
        }
        ["download"] => download(None, None)?,
        ["download", package] => download(Some(package), None)?,
        ["download", "--to", dir] => download(None, Some(Path::new(dir)))?,
        ["download", package, "--to", dir] => download(Some(package), Some(Path::new(dir)))?,
        ["verify", path] => verify(Path::new(path))?,
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        ["install"] => install()?,
        #[cfg(target_os = "linux")]
        ["rollback", version @ ..] if version.len() <= 1 => {
            let version = kosha_update::managed_linux::restore_version(version.first().copied())?;
            sayln!("Kosha {version} is active. Restart the desktop app to use it.");
        }
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE)),
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn install() -> io::Result<()> {
    if kosha_update::managed_windows_package() {
        report(&Check::Managed);
        return open_browser(kosha_update::STORE_UPDATES);
    }
    #[cfg(windows)]
    if kosha_update::windows_setup_scope().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Use the update guide for this copy of Kosha. {GUIDE}"),
        ));
    }
    let status = kosha_update::check(true);
    let Check::Latest(release) = status else {
        return Err(io::Error::other(status.message()));
    };
    if !release.is_newer() {
        report(&Check::Latest(release));
        return Ok(());
    }
    sayln!("Downloading and verifying Kosha {}…", release.version());
    #[cfg(target_os = "linux")]
    {
        kosha_update::managed_linux::install_update(&release)?;
        sayln!(
            "Installed Kosha {}. Launch it from ~/.local/bin. Restart the desktop app to use the new version.",
            release.version()
        );
    }
    #[cfg(windows)]
    {
        kosha_update::install_windows_update(&release)?;
        sayln!("Setup opened. Close other Kosha windows, then finish the upgrade in Setup.");
    }
    #[cfg(target_os = "macos")]
    {
        kosha_update::open_macos_update(&release)?;
        sayln!("Disk image opened. Quit Kosha and replace Kosha.app in Applications.");
    }
    Ok(())
}

fn download(option: Option<&str>, destination: Option<&Path>) -> io::Result<()> {
    if kosha_update::managed_windows_package() {
        if destination.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Use the package provider to download this update",
            ));
        }
        report(&Check::Managed);
        return open_browser(kosha_update::STORE_UPDATES);
    }
    let package = match (option, Package::for_host()) {
        (None, [only]) => Some(*only),
        (None, [Package::Setup, Package::Portable]) if kosha_update::windows_setup_scope().is_some() => Some(Package::Setup),
        (Some(option), choices) => choices.iter().copied().find(|package| package.option() == option),
        _ => None,
    }.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
        "Choose an official package for this platform. On Windows, use setup or zip. See kosha update check for the guide."))?;
    let status = kosha_update::check(true);
    if let Check::Latest(release) = &status
        && (release.is_newer()
            || (destination.is_some()
                && release.version().to_string() == kosha_core::version::NUMBER))
    {
        if let Some(directory) = destination {
            let saved = kosha_update::fetch_package(release, package, directory)?;
            sayln!(
                "Saved and checked SHA-256: {}\nUpdate guide: {GUIDE}",
                saved.display()
            );
            return Ok(());
        }
        let url = release.download_url(package);
        sayln!("{}: {url}\nUpdate guide: {GUIDE}", package.label());
        return open_browser(&url);
    }
    report(&status);
    Ok(())
}

fn verify(path: &Path) -> io::Result<()> {
    let status = kosha_update::check(true);
    let Check::Latest(release) = status else {
        return Err(io::Error::other(status.message()));
    };
    kosha_update::verify_package(&release, path)?;
    sayln!(
        "Size and SHA-256 match GitHub: {}\nUpdate guide: {GUIDE}",
        path.display()
    );
    Ok(())
}

fn report(status: &Check) {
    sayln!("Installed Kosha {}", kosha_core::version::NUMBER);
    sayln!("{}", status.message());
    if let Check::Latest(release) = status {
        sayln!("Release: {}", release.release_url());
    } else {
        sayln!("Releases: {RELEASES}");
    }
    sayln!("Update guide: {GUIDE}");
}

fn open_browser(url: &str) -> io::Result<()> {
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn for_scan(args: &super::Args) -> Option<(Instant, Receiver<Check>)> {
    if args.export.is_some()
        || args.out.is_some()
        || args.snapshot
        || args.since.is_some()
        || args.cache.enabled
        || args.cache.dir.is_some()
        || args.wsl_native
        || std::env::var_os("CI").is_some()
        || !io::stdin().is_terminal()
        || !io::stdout().is_terminal()
        || !io::stderr().is_terminal()
        || std::env::var_os("KOSHA_NO_UPDATE_CHECK").is_some()
        || kosha_update::managed_windows_package()
        || !kosha_update::automatic().unwrap_or(false)
        || !args.path.is_dir()
    {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::Builder::new()
        .name("release-check".into())
        .spawn(move || {
            let _ = tx.send(kosha_update::check(false));
        })
        .ok()?;
    Some((started, rx))
}

pub fn finish_notice(notice: Option<(Instant, Receiver<Check>)>) {
    if let Some((started, receiver)) = notice
        && let Ok(Check::Latest(release)) =
            receiver.recv_timeout(REQUEST_TIMEOUT.saturating_sub(started.elapsed()))
        && kosha_update::take_notice(&release)
    {
        eprintln!(
            "Kosha {} is available.\nRelease: {}\nUpdate guide: {GUIDE}",
            release.version(),
            release.release_url()
        );
    }
}
