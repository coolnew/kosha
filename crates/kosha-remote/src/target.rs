// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::path::{Path, PathBuf};

use crate::SshConfig;

/// Explicit values parsed by the caller from the user-selected target.
pub struct TargetOptions<'a> {
    pub alias: &'a str,
    pub user: Option<&'a str>,
    pub port: Option<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity_files: Vec<PathBuf>,
    pub identities_only: bool,
    pub known_hosts_files: Vec<PathBuf>,
    pub ignored: Vec<(usize, String)>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    #[error("invalid SSH host, user or port")]
    Endpoint,
    #[error("unsupported expansion in SSH option {0}")]
    Expansion(&'static str),
    #[error("SSH option {0} requires an absolute path or a path under ~/")]
    Path(&'static str),
}

/// Resolve a target for preflight without filesystem access or DNS. The caller
/// supplies the local home and any system or Kosha-managed host trust files.
pub fn resolve_target(
    config: SshConfig,
    options: TargetOptions<'_>,
    home: &Path,
    local_user: &str,
    additional_known_hosts: &[PathBuf],
) -> Result<ResolvedTarget, TargetError> {
    let host = config.host_name.as_deref().unwrap_or(options.alias);
    let user = options
        .user
        .or(config.user.as_deref())
        .unwrap_or(local_user);
    let port = options.port.or(config.port).unwrap_or(22);
    if !valid_endpoint_part(host)
        || (host.contains(':') && host.parse::<std::net::Ipv6Addr>().is_err())
        || !valid_endpoint_part(user)
        || user.contains(':')
        || port == 0
    {
        return Err(TargetError::Endpoint);
    }

    let mut identity_files = Vec::new();
    if config.identity_files.is_empty() {
        identity_files
            .extend(["id_ed25519", "id_ecdsa", "id_rsa"].map(|name| home.join(".ssh").join(name)));
    } else {
        for path in config.identity_files {
            if !path.eq_ignore_ascii_case("none") {
                identity_files.push(resolve_path(home, &path, "IdentityFile")?);
            }
        }
    }
    let mut known_hosts_files = Vec::new();
    match config.user_known_hosts_files.as_deref() {
        None => known_hosts_files
            .extend(["known_hosts", "known_hosts2"].map(|name| home.join(".ssh").join(name))),
        Some([path]) if path.eq_ignore_ascii_case("none") => {}
        Some(paths) => {
            for path in paths {
                known_hosts_files.push(resolve_path(home, path, "UserKnownHostsFile")?);
            }
        }
    }
    known_hosts_files.extend_from_slice(additional_known_hosts);

    Ok(ResolvedTarget {
        host: host.into(),
        user: user.into(),
        port,
        identity_files,
        identities_only: config.identities_only.unwrap_or(false),
        known_hosts_files,
        ignored: config.ignored,
    })
}

pub(crate) fn valid_endpoint_part(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '@' | '/' | '\\' | '%' | '$' | '[' | ']')
        })
}

fn resolve_path(home: &Path, value: &str, option: &'static str) -> Result<PathBuf, TargetError> {
    if value.contains(['%', '$']) {
        return Err(TargetError::Expansion(option));
    }
    if let Some(relative) = value.strip_prefix("~/") {
        let relative = Path::new(relative);
        if !relative.as_os_str().is_empty()
            && relative.components().all(|component| {
                matches!(
                    component,
                    std::path::Component::Normal(_)
                        | std::path::Component::CurDir
                        | std::path::Component::ParentDir
                )
            })
        {
            return Ok(home.join(relative));
        }
        return Err(TargetError::Path(option));
    }
    let path = Path::new(value);
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Err(TargetError::Path(option))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_ssh_config;

    #[test]
    fn explicit_endpoint_and_configured_paths_take_precedence() {
        let config = parse_ssh_config(
            "Host box\n HostName box.example\n User config\n Port 2222\n\
             IdentityFile ~/.ssh/custom\n UserKnownHostsFile ~/.ssh/pins\n IdentitiesOnly yes",
            "box",
        )
        .unwrap();
        let home = Path::new("test-home");
        let system = home.join("system-pins");
        let target = resolve_target(
            config,
            TargetOptions {
                alias: "box",
                user: Some("explicit"),
                port: Some(2200),
            },
            home,
            "local",
            std::slice::from_ref(&system),
        )
        .unwrap();
        assert_eq!(
            (target.host.as_str(), target.user.as_str(), target.port),
            ("box.example", "explicit", 2200)
        );
        assert_eq!(target.identity_files, [home.join(".ssh/custom")]);
        assert_eq!(target.known_hosts_files, [home.join(".ssh/pins"), system]);
        assert!(target.identities_only);
    }

    #[test]
    fn defaults_and_unsupported_expansion_are_explicit() {
        let resolve = |text| {
            resolve_target(
                parse_ssh_config(text, "box").unwrap(),
                TargetOptions {
                    alias: "box",
                    user: None,
                    port: None,
                },
                Path::new("test-home"),
                "local",
                &[],
            )
        };
        let defaults = resolve("").unwrap();
        assert_eq!((defaults.user.as_str(), defaults.port), ("local", 22));
        assert_eq!(defaults.identity_files.len(), 3);
        let disabled = resolve("IdentityFile none\n UserKnownHostsFile none").unwrap();
        assert!(disabled.identity_files.is_empty());
        assert!(disabled.known_hosts_files.is_empty());
        assert_eq!(
            resolve("IdentityFile ~/.ssh/%h"),
            Err(TargetError::Expansion("IdentityFile"))
        );
        assert_eq!(
            resolve("UserKnownHostsFile ~other/pins"),
            Err(TargetError::Path("UserKnownHostsFile"))
        );
    }
}
