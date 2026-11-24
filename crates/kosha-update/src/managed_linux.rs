// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Linux activation for a per-user installation. Package authentication is the
//! caller's responsibility. Both launchers resolve through the same pointer.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use semver::Version;

const PROGRAMS: [&str; 2] = ["kosha", "kosha-gui"];
const MAX_UNPACKED_BYTES: u64 = 768 * 1024 * 1024;

/// Install an authenticated release and provision stable per-user launchers.
pub fn install_update(release: &crate::Release) -> io::Result<()> {
    let root = managed_root()?;
    let bin = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(invalid_layout)?
        .join(".local/bin");
    fs::create_dir_all(root.parent().ok_or_else(invalid_layout)?)?;
    let installation = Installation::open(&root)?;
    if installation
        .current()?
        .as_ref()
        .is_some_and(|active| active >= release.version())
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "This managed installation already has this version or a newer version",
        ));
    }
    installation.check_launchers(&bin)?;
    let staging = tempfile::tempdir_in(&installation.root)?;
    let package = crate::prepare_package(release, crate::Package::LinuxArchive, staging.path())?
        .authenticate()?;
    installation.install_with_launchers(package, &bin)
}

/// Restore either the previous release or an explicitly identified retained
/// release, without making a network request.
pub fn restore_version(requested: Option<&str>) -> io::Result<Version> {
    let root = managed_root()?;
    if !root
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No managed Kosha installation",
        ));
    }
    let installation = Installation::open(&root)?;
    match requested {
        None => installation.rollback(),
        Some(requested) => {
            let version = Version::parse(requested).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "Enter a version such as 1.2.3")
            })?;
            installation.activate(&version)?;
            Ok(version)
        }
    }
}

fn managed_root() -> io::Result<PathBuf> {
    kosha_core::dirs::data_dir()
        .filter(|path| path.is_absolute())
        .map(|data| data.join("managed-install"))
        .ok_or_else(invalid_layout)
}

/// A transaction within a dedicated, user-private managed installation root.
/// Hold this value across staging and activation to serialize GUI and CLI work.
pub struct Installation {
    root: PathBuf,
    _lock: File,
}

impl Installation {
    /// Open a dedicated installation root with an existing parent directory.
    pub fn open(root: &Path) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;

        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => sync_dir(root.parent().ok_or_else(invalid_layout)?)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = root.symlink_metadata()?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid_layout());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".update.lock"))?;
        lock.try_lock().map_err(io::Error::from)?;
        Ok(Self {
            root: root.to_path_buf(),
            _lock: lock,
        })
    }

    /// Return the active version after checking the pointer and both programs.
    pub fn current(&self) -> io::Result<Option<Version>> {
        let target = match fs::read_link(self.root.join("current")) {
            Ok(target) => target,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let version = target
            .to_str()
            .and_then(|target| target.strip_prefix("v"))
            .and_then(|version| Version::parse(version).ok())
            .filter(|version| version.pre.is_empty() && version.build.is_empty())
            .ok_or_else(invalid_layout)?;
        self.check_version(&version)?;
        Ok(Some(version))
    }

    /// Extract just the executable pair from an already authenticated official
    /// release archive. Archive entries never determine a filesystem destination.
    pub fn install_package(&self, mut package: crate::VerifiedPackage) -> io::Result<()> {
        if package.package() != crate::Package::LinuxArchive {
            return Err(invalid_layout());
        }
        let release = package.release().clone();
        self.install_archive(&release, package.reader()?)
    }

    fn check_launchers(&self, bin: &Path) -> io::Result<()> {
        for program in PROGRAMS {
            let path = bin.join(program);
            match fs::read_link(&path) {
                Ok(target) if target == self.root.join("current").join(program) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "An existing launcher needs your attention: {}",
                            path.display()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn install_with_launchers(
        &self,
        package: crate::VerifiedPackage,
        bin: &Path,
    ) -> io::Result<()> {
        self.check_launchers(bin)?;
        fs::create_dir_all(bin)?;
        let mut created = Vec::new();
        let result = (|| {
            for program in PROGRAMS {
                let path = bin.join(program);
                let target = self.root.join("current").join(program);
                match symlink(&target, &path) {
                    Ok(()) => created.push((path, target)),
                    Err(error)
                        if error.kind() == io::ErrorKind::AlreadyExists
                            && fs::read_link(&path).is_ok_and(|existing| existing == target) => {}
                    Err(error) => return Err(error),
                }
            }
            sync_dir(bin)?;
            self.install_package(package)
        })();
        // A final fsync can fail after activation. Keep the launchers if they work.
        if result.is_err() && self.current().ok().flatten().is_none() {
            for (path, target) in created {
                if fs::read_link(&path).is_ok_and(|existing| existing == target) {
                    let _ = fs::remove_file(path);
                }
            }
        }
        result
    }

    fn install_archive(&self, release: &crate::Release, archive: impl Read) -> io::Result<()> {
        let prefix = release.package_name(crate::Package::LinuxArchive);
        let prefix = prefix.strip_suffix(".tar.gz").ok_or_else(invalid_layout)?;
        let [mut cli, mut gui] = extract_programs(archive, Path::new(prefix), &self.root)?;
        self.install(release.version(), &mut cli, &mut gui)
    }

    /// Stage a verified executable pair and activate it together. The caller
    /// supplies open files from the same authenticated package. Previous version
    /// directories remain available for recovery.
    pub fn install(&self, version: &Version, cli: &mut File, gui: &mut File) -> io::Result<()> {
        let installed = self.current()?;
        if !version.pre.is_empty()
            || !version.build.is_empty()
            || installed.as_ref().is_some_and(|current| current >= version)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Choose a newer stable Kosha release",
            ));
        }
        let name = format!("v{version}");
        // Use a generated directory even for a retry after interruption. A
        // completed version directory is never overwritten or implicitly trusted.
        let staging = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(&self.root)?;
        for (program, source) in PROGRAMS.into_iter().zip([cli, gui]) {
            let source_len = source.metadata()?.len();
            if !source.metadata()?.is_file() || source_len == 0 {
                return Err(invalid_layout());
            }
            let mut file = File::create(staging.path().join(program))?;
            if io::copy(source, &mut file)? != source_len {
                return Err(invalid_layout());
            }
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
            file.sync_all()?;
        }
        sync_dir(staging.path())?;
        let destination = self.root.join(&name);
        if destination.symlink_metadata().is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "This release is already staged. Activate the retained version to recover.",
            ));
        }
        fs::rename(staging.path(), &destination)?;
        sync_dir(&self.root)?;
        self.activate(version)
    }

    /// Switch to a retained version, including after an interrupted install.
    pub fn activate(&self, version: &Version) -> io::Result<()> {
        self.current()?;
        self.check_version(version)?;
        let staging = tempfile::Builder::new()
            .prefix(".activate-")
            .tempdir_in(&self.root)?;
        symlink(format!("v{version}"), staging.path().join("current"))?;
        fs::rename(staging.path().join("current"), self.root.join("current"))?;
        sync_dir(&self.root)
    }

    /// Restore the most recent older version available in this installation.
    pub fn rollback(&self) -> io::Result<Version> {
        let unavailable = || {
            io::Error::new(
                io::ErrorKind::NotFound,
                "No previous Kosha version is available",
            )
        };
        let active = self.current()?.ok_or_else(unavailable)?;
        let mut previous = None;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(version) = entry.file_name().to_str().and_then(|name| {
                name.strip_prefix('v')
                    .and_then(|name| Version::parse(name).ok())
            }) else {
                continue;
            };
            if version < active
                && previous.as_ref().is_none_or(|current| version > *current)
                && self.check_version(&version).is_ok()
            {
                previous = Some(version);
            }
        }
        let previous = previous.ok_or_else(unavailable)?;
        self.activate(&previous)?;
        Ok(previous)
    }

    fn check_version(&self, version: &Version) -> io::Result<()> {
        let path = self.root.join(format!("v{version}"));
        if !version.pre.is_empty()
            || !version.build.is_empty()
            || !path.symlink_metadata()?.is_dir()
        {
            return Err(invalid_layout());
        }
        for program in PROGRAMS {
            let metadata = path.join(program).symlink_metadata()?;
            if !metadata.is_file()
                || metadata.len() == 0
                || metadata.permissions().mode() & 0o111 == 0
            {
                return Err(invalid_layout());
            }
        }
        Ok(())
    }
}

fn extract_programs(input: impl Read, prefix: &Path, directory: &Path) -> io::Result<[File; 2]> {
    let decoded = flate2::read::GzDecoder::new(input).take(MAX_UNPACKED_BYTES);
    let mut archive = tar::Archive::new(decoded);
    let expected = PROGRAMS.map(|program| prefix.join(program));
    let mut programs = [None, None];
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let Some(index) = expected.iter().position(|expected| *expected == *path) else {
            continue;
        };
        if programs[index].is_some() || !entry.header().entry_type().is_file() || entry.size() == 0
        {
            return Err(invalid_layout());
        }
        let mut program = tempfile::tempfile_in(directory)?;
        if io::copy(&mut entry, &mut program)? != entry.size() {
            return Err(invalid_layout());
        }
        program.rewind()?;
        programs[index] = Some(program);
    }
    let [Some(cli), Some(gui)] = programs else {
        return Err(invalid_layout());
    };
    // Consume the remainder so budget exhaustion and the gzip trailer are checked.
    let mut decoded = archive.into_inner();
    io::copy(&mut decoded, &mut io::sink())?;
    if decoded.limit() == 0 {
        return Err(invalid_layout());
    }
    Ok([cli, gui])
}

fn invalid_layout() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid Kosha managed installation",
    )
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

    fn program(contents: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(contents).unwrap();
        file.rewind().unwrap();
        file
    }

    #[test]
    fn official_archive_layout_supplies_exactly_one_regular_file_per_program() {
        fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut archive = tar::Builder::new(encoder);
            for (path, bytes) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                archive.append_data(&mut header, path, *bytes).unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap()
        }

        let dir = tempfile::tempdir().unwrap();
        let install = Installation::open(&dir.path().join("installation")).unwrap();
        let release = crate::Release::parse("v1.0.0").unwrap();
        let cli = "kosha-v1.0.0-x86_64-unknown-linux-gnu/kosha";
        let gui = "kosha-v1.0.0-x86_64-unknown-linux-gnu/kosha-gui";
        for entries in [
            vec![(cli, b"cli".as_slice())],
            vec![(cli, b"cli"), (gui, b"gui"), (gui, b"dup")],
        ] {
            assert!(
                install
                    .install_archive(&release, &mut program(&archive(&entries)))
                    .is_err()
            );
            assert_eq!(install.current().unwrap(), None);
        }
        install
            .install_archive(
                &release,
                &mut program(&archive(&[(cli, b"cli"), (gui, b"gui")])),
            )
            .unwrap();
        assert_eq!(install.current().unwrap(), Some(Version::new(1, 0, 0)));
    }

    #[test]
    fn switches_both_programs_and_retains_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("installation");
        let install = Installation::open(&root).unwrap();
        assert!(Installation::open(&root).is_err());
        let old = Version::new(1, 0, 0);
        let new = Version::new(1, 1, 0);
        install
            .install(&old, &mut program(b"old CLI"), &mut program(b"old GUI"))
            .unwrap();
        assert!(
            install
                .install(&new, &mut program(b"new CLI"), &mut program(b""))
                .is_err()
        );
        assert_eq!(install.current().unwrap(), Some(old.clone()));
        assert_eq!(fs::read(root.join("current/kosha")).unwrap(), b"old CLI");
        install
            .install(&new, &mut program(b"new CLI"), &mut program(b"new GUI"))
            .unwrap();
        assert_eq!(install.current().unwrap(), Some(new));
        assert_eq!(fs::read(root.join("current/kosha")).unwrap(), b"new CLI");
        assert_eq!(
            fs::read(root.join("current/kosha-gui")).unwrap(),
            b"new GUI"
        );
        install.activate(&old).unwrap();
        assert_eq!(
            fs::read(root.join("current/kosha-gui")).unwrap(),
            b"old GUI"
        );
    }

    #[test]
    fn refuses_non_managed_current_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("installation");
        let install = Installation::open(&root).unwrap();
        symlink("../another-application", root.join("current")).unwrap();
        assert!(
            install
                .install(
                    &Version::new(1, 0, 0),
                    &mut program(b"cli"),
                    &mut program(b"gui")
                )
                .is_err()
        );
        assert_eq!(
            fs::read_link(root.join("current")).unwrap(),
            Path::new("../another-application")
        );
    }

    #[test]
    fn preserves_existing_unmanaged_launchers() {
        let dir = tempfile::tempdir().unwrap();
        let install = Installation::open(&dir.path().join("installation")).unwrap();
        let bin = dir.path().join("bin");
        assert!(install.check_launchers(&bin).is_ok());
        fs::create_dir(&bin).unwrap();
        symlink("temporary-missing-target", bin.join("kosha")).unwrap();
        assert_eq!(
            install.check_launchers(&bin).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fs::read_link(bin.join("kosha")).unwrap(),
            Path::new("temporary-missing-target")
        );
    }
}
