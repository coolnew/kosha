// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Remote scanning transport for explicitly enabled clients.

#[cfg(feature = "ssh")]
mod auth;
#[cfg(feature = "ssh")]
mod channel;
#[cfg(feature = "ssh")]
mod connect;
#[cfg(feature = "ssh")]
mod host_trust;
#[cfg(feature = "ssh")]
pub mod key_input;
#[cfg(feature = "ssh")]
mod local_files;
#[cfg(feature = "ssh")]
mod preflight;
#[cfg(feature = "ssh")]
mod rsa;
#[cfg(feature = "ssh")]
mod runtime;
#[cfg(feature = "ssh")]
mod ssh_config;
#[cfg(feature = "ssh")]
mod target;
#[cfg(all(feature = "ssh", unix))]
pub mod unix_prompt;
#[cfg(all(feature = "ssh", any(unix, windows)))]
pub use auth::SelectedAgentKey;
#[cfg(all(feature = "ssh", unix))]
pub use auth::authenticate_unix_agent;
#[cfg(feature = "ssh")]
pub use auth::{AuthenticationError, authenticate_key, try_authenticate_key};
#[cfg(feature = "ssh")]
pub use channel::{ScanError, scan_channel, scan_session};
#[cfg(feature = "ssh")]
pub use connect::{ConnectError, HostVerifier, connect};
#[cfg(feature = "ssh")]
pub use host_trust::{Trust, check_host_key};
#[cfg(feature = "ssh")]
pub use local_files::{LocalFileError, load_known_hosts, load_ssh_config};
#[cfg(feature = "ssh")]
pub use preflight::{PreflightError, PreparedTarget, prepare_local_target, prepare_target};
#[cfg(feature = "ssh")]
pub use rsa::{NoRsaSha2, rsa_signature_hash};
#[cfg(feature = "ssh")]
pub use runtime::{ScanCancellation, ScanHandle, run_ssh, scan_cancellation};
#[cfg(feature = "ssh")]
pub use ssh_config::{ConfigError, SshConfig, parse_ssh_config};
#[cfg(feature = "ssh")]
pub use target::{ResolvedTarget, TargetError, TargetOptions, resolve_target};
