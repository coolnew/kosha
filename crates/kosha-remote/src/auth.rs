// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

#[cfg(any(unix, windows))]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use russh::client;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg};

use crate::{HostVerifier, NoRsaSha2, rsa_signature_hash};

const AUTH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum AuthenticationError {
    #[error("SSH public-key authentication timed out")]
    Timeout,
    #[error("SSH key algorithm is unsupported")]
    Algorithm,
    #[error(transparent)]
    Rsa(#[from] NoRsaSha2),
    #[error("SSH server refused the public key")]
    Rejected,
    #[error("SSH authentication failed: {0}")]
    Ssh(#[from] russh::Error),
    #[cfg(any(unix, windows))]
    #[error("SSH agent communication failed")]
    Agent,
    #[cfg(any(unix, windows))]
    #[error("the selected key is unavailable in the SSH agent")]
    AgentIdentity,
    #[cfg(unix)]
    #[error("SSH_AUTH_SOCK is unavailable")]
    AgentSocket,
    #[cfg(any(unix, windows))]
    #[error(transparent)]
    AgentKey(#[from] crate::key_input::Error),
}

/// One explicitly selected local agent identity. Loading does no network I/O.
#[cfg(any(unix, windows))]
pub struct SelectedAgentKey {
    endpoint: PathBuf,
    key: russh::keys::PublicKey,
}

#[cfg(any(unix, windows))]
impl SelectedAgentKey {
    pub fn load(enabled: bool, path: &Path) -> Result<Self, AuthenticationError> {
        let key = crate::key_input::load_public_key(enabled, path)?;
        #[cfg(unix)]
        let endpoint = std::env::var_os("SSH_AUTH_SOCK")
            .filter(|socket| !socket.is_empty())
            .map(PathBuf::from)
            .ok_or(AuthenticationError::AgentSocket)?;
        #[cfg(windows)]
        let endpoint = PathBuf::from(r"\\.\pipe\openssh-ssh-agent");
        Ok(Self { endpoint, key })
    }

    pub async fn authenticate(
        self,
        session: client::Handle<HostVerifier>,
        user: &str,
    ) -> Result<client::Handle<HostVerifier>, AuthenticationError> {
        authenticate_agent(session, user, &self.endpoint, &self.key).await
    }
}

/// Authenticate with one decoded key on a host-verified session.
/// A timeout consumes the session so a partially completed exchange cannot be reused.
pub async fn authenticate_key(
    session: client::Handle<HostVerifier>,
    user: &str,
    key: Arc<PrivateKey>,
) -> Result<client::Handle<HostVerifier>, AuthenticationError> {
    let (session, accepted) = try_authenticate_key(session, user, key).await?;
    if accepted {
        Ok(session)
    } else {
        Err(AuthenticationError::Rejected)
    }
}

/// Return the session after a completed attempt so a rejected key can be
/// followed by another. Transport errors and timeouts consume the session.
pub async fn try_authenticate_key(
    mut session: client::Handle<HostVerifier>,
    user: &str,
    key: Arc<PrivateKey>,
) -> Result<(client::Handle<HostVerifier>, bool), AuthenticationError> {
    tokio::time::timeout(AUTH_TIMEOUT, async {
        let hash = signature_hash(&session, key.algorithm()).await?;
        let key = PrivateKeyWithHashAlg::new(key, hash);
        match session.authenticate_publickey(user, key).await? {
            client::AuthResult::Success => Ok((session, true)),
            client::AuthResult::Failure {
                remaining_methods,
                partial_success: false,
            } if remaining_methods
                .iter()
                .any(|method| <&str>::from(method) == "publickey") =>
            {
                Ok((session, false))
            }
            client::AuthResult::Failure { .. } => Err(AuthenticationError::Rejected),
        }
    })
    .await
    .map_err(|_| AuthenticationError::Timeout)?
}

/// Ask a local Unix agent to sign for one explicitly selected bare public key.
/// The caller supplies the socket path after checking that SSH is enabled.
#[cfg(unix)]
pub async fn authenticate_unix_agent(
    session: client::Handle<HostVerifier>,
    user: &str,
    socket: &Path,
    selected_key: &russh::keys::PublicKey,
) -> Result<client::Handle<HostVerifier>, AuthenticationError> {
    authenticate_agent(session, user, socket, selected_key).await
}

#[cfg(any(unix, windows))]
async fn authenticate_agent(
    mut session: client::Handle<HostVerifier>,
    user: &str,
    endpoint: &Path,
    selected_key: &russh::keys::PublicKey,
) -> Result<client::Handle<HostVerifier>, AuthenticationError> {
    use russh::keys::agent::{AgentIdentity, client::AgentClient};

    tokio::time::timeout(AUTH_TIMEOUT, async {
        let hash = signature_hash(&session, selected_key.algorithm()).await?;
        #[cfg(unix)]
        let agent = AgentClient::connect_uds(endpoint).await;
        #[cfg(windows)]
        let agent = AgentClient::connect_named_pipe(endpoint.as_os_str()).await;
        let mut agent = agent.map_err(|_| AuthenticationError::Agent)?;
        let identities = agent.request_identities().await.map_err(|_| AuthenticationError::Agent)?;
        let identity = identities.into_iter().find(|identity| {
            matches!(identity, AgentIdentity::PublicKey { key, .. } if key.key_data() == selected_key.key_data())
        }).ok_or(AuthenticationError::AgentIdentity)?;
        if !session.authenticate_publickey_with(user, identity.public_key().into_owned(), hash, &mut agent)
            .await.map_err(|_| AuthenticationError::Agent)?.success() {
            return Err(AuthenticationError::Rejected);
        }
        Ok(session)
    })
    .await
    .map_err(|_| AuthenticationError::Timeout)?
}

async fn signature_hash(
    session: &client::Handle<HostVerifier>,
    algorithm: Algorithm,
) -> Result<Option<HashAlg>, AuthenticationError> {
    match algorithm {
        Algorithm::Rsa { .. } => Ok(Some(rsa_signature_hash(
            session.best_supported_rsa_hash().await?,
        )?)),
        Algorithm::Ed25519 | Algorithm::Ecdsa { .. } => Ok(None),
        _ => Err(AuthenticationError::Algorithm),
    }
}
