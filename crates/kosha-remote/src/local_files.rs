// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::{ConfigError, SshConfig};
use crate::{host_trust::wildcard_match, ssh_config::parse_config_into};

const CONFIG_BYTES: usize = 256 * 1024;
const INCLUDE_DEPTH: usize = 8;
const INCLUDE_FILES: usize = 32;
const INCLUDE_DIRECTORY_ENTRIES: usize = 4096;
const KNOWN_HOSTS_BYTES: usize = 8 * 1024 * 1024;
const KNOWN_HOSTS_FILES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum LocalFileError {
    #[error("SSH scanning is disabled")]
    Disabled,
    #[error("could not read SSH file {path:?}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("SSH path is not a regular file: {0:?}")]
    NotRegular(PathBuf),
    #[error("SSH file exceeds the remaining byte budget: {0:?}")]
    TooLarge(PathBuf),
    #[error("SSH file is not UTF-8: {0:?}")]
    Encoding(PathBuf),
    #[error("too many SSH known-hosts files")]
    TooManyFiles,
    #[error("invalid SSH config {path:?}: {source}")]
    Config { path: PathBuf, source: ConfigError },
    #[error(
        "SSH Include at {path:?}:{line} requires a path inside the config directory with wildcards only in the filename"
    )]
    IncludePath { path: PathBuf, line: usize },
    #[error("SSH Include depth, file count or directory entry limit exceeded")]
    IncludeLimit,
    #[error(transparent)]
    Syntax(#[from] ConfigError),
}

/// Load one optional config file from a caller-supplied path.
pub fn load_ssh_config(
    enabled: bool,
    path: &Path,
    alias: &str,
) -> Result<SshConfig, LocalFileError> {
    if !enabled {
        return Err(LocalFileError::Disabled);
    }
    let mut config = SshConfig::default();
    let mut loader = ConfigLoader {
        root: path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
        alias,
        bytes: CONFIG_BYTES,
        files: INCLUDE_FILES,
        entries: INCLUDE_DIRECTORY_ENTRIES,
    };
    loader.parse(path, 0, &mut config)?;
    Ok(config)
}

struct ConfigLoader<'a> {
    root: &'a Path,
    alias: &'a str,
    bytes: usize,
    files: usize,
    entries: usize,
}

impl ConfigLoader<'_> {
    fn parse(
        &mut self,
        path: &Path,
        depth: usize,
        config: &mut SshConfig,
    ) -> Result<(), LocalFileError> {
        let Some(input) = read_optional_text(path, self.bytes)? else {
            return Ok(());
        };
        self.bytes -= input.len();
        parse_config_into(&input, self.alias, config, |words, line, config| {
            for word in words {
                if depth >= INCLUDE_DEPTH || self.files == 0 {
                    return Err(LocalFileError::IncludeLimit);
                }
                self.files -= 1;
                for child in self.include_paths(word, path, line)? {
                    self.parse(&child, depth + 1, config)?;
                }
            }
            Ok(())
        })
        .map_err(|error| match error {
            LocalFileError::Syntax(source) => LocalFileError::Config {
                path: path.to_owned(),
                source,
            },
            other => other,
        })
    }

    fn include_paths(
        &mut self,
        value: &str,
        path: &Path,
        line: usize,
    ) -> Result<Vec<PathBuf>, LocalFileError> {
        let invalid = || LocalFileError::IncludePath {
            path: path.to_owned(),
            line,
        };
        let value = value.strip_prefix("~/.ssh/").unwrap_or(value);
        let child = Path::new(value);
        if value.is_empty()
            || value.contains(['[', ']', '$', '%', '~'])
            || !child.components().all(|part| {
                matches!(
                    part,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        {
            return Err(invalid());
        }
        let root = self
            .root
            .canonicalize()
            .map_err(|source| LocalFileError::Read {
                path: self.root.to_owned(),
                source,
            })?;
        let canonicalize = |child: PathBuf| match child.canonicalize() {
            Ok(child) if child.starts_with(&root) => Ok(Some(child)),
            Ok(_) => Err(invalid()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(LocalFileError::Read {
                path: child,
                source,
            }),
        };
        if !value.contains(['*', '?']) {
            return Ok(canonicalize(root.join(child))?.into_iter().collect());
        }
        let parent = child.parent().ok_or_else(invalid)?;
        let pattern = child
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(invalid)?;
        if parent.to_string_lossy().contains(['*', '?']) {
            return Err(invalid());
        }
        let Some(directory) = canonicalize(root.join(parent))? else {
            return Ok(Vec::new());
        };
        let read_error = |source| LocalFileError::Read {
            path: directory.clone(),
            source,
        };
        let mut matched = Vec::new();
        for entry in std::fs::read_dir(&directory).map_err(read_error)? {
            self.entries = self
                .entries
                .checked_sub(1)
                .ok_or(LocalFileError::IncludeLimit)?;
            let entry = entry.map_err(read_error)?;
            let name = entry.file_name();
            if name.as_encoded_bytes().starts_with(b".") && !pattern.starts_with('.') {
                continue;
            }
            if wildcard_match(pattern.as_bytes(), name.as_encoded_bytes(), cfg!(windows)) {
                if matched.len() > self.files {
                    return Err(LocalFileError::IncludeLimit);
                }
                matched.push(entry.path());
            }
        }
        self.files -= matched.len().saturating_sub(1);
        matched.sort_unstable();
        let resolved = matched
            .into_iter()
            .map(canonicalize)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(resolved.into_iter().flatten().collect())
    }
}

/// Load selected trust files into one record stream, preserving revocations
/// across files. Missing files contribute no records.
pub fn load_known_hosts(enabled: bool, paths: &[PathBuf]) -> Result<String, LocalFileError> {
    if !enabled {
        return Err(LocalFileError::Disabled);
    }
    if paths.len() > KNOWN_HOSTS_FILES {
        return Err(LocalFileError::TooManyFiles);
    }
    let mut records = String::new();
    for path in paths {
        let remaining = KNOWN_HOSTS_BYTES - records.len();
        if let Some(mut input) = read_optional_text(path, remaining)? {
            if !input.is_empty() && !input.ends_with('\n') {
                if input.len() == remaining {
                    return Err(LocalFileError::TooLarge(path.clone()));
                }
                input.push('\n');
            }
            records.push_str(&input);
        }
    }
    Ok(records)
}

fn read_optional_text(path: &Path, limit: usize) -> Result<Option<String>, LocalFileError> {
    let read_error = |source| LocalFileError::Read {
        path: path.to_owned(),
        source,
    };
    match path.metadata() {
        Ok(metadata) if !metadata.is_file() => return Err(LocalFileError::NotRegular(path.into())),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(read_error(error)),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Refuse a FIFO even if it replaces the file after preflight.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(read_error)?;
    if !file.metadata().map_err(read_error)?.is_file() {
        return Err(LocalFileError::NotRegular(path.into()));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    if bytes.len() > limit {
        return Err(LocalFileError::TooLarge(path.into()));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| LocalFileError::Encoding(path.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_globs_use_filename_order_and_bound_matches() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let fragments = root.path().join("config.d");
        std::fs::create_dir(&fragments).unwrap();
        std::fs::write(&config, "Include config.d/*.conf\nPort 22").unwrap();
        std::fs::write(fragments.join("b.conf"), "Port 2222").unwrap();
        std::fs::write(fragments.join("a.conf"), "Port 1222").unwrap();
        std::fs::write(fragments.join(".hidden.conf"), "Port 3222").unwrap();
        assert_eq!(
            load_ssh_config(true, &config, "box").unwrap().port,
            Some(1222)
        );
        for index in 0..INCLUDE_FILES {
            std::fs::write(fragments.join(format!("{index}.conf")), "").unwrap();
        }
        assert!(matches!(
            load_ssh_config(true, &config, "box"),
            Err(LocalFileError::IncludeLimit)
        ));
    }

    #[test]
    fn includes_preserve_order_scope_and_depth_limits() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let child = root.path().join("child");
        std::fs::write(
            &config,
            "Host box\n Include child\n Port 22\n User parent\n Host other\n Include ../outside",
        )
        .unwrap();
        std::fs::write(&child, "Port 2222\nHost other\nUser wrong").unwrap();
        let selected = load_ssh_config(true, &config, "box").unwrap();
        assert_eq!(selected.port, Some(2222));
        assert_eq!(selected.user.as_deref(), Some("parent"));
        assert!(matches!(
            load_ssh_config(true, &config, "other"),
            Err(LocalFileError::IncludePath { line: 6, .. })
        ));

        std::fs::write(&child, "Include child").unwrap();
        assert!(matches!(
            load_ssh_config(true, &config, "box"),
            Err(LocalFileError::IncludeLimit)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn include_rejects_symlink_outside_config_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let config = root.path().join("config");
        std::fs::write(&config, "Include link").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(matches!(
            load_ssh_config(true, &config, "box"),
            Err(LocalFileError::IncludePath { .. })
        ));
    }

    #[test]
    fn disabled_loading_returns_before_touching_paths() {
        let path = PathBuf::from("\0");
        assert!(matches!(
            load_ssh_config(false, &path, "box"),
            Err(LocalFileError::Disabled)
        ));
        assert!(matches!(
            load_known_hosts(false, &[path]),
            Err(LocalFileError::Disabled)
        ));
    }

    #[test]
    fn bounds_reads_and_keeps_host_records_separate() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let missing = directory.path().join("missing");
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, "second\n").unwrap();
        assert_eq!(
            load_known_hosts(true, &[first.clone(), missing, second]).unwrap(),
            "first\nsecond\n"
        );
        assert!(matches!(
            read_optional_text(&first, 4),
            Err(LocalFileError::TooLarge(_))
        ));
        assert_eq!(
            read_optional_text(&first, 5).unwrap().as_deref(),
            Some("first")
        );
        assert!(matches!(
            read_optional_text(directory.path(), 4),
            Err(LocalFileError::NotRegular(_))
        ));
        std::fs::write(&first, "Host box\n Port 2222").unwrap();
        assert_eq!(
            load_ssh_config(true, &first, "box").unwrap().port,
            Some(2222)
        );
        std::fs::write(&first, [0xff]).unwrap();
        assert!(matches!(
            read_optional_text(&first, 4),
            Err(LocalFileError::Encoding(_))
        ));
    }
}
