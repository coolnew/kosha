// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use crate::host_trust::wildcard;

/// The connection options contributed by a single SSH configuration file.
/// Paths are left unresolved for the caller's local path policy.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SshConfig {
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<String>,
    pub identities_only: Option<bool>,
    pub user_known_hosts_files: Option<Vec<String>>,
    pub ignored: Vec<(usize, String)>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("unsupported SSH configuration option {option} on line {line}")]
    Unsupported { line: usize, option: String },
    #[error("invalid SSH configuration on line {line}")]
    Invalid { line: usize },
}

/// Parse the supported settings without reading files or resolving hostnames.
/// Unsupported active settings fail closed unless explicitly safe to ignore.
pub fn parse_ssh_config(input: &str, alias: &str) -> Result<SshConfig, ConfigError> {
    let mut config = SshConfig::default();
    parse_config_into(input, alias, &mut config, |_, line, _| {
        Err(ConfigError::Unsupported {
            line,
            option: "Include".into(),
        })
    })?;
    Ok(config)
}

pub(crate) fn parse_config_into<E: From<ConfigError>>(
    input: &str,
    alias: &str,
    config: &mut SshConfig,
    mut include: impl FnMut(&[String], usize, &mut SshConfig) -> Result<(), E>,
) -> Result<(), E> {
    let mut active = true;
    for (index, text) in input.lines().enumerate() {
        let line = index + 1;
        let text = text.trim_start();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let end = text.find(|c: char| c.is_ascii_whitespace() || c == '=');
        let (option, raw) = text.split_at(end.unwrap_or(text.len()));
        let keyword = option.to_ascii_lowercase();
        let unsupported = || ConfigError::Unsupported {
            line,
            option: option.into(),
        };
        let invalid = || ConfigError::Invalid { line };
        // A Match block may change the interpretation of later lines even
        // when the preceding Host block did not apply.
        if keyword == "match" {
            return Err(unsupported().into());
        }
        if !active && keyword != "host" {
            continue;
        }
        let raw = raw.trim_start();
        let words = arguments(raw.strip_prefix('=').unwrap_or(raw)).ok_or_else(invalid)?;
        let single = || match words.as_slice() {
            [word] if !word.is_empty() => Ok(word.clone()),
            _ => Err(invalid()),
        };
        match keyword.as_str() {
            "host" => {
                let mut matched = false;
                let mut excluded = false;
                for word in words {
                    let (negative, pattern) = word
                        .strip_prefix('!')
                        .map_or((false, word.as_str()), |pattern| (true, pattern));
                    if pattern.is_empty() {
                        return Err(invalid().into());
                    }
                    if wildcard(pattern.as_bytes(), alias.as_bytes()) {
                        excluded |= negative;
                        matched |= !negative;
                    }
                }
                active = matched && !excluded;
            }
            "hostname" => {
                config.host_name.get_or_insert(single()?);
            }
            "user" => {
                config.user.get_or_insert(single()?);
            }
            "port" => {
                let port = single()?
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(invalid)?;
                config.port.get_or_insert(port);
            }
            "identityfile" => config.identity_files.push(single()?),
            "identitiesonly" => {
                let enabled = match single()?.to_ascii_lowercase().as_str() {
                    "yes" => true,
                    "no" => false,
                    _ => return Err(invalid().into()),
                };
                config.identities_only.get_or_insert(enabled);
            }
            "userknownhostsfile" if words.iter().all(|word| !word.is_empty()) => {
                config.user_known_hosts_files.get_or_insert(words);
            }
            "userknownhostsfile" => return Err(invalid().into()),
            "include" => include(&words, line, config)?,
            "stricthostkeychecking" if single()?.eq_ignore_ascii_case("yes") => {}
            "serveraliveinterval" | "serveralivecountmax" | "compression" | "checkhostip" => {
                config.ignored.push((line, option.into()));
            }
            _ => return Err(unsupported().into()),
        }
    }
    Ok(())
}

fn arguments(raw: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in raw.chars() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted && !started => break,
            '\\' | '\0' => return None,
            character if character.is_ascii_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
                continue;
            }
            character => word.push(character),
        }
        started = true;
    }
    if started {
        words.push(word);
    }
    (!quoted && !words.is_empty()).then_some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_first_scalar_and_accumulates_matching_identity_files() {
        let config = parse_ssh_config(
            "Host BOX\n HostName host.example\n Port=2222\n IdentityFile \"~/.ssh/key one\"\n\
             Host * !admin\n Port 22\n IdentityFile ~/.ssh/key2 # comment\n User scan",
            "box",
        )
        .unwrap();
        assert_eq!(config.host_name.as_deref(), Some("host.example"));
        assert_eq!(config.port, Some(2222));
        assert_eq!(config.user.as_deref(), Some("scan"));
        assert_eq!(config.identity_files, ["~/.ssh/key one", "~/.ssh/key2"]);
        assert_eq!(
            parse_ssh_config("Host * !admin\n Port 22", "admin")
                .unwrap()
                .port,
            None
        );
    }

    #[test]
    fn unsupported_routing_or_trust_settings_fail_closed() {
        for option in [
            "ProxyCommand helper",
            "ProxyJump jump",
            "Include config.d/*",
            "StrictHostKeyChecking accept-new",
            "KnownHostsCommand helper",
        ] {
            assert!(matches!(
                parse_ssh_config(option, "box"),
                Err(ConfigError::Unsupported { line: 1, .. })
            ));
        }
        assert!(matches!(
            parse_ssh_config("Host other\nMatch all\n User other", "box"),
            Err(ConfigError::Unsupported { line: 2, .. })
        ));
        let config =
            parse_ssh_config("Compression yes\nHost other\n ProxyCommand helper", "box").unwrap();
        assert_eq!(config.ignored, [(1, "Compression".to_string())]);
    }
}
