// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::client;

use crate::target::valid_endpoint_part;
use crate::{
    ConnectError, HostVerifier, LocalFileError, ResolvedTarget, TargetError, TargetOptions,
    connect, load_known_hosts, load_ssh_config, resolve_target,
};

#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("SSH scanning is disabled")]
    Disabled,
    #[error("local home directory is unavailable")]
    HomeUnavailable,
    #[error(transparent)]
    Local(#[from] LocalFileError),
    #[error(transparent)]
    Target(#[from] TargetError),
}

/// Resolve the shared desktop and CLI SSH environment before any connection.
pub fn prepare_local_target(
    enabled: bool,
    input: &str,
    config_dir: &Path,
) -> Result<PreparedTarget, PreflightError> {
    if !enabled {
        return Err(PreflightError::Disabled);
    }
    let (home_var, user_var) = if cfg!(windows) {
        ("USERPROFILE", "USERNAME")
    } else {
        ("HOME", "USER")
    };
    let home = std::env::var_os(home_var)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(PreflightError::HomeUnavailable)?;
    let user = std::env::var(user_var).unwrap_or_default();
    let mut trust = vec![config_dir.join("ssh_known_hosts")];
    if cfg!(unix) {
        trust.extend(["/etc/ssh/ssh_known_hosts", "/etc/ssh/ssh_known_hosts2"].map(PathBuf::from));
    } else if let Some(program_data) = std::env::var_os("PROGRAMDATA") {
        trust.push(
            PathBuf::from(program_data)
                .join("ssh")
                .join("ssh_known_hosts"),
        );
    }
    prepare_target(enabled, input, &home, &user, &trust)
}

pub struct PreparedTarget {
    target: ResolvedTarget,
    known_hosts: String,
}

impl PreparedTarget {
    pub fn target(&self) -> &ResolvedTarget {
        &self.target
    }

    /// Establish a verified connection, checking the setting again at dial time.
    pub async fn connect(
        self,
        enabled: bool,
        config: Arc<client::Config>,
    ) -> Result<(ResolvedTarget, client::Handle<HostVerifier>), ConnectError> {
        let session = connect(
            enabled,
            &self.target.host,
            self.target.port,
            self.known_hosts,
            config,
        )
        .await?;
        Ok((self.target, session))
    }

    /// Connect with the standard SSH algorithm preferences.
    pub async fn connect_default(
        self,
        enabled: bool,
    ) -> Result<(ResolvedTarget, client::Handle<HostVerifier>), ConnectError> {
        self.connect(enabled, Arc::new(client::Config::default()))
            .await
    }
}

/// Prepare a user-selected host using an injected local environment.
/// This reads local files only. DNS and network I/O are deferred to connect.
pub fn prepare_target(
    enabled: bool,
    input: &str,
    home: &Path,
    local_user: &str,
    additional_known_hosts: &[PathBuf],
) -> Result<PreparedTarget, PreflightError> {
    if !enabled {
        return Err(PreflightError::Disabled);
    }
    let options = parse_target(input)?;
    let config = load_ssh_config(enabled, &home.join(".ssh").join("config"), options.alias)?;
    let target = resolve_target(config, options, home, local_user, additional_known_hosts)?;
    let known_hosts = load_known_hosts(enabled, &target.known_hosts_files)?;
    Ok(PreparedTarget {
        target,
        known_hosts,
    })
}

fn parse_target(input: &str) -> Result<TargetOptions<'_>, TargetError> {
    let (user, endpoint) = match input.split_once('@') {
        Some((user, endpoint)) if valid_endpoint_part(user) && !user.contains(':') => {
            (Some(user), endpoint)
        }
        Some(_) => return Err(TargetError::Endpoint),
        None => (None, input),
    };
    let (alias, suffix) = if let Some(bracketed) = endpoint.strip_prefix('[') {
        let (ip, suffix) = bracketed.split_once(']').ok_or(TargetError::Endpoint)?;
        ip.parse::<Ipv6Addr>().map_err(|_| TargetError::Endpoint)?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(TargetError::Endpoint)?)
        };
        (ip, port)
    } else if endpoint.parse::<Ipv6Addr>().is_ok() {
        (endpoint, None)
    } else {
        match endpoint.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (endpoint, None),
        }
    };
    if !valid_endpoint_part(alias) {
        return Err(TargetError::Endpoint);
    }
    let port = suffix
        .map(|port| {
            if !port.bytes().all(|b| b.is_ascii_digit()) {
                return Err(TargetError::Endpoint);
            }
            port.parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or(TargetError::Endpoint)
        })
        .transpose()?;
    Ok(TargetOptions { alias, user, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_target_and_ipv6_ports() {
        let options = parse_target("scanner@[2001:db8::1]:2222").unwrap();
        assert_eq!(
            (options.alias, options.user, options.port),
            ("2001:db8::1", Some("scanner"), Some(2222))
        );
        assert_eq!(parse_target("2001:db8::1").unwrap().port, None);
        for input in [
            "host:0",
            "host:+22",
            "host:",
            "user@@host",
            "@host",
            "[box]",
            "[::1]22",
        ] {
            assert!(parse_target(input).is_err(), "{input}");
        }
    }

    #[test]
    fn gates_preflight_and_loads_only_selected_local_files() {
        assert!(matches!(
            prepare_local_target(false, "@", Path::new("\0")),
            Err(PreflightError::Disabled)
        ));
        assert!(matches!(
            prepare_target(false, "@", Path::new("\0"), "", &[]),
            Err(PreflightError::Disabled)
        ));
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        std::fs::create_dir(ssh.join("known_hosts")).unwrap();
        std::fs::write(ssh.join("config"), "Host box\n HostName host.invalid\n User config\n Port 123\n UserKnownHostsFile ~/.ssh/selected").unwrap();
        std::fs::write(ssh.join("selected"), "selected pin").unwrap();
        let prepared = prepare_target(true, "scanner@box:2222", home.path(), "local", &[]).unwrap();
        assert_eq!(prepared.target.host, "host.invalid");
        assert_eq!(prepared.target.user, "scanner");
        assert_eq!(prepared.target.port, 2222);
        assert_eq!(prepared.known_hosts, "selected pin\n");
    }
}
