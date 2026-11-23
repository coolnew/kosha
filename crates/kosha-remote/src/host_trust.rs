// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use ring::hmac;
use russh::keys::PublicKey;
use russh::keys::ssh_key::known_hosts::{Entry, HostPatterns, Marker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    Trusted,
    Unknown,
    Changed,
    Revoked,
    CertificateAuthorityOnly,
    InvalidRecord,
}

/// Decide trust for a bare host key against an already loaded OpenSSH file.
/// A CA record is never a bare-key pin. Certificate validation is separate.
pub fn check_host_key(input: &str, host: &str, port: u16, key: &PublicKey) -> Trust {
    let host = host.to_ascii_lowercase();
    let identity = if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    };
    let mut trusted = false;
    let mut recorded = false;
    let mut ca = false;
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The ssh-key parser uses literal spaces as separators. Normalize
        // whitespace between fields and omit the optional free-form comment.
        let field_count = if line.starts_with('@') { 4 } else { 3 };
        let record = line
            .split_ascii_whitespace()
            .take(field_count)
            .collect::<Vec<_>>()
            .join(" ");
        let Ok(entry) = record.parse::<Entry>() else {
            return Trust::InvalidRecord;
        };
        if !matches_host(entry.host_patterns(), &identity) {
            continue;
        }
        let same_key = entry.public_key().key_data() == key.key_data();
        match entry.marker() {
            Some(Marker::Revoked) if same_key => return Trust::Revoked,
            Some(Marker::Revoked) => {}
            Some(Marker::CertAuthority) => ca = true,
            None => {
                recorded = true;
                trusted |= same_key;
            }
        }
    }
    if trusted {
        Trust::Trusted
    } else if recorded {
        Trust::Changed
    } else if ca {
        Trust::CertificateAuthorityOnly
    } else {
        Trust::Unknown
    }
}

fn matches_host(patterns: &HostPatterns, identity: &str) -> bool {
    match patterns {
        HostPatterns::HashedName { salt, hash } => {
            // OpenSSH uses HMAC-SHA1 for the known_hosts lookup token. This
            // lookup is independent of signature negotiation or validation.
            let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, salt);
            hmac::verify(&key, identity.as_bytes(), hash).is_ok()
        }
        HostPatterns::Patterns(patterns) => {
            let mut matched = false;
            for pattern in patterns {
                if let Some(negated) = pattern.strip_prefix('!') {
                    if wildcard(negated.as_bytes(), identity.as_bytes()) {
                        return false;
                    }
                } else {
                    matched |= wildcard(pattern.as_bytes(), identity.as_bytes());
                }
            }
            matched
        }
    }
}

pub(crate) fn wildcard(pattern: &[u8], value: &[u8]) -> bool {
    wildcard_match(pattern, value, true)
}

pub(crate) fn wildcard_match(mut pattern: &[u8], mut value: &[u8], ignore_case: bool) -> bool {
    let mut retry = None;
    while let Some((&byte, rest)) = value.split_first() {
        match pattern.split_first() {
            Some((b'*', tail)) => {
                pattern = tail;
                retry = Some((tail, value));
            }
            Some((&expected, tail))
                if expected == b'?'
                    || expected == byte
                    || (ignore_case && expected.eq_ignore_ascii_case(&byte)) =>
            {
                pattern = tail;
                value = rest;
            }
            _ => match retry {
                Some((tail, [_, remaining @ ..])) => {
                    pattern = tail;
                    value = remaining;
                    retry = Some((tail, remaining));
                }
                _ => return false,
            },
        }
    }
    pattern.iter().all(|&byte| byte == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_glob_respects_case_and_port() {
        assert!(wildcard(
            b"[*.Example.test]:2222",
            b"[box.example.test]:2222"
        ));
        assert!(!wildcard(
            b"[*.example.test]:2222",
            b"[box.example.test]:22"
        ));
        assert!(wildcard(b"a?b*c", b"axbyyyc"));
        assert!(!wildcard(b"a?b*c", b"abyyycd"));
    }

    #[test]
    fn negative_patterns_override_positive_patterns() {
        let patterns = "*.example.test,!excluded.example.test".parse().unwrap();
        assert!(matches_host(&patterns, "box.example.test"));
        assert!(!matches_host(&patterns, "excluded.example.test"));
    }

    #[test]
    fn hashed_identity_only_matches_the_recorded_host_and_port() {
        let salt = [0x42; 20];
        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &salt);
        let digest = hmac::sign(&key, b"[box.example.test]:2222");
        let patterns = HostPatterns::HashedName {
            salt: salt.to_vec(),
            hash: digest.as_ref().try_into().unwrap(),
        };
        assert!(matches_host(&patterns, "[box.example.test]:2222"));
        assert!(!matches_host(&patterns, "box.example.test"));
    }
}
