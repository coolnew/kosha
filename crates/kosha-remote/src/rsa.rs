// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use russh::keys::HashAlg;

/// Select a SHA-2 signature for RSA user authentication. An absent
/// server-sig-algs extension allows a SHA-256 offer with no SHA-1 fallback.
pub fn rsa_signature_hash(advertised: Option<Option<HashAlg>>) -> Result<HashAlg, NoRsaSha2> {
    match advertised {
        Some(Some(hash @ (HashAlg::Sha256 | HashAlg::Sha512))) => Ok(hash),
        None => Ok(HashAlg::Sha256),
        _ => Err(NoRsaSha2),
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct NoRsaSha2;

impl std::fmt::Display for NoRsaSha2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("server does not advertise an RSA/SHA-2 signature")
    }
}

impl std::error::Error for NoRsaSha2 {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_sha2_when_the_server_advertises_signature_algorithms() {
        assert_eq!(rsa_signature_hash(None), Ok(HashAlg::Sha256));
        assert_eq!(rsa_signature_hash(Some(None)), Err(NoRsaSha2));
        for hash in [HashAlg::Sha256, HashAlg::Sha512] {
            assert_eq!(rsa_signature_hash(Some(Some(hash))), Ok(hash));
        }
    }
}
