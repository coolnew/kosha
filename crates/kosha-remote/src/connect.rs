// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::sync::Arc;
use std::time::Duration;

use russh::{client, keys::PublicKeyOrCertificate};

use crate::{Trust, check_host_key};

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("SSH scanning is disabled")]
    Disabled,
    #[error("SSH connection timed out")]
    Timeout,
    #[error("SSH server presented an unsupported host certificate")]
    Certificate,
    #[error("SSH host key refused: {0:?}")]
    HostKey(Trust),
    #[error("SSH connection failed: {0}")]
    Ssh(#[from] russh::Error),
}

/// Verifies the server key during initial key exchange and rekeying.
pub struct HostVerifier {
    host: String,
    port: u16,
    known_hosts: String,
}

impl client::Handler for HostVerifier {
    type Error = ConnectError;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, ConnectError> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Err(ConnectError::Certificate);
        };
        match check_host_key(&self.known_hosts, &self.host, self.port, key) {
            Trust::Trusted => Ok(true),
            refused => Err(ConnectError::HostKey(refused)),
        }
    }
}

/// Establishes a host-verified SSH session before user authentication.
/// Callers must also enforce the setting before loading local SSH configuration.
pub async fn connect(
    enabled: bool,
    host: &str,
    port: u16,
    known_hosts: String,
    config: Arc<client::Config>,
) -> Result<client::Handle<HostVerifier>, ConnectError> {
    if !enabled {
        return Err(ConnectError::Disabled);
    }
    let verifier = HostVerifier {
        host: host.into(),
        port,
        known_hosts,
    };
    tokio::time::timeout(
        Duration::from_secs(15),
        client::connect(config, (host, port), verifier),
    )
    .await
    .map_err(|_| ConnectError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_connection_requires_no_tokio_runtime() {
        let mut future = std::pin::pin!(connect(
            false,
            "localhost",
            22,
            String::new(),
            Arc::new(client::Config::default()),
        ));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            future.as_mut().poll(&mut context),
            std::task::Poll::Ready(Err(ConnectError::Disabled))
        ));
    }
}
