// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Package integrity checks against GitHub's release asset metadata.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{ENDPOINT, Package, Release, bounded, decode, managed_windows_package};

const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const MAX_PROVENANCE_BYTES: usize = 512 * 1024;
#[cfg(any(windows, target_os = "linux", target_os = "macos", test))]
const PROVENANCE_COUNT: usize = 8;

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    digest: Option<String>,
    state: String,
}

/// An integrity-checked download and its untrusted provenance bundles.
/// Installation additionally requires cryptographic verification of a bundle.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub struct PreparedPackage {
    file: tempfile::NamedTempFile,
    digest: String,
    bundles: Vec<serde_json::Value>,
    release: Release,
    package: Package,
}

/// A staged artifact authenticated against Kosha's tagged release workflow.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub struct VerifiedPackage {
    file: tempfile::NamedTempFile,
    release: Release,
    package: Package,
    #[cfg(windows)]
    digest: String,
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
impl VerifiedPackage {
    pub fn release(&self) -> &Release {
        &self.release
    }

    pub fn package(&self) -> Package {
        self.package
    }

    pub fn reader(&mut self) -> io::Result<impl Read + '_> {
        self.file.rewind()?;
        Ok(self.file.as_file_mut())
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub(crate) fn save(self, path: &Path) -> io::Result<File> {
        Ok(self.file.persist_noclobber(path)?)
    }

    #[cfg(windows)]
    pub(crate) fn save_and_pin_setup(self, path: &Path) -> io::Result<File> {
        if self.package != Package::Setup {
            return Err(invalid("Choose the official Windows Setup"));
        }
        let size = self.file.as_file().metadata()?.len();
        let digest = self.digest.clone();
        drop(self.save(path)?);
        let mut zone = path.as_os_str().to_os_string();
        zone.push(":Zone.Identifier");
        std::fs::write(zone, "[ZoneTransfer]\r\nZoneId=3\r\n")?;
        pin_setup(path, size, digest)
    }
}

#[cfg(windows)]
fn pin_setup(path: &Path, size: u64, digest: String) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // Permit reads while the Windows shell verifies and maps the image.
    // Deny writes and deletion until the shell has opened it.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)?;
    let asset = Asset {
        name: String::new(),
        size,
        digest: Some(digest),
        state: String::new(),
    };
    copy_checked(&file, io::sink(), &asset)?;
    Ok(file)
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
impl PreparedPackage {
    /// Check the signed in-toto statement and GitHub workload identity.
    pub fn authenticate(mut self) -> io::Result<VerifiedPackage> {
        use sigstore_trust_root::{SIGSTORE_PRODUCTION_TRUSTED_ROOT, TrustedRoot};
        use sigstore_types::{Bundle, SignatureContent, intoto::Statement};
        use sigstore_verify::{VerificationPolicy, Verifier};

        const REPOSITORY: &str = "https://github.com/coolnew/kosha";
        const ISSUER: &str = "https://token.actions.githubusercontent.com";
        let reference = format!("refs/tags/v{}", self.release.version());
        let identity = format!("{REPOSITORY}/.github/workflows/release.yml@{reference}");
        let expected_name = self.release.package_name(self.package);
        let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)
            .map_err(|_| invalid("Invalid built-in Sigstore trust root"))?;
        let verifier = Verifier::new(&root)
            .map_err(|_| invalid("Could not initialise the Sigstore verifier"))?;
        let policy = VerificationPolicy::new(identity.clone(), ISSUER);
        for json in &self.bundles {
            let Ok(bundle) = Bundle::from_json(&json.to_string()) else {
                continue;
            };
            let SignatureContent::DsseEnvelope(envelope) = &bundle.content else {
                continue;
            };
            let Ok(statement) = serde_json::from_slice::<Statement>(envelope.payload.as_bytes())
            else {
                continue;
            };
            if statement.type_ != "https://in-toto.io/Statement/v1"
                || statement.predicate_type != "https://slsa.dev/provenance/v1"
                || !statement.subject.iter().any(|subject| {
                    subject.name.rsplit('/').next() == Some(expected_name.as_str())
                        && subject.digest.sha256.as_ref().is_some_and(|hash| {
                            self.digest.strip_prefix("sha256:") == Some(hash.to_hex().as_str())
                        })
                })
            {
                continue;
            }
            self.file.rewind()?;
            let Ok(result) = verifier.verify_reader(self.file.as_file_mut(), &bundle, &policy)
            else {
                continue;
            };
            let Some(certificate) = result.certificate() else {
                continue;
            };
            let claims = &certificate.ci_claims;
            if claims.source_repository_uri.as_deref() == Some(REPOSITORY)
                && claims.source_repository_ref.as_deref() == Some(reference.as_str())
                && claims.build_signer_uri.as_deref() == Some(identity.as_str())
                && claims.runner_environment.as_deref() == Some("github-hosted")
                && claims.build_trigger.as_deref() == Some("push")
            {
                return Ok(VerifiedPackage {
                    file: self.file,
                    release: self.release,
                    package: self.package,
                    #[cfg(windows)]
                    digest: self.digest,
                });
            }
        }
        Err(invalid(
            "No valid attestation from Kosha’s tagged release workflow",
        ))
    }
}

/// Open an authenticated release image in macOS DiskImageMounter.
#[cfg(target_os = "macos")]
pub fn open_macos_update(release: &Release) -> io::Result<()> {
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    let folder = kosha_core::dirs::data_dir()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("Application data directory is unavailable"))?
        .join("updates/dmg");
    std::fs::create_dir_all(&folder)?;
    let stage = tempfile::tempdir_in(folder)?;
    let package = prepare_package(release, Package::Dmg, stage.path())?.authenticate()?;
    let image = stage.path().join(release.package_name(Package::Dmg));
    drop(package.save(&image)?);
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let quarantine = format!("0081;{seconds:x};Kosha;");
    let marked = Command::new("/usr/bin/xattr")
        .args(["-w", "com.apple.quarantine", &quarantine])
        .arg(&image)
        .stdin(Stdio::null())
        .status()?;
    if !marked.success() {
        return Err(io::Error::other(
            "macOS could not mark the image as a download",
        ));
    }
    let opened = Command::new("/usr/bin/open")
        .args(["-b", "com.apple.DiskImageMounter"])
        .arg(&image)
        .stdin(Stdio::null())
        .status()?;
    if !opened.success() {
        return Err(io::Error::other("macOS could not open the image"));
    }
    let _ = stage.keep();
    Ok(())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn asset(release: &Release, package: Package) -> io::Result<Asset> {
    if managed_windows_package() || !Package::for_host().contains(&package) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Use the installation provider for this copy",
        ));
    }
    if std::env::var_os("KOSHA_NO_UPDATE_CHECK").is_some() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "GitHub update requests are disabled",
        ));
    }
    let url = format!(
        "{}/tags/v{}",
        ENDPOINT.trim_end_matches("/latest"),
        release.version()
    );
    let response = agent(Duration::from_secs(10))
        .get(&url)
        .header("User-Agent", "kosha-update")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .call()
        .map_err(|_| io::Error::other("Could not read GitHub package metadata"))?;
    if response.status() != 200 {
        return Err(invalid("GitHub did not return package metadata"));
    }
    choose_asset(
        &bounded(response.into_body().into_reader(), 512 * 1024)?,
        release,
        package,
    )
}

fn choose_asset(data: &[u8], release: &Release, package: Package) -> io::Result<Asset> {
    #[derive(Deserialize)]
    struct Reply {
        assets: Vec<Asset>,
    }
    if decode(data)? != *release {
        return Err(invalid("The selected release no longer matches GitHub"));
    }
    let filename = release.package_name(package);
    let mut assets = serde_json::from_slice::<Reply>(data)?
        .assets
        .into_iter()
        .filter(|asset| asset.name == filename);
    let selected = assets
        .next()
        .ok_or_else(|| invalid("This release does not provide the selected package"))?;
    if assets.next().is_some()
        || selected.state != "uploaded"
        || !(1..=MAX_ASSET_BYTES).contains(&selected.size)
        || !selected
            .digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(invalid(
            "The package is missing valid size or SHA-256 metadata",
        ));
    }
    Ok(selected)
}

fn copy_checked(reader: impl Read, mut writer: impl Write, asset: &Asset) -> io::Result<()> {
    let mut reader = reader.take(asset.size + 1);
    let mut buffer = [0u8; 64 * 1024];
    let mut hash = Sha256::new();
    let mut count = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += read as u64;
        if count > asset.size {
            return Err(invalid("Package exceeded the expected size"));
        }
        hash.update(&buffer[..read]);
        writer.write_all(&buffer[..read])?;
    }
    let hash = format!("sha256:{:x}", hash.finalize());
    if count != asset.size
        || !asset
            .digest
            .as_deref()
            .is_some_and(|digest| digest.eq_ignore_ascii_case(&hash))
    {
        return Err(invalid("Package size or SHA-256 does not match GitHub"));
    }
    Ok(())
}

/// Check a package saved by the browser without opening or changing it.
pub fn verify_package(release: &Release, path: &Path) -> io::Result<()> {
    let package = Package::for_host()
        .iter()
        .copied()
        .find(|&package| {
            path.file_name().and_then(|name| name.to_str())
                == Some(release.package_name(package).as_str())
        })
        .ok_or_else(|| {
            invalid("Choose the original package filename for the current stable release")
        })?;
    let asset = asset(release, package)?;
    if !path.metadata()?.is_file() {
        return Err(invalid("Choose a regular package file"));
    }
    copy_checked(File::open(path)?, io::sink(), &asset)
}

/// Save an official Linux package to a directory selected by the caller.
/// Browsers remain the download path on platforms that attach launch metadata.
pub fn fetch_package(release: &Release, package: Package, dir: &Path) -> io::Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Download through the browser, then use kosha update verify",
        ));
    }
    let asset = asset(release, package)?;
    let destination = dir.join(&asset.name);
    if destination.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The package already exists in that directory",
        ));
    }
    let file = download(release, package, dir, &asset)?;
    file.persist_noclobber(&destination)?;
    Ok(destination)
}

/// Stage an update in a temporary file without executing it or keeping it in Downloads.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub fn prepare_package(
    release: &Release,
    package: Package,
    staging_dir: &Path,
) -> io::Result<PreparedPackage> {
    if !release.is_newer()
        || !matches!(
            package,
            Package::Setup | Package::LinuxArchive | Package::Dmg
        )
    {
        return Err(invalid("Choose a newer release for this installation"));
    }
    let asset = asset(release, package)?;
    let digest = asset
        .digest
        .as_ref()
        .ok_or_else(|| invalid("Missing SHA-256"))?
        .to_ascii_lowercase();
    let bundles = provenance_bundles(&digest)?;
    let file = download(release, package, staging_dir, &asset)?;
    Ok(PreparedPackage {
        file,
        digest,
        bundles,
        release: release.clone(),
        package,
    })
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn provenance_bundles(digest: &str) -> io::Result<Vec<serde_json::Value>> {
    let url = format!(
        "https://api.github.com/repos/coolnew/kosha/attestations/{digest}?predicate_type=provenance&per_page={PROVENANCE_COUNT}"
    );
    let response = agent(Duration::from_secs(10))
        .get(&url)
        .header("User-Agent", "kosha-update")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .call()
        .map_err(|_| io::Error::other("Could not retrieve GitHub release provenance"))?;
    if response.status() != 200 {
        return Err(invalid("GitHub did not return release provenance"));
    }
    read_bundles(&bounded(
        response.into_body().into_reader(),
        MAX_PROVENANCE_BYTES,
    )?)
}

#[cfg(any(windows, target_os = "linux", target_os = "macos", test))]
fn read_bundles(data: &[u8]) -> io::Result<Vec<serde_json::Value>> {
    #[derive(Deserialize)]
    struct Attestation {
        bundle: Option<serde_json::Value>,
    }
    #[derive(Deserialize)]
    struct Reply {
        attestations: Vec<Attestation>,
    }
    let records = serde_json::from_slice::<Reply>(data)?.attestations;
    if records.len() > PROVENANCE_COUNT {
        return Err(invalid("Unexpected number of provenance records"));
    }
    let bundles: Vec<_> = records
        .into_iter()
        .filter_map(|record| record.bundle)
        .filter(serde_json::Value::is_object)
        .collect();
    if bundles.is_empty() {
        return Err(invalid("This release has no available provenance bundle"));
    }
    Ok(bundles)
}

fn download(
    release: &Release,
    package: Package,
    dir: &Path,
    asset: &Asset,
) -> io::Result<tempfile::NamedTempFile> {
    let agent = agent(Duration::from_secs(120));
    let mut url = release.download_url(package);
    for _ in 0..3 {
        let response = agent
            .get(&url)
            .header("User-Agent", "kosha-update")
            .call()
            .map_err(|_| io::Error::other("GitHub package download failed"))?;
        if response.status() == 200 {
            let mut file = tempfile::NamedTempFile::new_in(dir)?;
            copy_checked(response.into_body().into_reader(), &mut file, asset)?;
            file.as_file().sync_all()?;
            file.rewind()?;
            return Ok(file);
        }
        if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Err(invalid("Unexpected package download response"));
        }
        url = response
            .headers()
            .get("location")
            .and_then(|header| header.to_str().ok())
            .filter(|location| allowed_cdn(location))
            .ok_or_else(|| invalid("Unexpected package download destination"))?
            .to_owned();
    }
    Err(invalid("Too many package download redirects"))
}

fn allowed_cdn(url: &str) -> bool {
    url.len() <= 8192
        && url.parse::<ureq::http::Uri>().is_ok_and(|uri| {
            uri.scheme_str() == Some("https")
                && matches!(
                    uri.authority().map(|authority| authority.as_str()),
                    Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn setup_pin_rejects_substitution_and_excludes_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.exe");
        std::fs::write(&path, b"original").unwrap();
        let digest = format!("sha256:{:x}", Sha256::digest(b"original"));
        let file = pin_setup(&path, 8, digest.clone()).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        drop(file);
        std::fs::write(&path, b"modified").unwrap();
        assert!(pin_setup(&path, 8, digest).is_err());
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn invalid_attestation_cannot_produce_a_verified_package() {
        let prepared = PreparedPackage {
            file: tempfile::NamedTempFile::new().unwrap(),
            digest: format!("sha256:{:x}", Sha256::digest(b"")),
            bundles: vec![serde_json::json!({"signature": "not a Sigstore bundle"})],
            release: Release::parse("v1.2.3").unwrap(),
            package: Package::LinuxArchive,
        };
        assert!(prepared.authenticate().is_err());
    }

    #[test]
    fn accepts_only_inline_provenance_bundles_and_bounds_the_record_count() {
        let data = br#"{"attestations":[{"bundle_url":"https://invalid.example/bundle"},{"bundle":{"example":true}}]}"#;
        assert_eq!(
            read_bundles(data).unwrap(),
            [serde_json::json!({"example": true})]
        );
        assert!(
            read_bundles(br#"{"attestations":[{"bundle_url":"https://invalid.example/bundle"}]}"#)
                .is_err()
        );
        let excessive = serde_json::json!({"attestations": vec![serde_json::json!({"bundle": {}}); PROVENANCE_COUNT + 1]});
        assert!(read_bundles(&serde_json::to_vec(&excessive).unwrap()).is_err());
    }

    #[test]
    fn matches_stable_release_and_verifies_complete_payload() {
        let release = Release::parse("v1.2.3").unwrap();
        let package = Package::LinuxArchive;
        let filename = release.package_name(package);
        let digest = format!("sha256:{:x}", Sha256::digest(b"abc"));
        let mut reply = serde_json::json!({
            "tag_name": "v1.2.3", "draft": false, "prerelease": false, "published_at": "2026-11-24",
            "assets": [{"name": filename, "size": 3, "digest": digest, "state": "uploaded"}],
        });
        let selected =
            choose_asset(&serde_json::to_vec(&reply).unwrap(), &release, package).unwrap();
        let mut payload = Vec::new();
        copy_checked(&b"abc"[..], &mut payload, &selected).unwrap();
        assert_eq!(payload, b"abc");
        for bad in [b"ab".as_slice(), b"abcd", b"xyz"] {
            assert!(copy_checked(bad, io::sink(), &selected).is_err());
        }
        reply["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(choose_asset(&serde_json::to_vec(&reply).unwrap(), &release, package).is_err());
    }

    #[test]
    fn download_redirects_accept_only_named_https_cdn_origins() {
        assert!(allowed_cdn(
            "https://release-assets.githubusercontent.com/package?token=x"
        ));
        for bad in [
            "http://objects.githubusercontent.com/pkg",
            "https://objects.githubusercontent.com:123/pkg",
            "https://objects.githubusercontent.com@invalid.example/pkg",
            "https://objects.githubusercontent.com.invalid.example/pkg",
            "/relative",
        ] {
            assert!(!allowed_cdn(bad));
        }
    }
}
