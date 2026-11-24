// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Release discovery shared by the desktop and command line.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

mod download;
#[cfg(target_os = "macos")]
pub use download::open_macos_update;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub use download::{PreparedPackage, VerifiedPackage, prepare_package};
pub use download::{fetch_package, verify_package};

mod setup;
#[cfg(windows)]
pub use setup::install_windows_update;
pub use setup::{SetupScope, windows_setup_scope};

#[cfg(target_os = "linux")]
pub mod managed_linux;

const ENDPOINT: &str = "https://api.github.com/repos/coolnew/kosha/releases/latest";
pub const RELEASES: &str = "https://github.com/coolnew/kosha/releases";
pub const STORE_UPDATES: &str = "ms-windows-store://downloadsandupdates";
pub const GUIDE: &str =
    "https://coolnew.github.io/kosha/manual/downloads/#update-an-existing-installation";
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const DAY: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release(Version);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Package {
    Setup,
    Portable,
    Dmg,
    LinuxArchive,
}

impl Package {
    pub fn for_host() -> &'static [Self] {
        match kosha_core::version::TARGET {
            "x86_64-pc-windows-msvc" => &[Self::Setup, Self::Portable],
            "aarch64-apple-darwin" => &[Self::Dmg],
            "x86_64-unknown-linux-gnu" => &[Self::LinuxArchive],
            _ => &[],
        }
    }

    pub const fn option(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Portable => "zip",
            Self::Dmg => "dmg",
            Self::LinuxArchive => "archive",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Setup => "Download Setup",
            Self::Portable => "Download portable ZIP",
            Self::Dmg => "Download DMG",
            Self::LinuxArchive => "Download Linux archive",
        }
    }

    const fn suffix(self) -> &'static str {
        match self {
            Self::Setup => "x64-setup.exe",
            Self::Portable => "x86_64-pc-windows-msvc.zip",
            Self::Dmg => "aarch64-apple-darwin.dmg",
            Self::LinuxArchive => "x86_64-unknown-linux-gnu.tar.gz",
        }
    }
}

impl Release {
    fn parse(tag: &str) -> Option<Self> {
        let number = tag.strip_prefix('v')?;
        let version = Version::parse(number).ok()?;
        (version.pre.is_empty() && version.build.is_empty()).then_some(Self(version))
    }

    pub fn version(&self) -> &Version {
        &self.0
    }

    pub fn release_url(&self) -> String {
        format!("{RELEASES}/tag/v{}", self.0)
    }

    pub fn download_url(&self, package: Package) -> String {
        format!(
            "{RELEASES}/download/v{}/{}",
            self.0,
            self.package_name(package)
        )
    }

    pub fn package_name(&self, package: Package) -> String {
        format!("kosha-v{}-{}", self.0, package.suffix())
    }

    pub fn is_newer(&self) -> bool {
        Version::parse(kosha_core::version::NUMBER)
            .is_ok_and(|installed| self.0.cmp_precedence(&installed).is_gt())
    }
}

#[derive(Clone, Debug)]
pub enum Check {
    Managed,
    Disabled,
    Busy,
    Unavailable,
    /// A daily attempt finished without a usable release, including an interrupted check.
    Waiting,
    Latest(Release),
}

impl Check {
    pub fn message(&self) -> String {
        match self {
            Self::Managed => {
                "Use Microsoft Store or your organization's package provider to update this copy."
                    .into()
            }
            Self::Disabled => "Update checks are disabled.".into(),
            Self::Busy => "Another Kosha process is checking for updates.".into(),
            Self::Unavailable => "Could not check GitHub for updates.".into(),
            Self::Waiting => {
                "Today's check returned no result. Kosha checks at most once every 24 hours.".into()
            }
            Self::Latest(release) if release.is_newer() => {
                format!("Kosha {} is available.", release.version())
            }
            Self::Latest(release) => format!(
                "No newer stable release. GitHub Latest is {}.",
                release.version()
            ),
        }
    }
}

/// Package identity establishes that a Windows package provider owns this copy.
/// An unexpected probe failure also suppresses direct-install recommendations.
pub fn managed_windows_package() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::APPMODEL_ERROR_NO_PACKAGE,
            Storage::Packaging::Appx::GetCurrentPackageFullName,
        };
        let mut length = 0;
        // SAFETY: Windows accepts a null output buffer when querying the required size.
        unsafe {
            GetCurrentPackageFullName(&mut length, std::ptr::null_mut())
                != APPMODEL_ERROR_NO_PACKAGE
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[derive(Serialize, Deserialize)]
struct State {
    schema: u8,
    automatic: bool,
    next_check: u64,
    tag: Option<String>,
    noticed: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: 1,
            automatic: false,
            next_check: 0,
            tag: None,
            noticed: None,
        }
    }
}

fn directory() -> io::Result<PathBuf> {
    kosha_core::dirs::data_dir()
        .map(|dir| dir.join("updates"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Kosha's data directory is unavailable",
            )
        })
}

fn load(dir: &Path) -> io::Result<State> {
    let file = match File::open(dir.join("state.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
        Err(error) => return Err(error),
    };
    let state: State = serde_json::from_slice(&bounded(file, 4096)?)?;
    if state.schema != 1
        || state
            .tag
            .as_deref()
            .is_some_and(|tag| Release::parse(tag).is_none())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsupported update state",
        ));
    }
    Ok(state)
}

fn save(dir: &Path, state: &State) -> io::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(dir.join("state.json"))?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn lock(dir: &Path) -> io::Result<File> {
    std::fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("state.lock"))?;
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

pub fn automatic() -> io::Result<bool> {
    Ok(load(&directory()?)?.automatic)
}

pub fn set_automatic(enabled: bool) -> io::Result<()> {
    let dir = directory()?;
    let _lock = lock(&dir)?;
    let mut state = load(&dir)?;
    state.automatic = enabled;
    save(&dir, &state)
}

pub fn check(manual: bool) -> Check {
    if managed_windows_package() {
        return Check::Managed;
    }
    if std::env::var_os("KOSHA_NO_UPDATE_CHECK").is_some() {
        return Check::Disabled;
    }
    let Ok(dir) = directory() else {
        return Check::Unavailable;
    };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return Check::Unavailable;
    };
    match check_in(&dir, now.as_secs(), manual, fetch) {
        Ok(status) => status,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Check::Busy,
        Err(_) => Check::Unavailable,
    }
}

fn check_in(
    dir: &Path,
    now: u64,
    manual: bool,
    fetch: impl FnOnce() -> io::Result<Release>,
) -> io::Result<Check> {
    if !manual && !load(dir)?.automatic {
        return Ok(Check::Disabled);
    }
    let _lock = lock(dir)?;
    let mut state = load(dir)?;
    if !manual && !state.automatic {
        return Ok(Check::Disabled);
    }
    if now < state.next_check {
        return Ok(state
            .tag
            .as_deref()
            .and_then(Release::parse)
            .map_or(Check::Waiting, Check::Latest));
    }
    // Reserve the day before requesting. A timeout or process exit consumes the reservation.
    state.next_check = now.saturating_add(DAY);
    state.tag = None;
    save(dir, &state)?;
    let Ok(release) = fetch() else {
        return Ok(Check::Unavailable);
    };
    state.tag = Some(format!("v{}", release.0));
    save(dir, &state)?;
    Ok(Check::Latest(release))
}

/// Claim the unsolicited notification across CLI and desktop for this release.
pub fn take_notice(release: &Release) -> bool {
    if !release.is_newer() {
        return false;
    }
    let Ok(dir) = directory() else { return false };
    claim_in(&dir, release).unwrap_or(false)
}

fn claim_in(dir: &Path, release: &Release) -> io::Result<bool> {
    let _lock = lock(dir)?;
    let mut state = load(dir)?;
    if !state.automatic
        || state.tag.as_deref().and_then(Release::parse).as_ref() != Some(release)
        || state
            .noticed
            .as_deref()
            .and_then(Release::parse)
            .is_some_and(|seen| seen.0 >= release.0)
    {
        return Ok(false);
    }
    state.noticed = state.tag.clone();
    save(dir, &state)?;
    Ok(true)
}

fn bounded(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut data)?;
    if data.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Update response is too large",
        ));
    }
    Ok(data)
}

fn decode(data: &[u8]) -> io::Result<Release> {
    #[derive(Deserialize)]
    struct Reply {
        tag_name: String,
        draft: bool,
        prerelease: bool,
        published_at: Option<String>,
    }
    let reply: Reply = serde_json::from_slice(data)?;
    if !reply.draft
        && !reply.prerelease
        && reply.published_at.is_some_and(|date| !date.is_empty())
        && let Some(release) = Release::parse(&reply.tag_name)
    {
        return Ok(release);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "No published stable Kosha release",
    ))
}

fn fetch() -> io::Result<Release> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let response = agent
        .get(ENDPOINT)
        .header("User-Agent", "kosha-update")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .call()
        .map_err(io::Error::other)?;
    if response.status() != 200 {
        return Err(io::Error::other("GitHub did not return release metadata"));
    }
    decode(&bounded(response.into_body().into_reader(), 512 * 1024)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stable_published_versions_supply_links() {
        let data = br#"{"tag_name":"v1.10.0","draft":false,"prerelease":false,"published_at":"2026-11-24T10:00:00Z","html_url":"https://invalid.example/"}"#;
        let release = decode(data).unwrap();
        assert_eq!(release.release_url(), format!("{RELEASES}/tag/v1.10.0"));
        assert_eq!(
            release.download_url(Package::Setup),
            format!("{RELEASES}/download/v1.10.0/kosha-v1.10.0-x64-setup.exe")
        );
        assert!(release.version() > &Version::new(1, 9, 0));
        for tag in [
            "v01.2.3",
            "1.2.3",
            "v1.2.3-rc.1",
            "v1.2.3+metadata",
            "v1.2.3/extra",
        ] {
            assert!(Release::parse(tag).is_none());
        }
        assert!(decode(br#"{"tag_name":"v1.10.0","draft":false,"prerelease":true,"published_at":"2026-11-24"}"#).is_err());
        assert!(
            decode(
                br#"{"tag_name":"v1.10.0","draft":true,"prerelease":false,"published_at":null}"#
            )
            .is_err()
        );
    }

    #[test]
    fn opt_in_daily_budget_and_single_notification_are_shared() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        assert!(matches!(
            check_in(dir, 100, false, || panic!("disabled")).unwrap(),
            Check::Disabled
        ));
        let release = Release::parse("v1.2.3").unwrap();
        assert!(matches!(
            check_in(dir, 100, true, || {
                let during = load(dir).unwrap();
                assert_eq!(during.next_check, DAY + 100);
                assert!(during.tag.is_none());
                assert_eq!(lock(dir).unwrap_err().kind(), io::ErrorKind::WouldBlock);
                Ok(release.clone())
            })
            .unwrap(),
            Check::Latest(_)
        ));
        assert!(!claim_in(dir, &release).unwrap());
        let mut enabled = load(dir).unwrap();
        enabled.automatic = true;
        save(dir, &enabled).unwrap();
        assert!(claim_in(dir, &release).unwrap());
        assert!(!claim_in(dir, &release).unwrap());
        assert!(matches!(
            check_in(dir, DAY + 99, false, || panic!("early retry")).unwrap(),
            Check::Latest(_)
        ));
        assert!(matches!(
            check_in(dir, DAY + 100, false, || Err(io::Error::other("offline"))).unwrap(),
            Check::Unavailable
        ));
        assert!(matches!(
            check_in(dir, 90, true, || panic!("clock moved backward")).unwrap(),
            Check::Waiting
        ));
    }

    #[test]
    fn corrupt_state_prevents_network_activity() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("state.json"), b"broken").unwrap();
        assert!(check_in(temp.path(), 100, true, || panic!("corrupt state")).is_err());
    }
}
