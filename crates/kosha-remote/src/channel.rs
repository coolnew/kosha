// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::time::{Duration, Instant};

use kosha_core::snapshot::{ReadError, ReadLimits, RemoteRecord};
use kosha_scan_wire::{self as wire, Hello, Progress, ReplyError, ReplyReceiver, Request};
use russh::{Channel, ChannelMsg, client};
use tokio::time::timeout;

use crate::HostVerifier;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const REPLY_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const COMMAND: &str = "kosha serve-scan";

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("SSH scanning is disabled")]
    Disabled,
    #[error(transparent)]
    Wire(#[from] wire::Error),
    #[error(transparent)]
    Reply(#[from] ReplyError),
    #[error(transparent)]
    Snapshot(#[from] ReadError),
    #[error("SSH channel operation failed")]
    Channel,
    #[error("unexpected SSH event during remote scan")]
    UnexpectedEvent,
    #[error("remote scan handshake timed out")]
    HandshakeTimeout,
    #[error("remote scan stopped sending data")]
    IdleTimeout,
    #[error("remote scan exceeded the reply deadline")]
    ReplyTimeout,
    #[error("remote snapshot decoding worker failed")]
    Worker,
}

/// Open a session channel and run the fixed read-only scan command.
pub async fn scan_session(
    enabled: bool,
    session: &client::Handle<HostVerifier>,
    request: &Request,
    progress: impl FnMut(Progress),
) -> Result<RemoteRecord, ScanError> {
    if !enabled {
        return Err(ScanError::Disabled);
    }
    let channel = timeout(HANDSHAKE_TIMEOUT, session.channel_open_session())
        .await
        .map_err(|_| ScanError::HandshakeTimeout)?
        .map_err(|_| ScanError::Channel)?;
    scan_channel(channel, request, progress).await
}

/// Run one scan on an authenticated session channel. The caller must decide
/// network policy and verify the host before opening the channel.
pub async fn scan_channel(
    mut channel: Channel<client::Msg>,
    request: &Request,
    progress: impl FnMut(Progress),
) -> Result<RemoteRecord, ScanError> {
    let limits = ReadLimits::default();
    let result = receive(&mut channel, request, limits, progress).await;
    if result.is_err() {
        let _ = timeout(CLOSE_TIMEOUT, channel.close()).await;
    }
    let reply = result?;
    tokio::task::spawn_blocking(move || RemoteRecord::read(reply.as_slice(), limits))
        .await
        .map_err(|_| ScanError::Worker)?
        .map_err(ScanError::from)
}

async fn receive(
    channel: &mut Channel<client::Msg>,
    request: &Request,
    limits: ReadLimits,
    mut progress: impl FnMut(Progress),
) -> Result<Vec<u8>, ScanError> {
    timeout(HANDSHAKE_TIMEOUT, handshake(channel, request))
        .await
        .map_err(|_| ScanError::HandshakeTimeout)??;
    let mut reply = ReplyReceiver::new(limits.max_wire_bytes as usize, Instant::now());
    timeout(REPLY_TIMEOUT, async {
        loop {
            let event = timeout(IDLE_TIMEOUT, channel.wait())
                .await
                .map_err(|_| ScanError::IdleTimeout)?;
            match event {
                Some(ChannelMsg::Data { data }) => reply.stdout(&data)?,
                Some(ChannelMsg::ExtendedData { ext: 1, data }) => {
                    reply.stderr(&data, Instant::now(), &mut progress)?;
                }
                Some(ChannelMsg::Eof) => reply.eof()?,
                Some(ChannelMsg::ExitStatus { exit_status }) => reply.exit_status(exit_status)?,
                Some(ChannelMsg::Close) | None => return Ok(reply.finish()?),
                _ => return Err(ScanError::UnexpectedEvent),
            }
        }
    })
    .await
    .map_err(|_| ScanError::ReplyTimeout)?
}

async fn handshake(channel: &mut Channel<client::Msg>, request: &Request) -> Result<(), ScanError> {
    let local = Hello::new(concat!("kosha ", env!("CARGO_PKG_VERSION")));
    let mut outgoing = Vec::new();
    wire::write_hello(&mut outgoing, &local)?;
    wire::write_request(&mut outgoing, request)?;
    channel
        .exec(true, COMMAND)
        .await
        .map_err(|_| ScanError::Channel)?;
    if !matches!(channel.wait().await, Some(ChannelMsg::Success)) {
        return Err(ScanError::UnexpectedEvent);
    }
    let mut incoming = Vec::new();
    let peer = loop {
        let Some(ChannelMsg::Data { data }) = channel.wait().await else {
            return Err(ScanError::UnexpectedEvent);
        };
        if data.len() > 4 + wire::MAX_FRAME_BYTES - incoming.len() {
            return Err(ScanError::UnexpectedEvent);
        }
        incoming.extend_from_slice(&data);
        let mut input = incoming.as_slice();
        match wire::read_hello(&mut input) {
            Ok(peer) if input.is_empty() => break peer,
            Ok(_) => return Err(ScanError::UnexpectedEvent),
            Err(wire::Error::Truncated) => {}
            Err(error) => return Err(error.into()),
        }
    };
    local.require_compatible(&peer)?;
    channel
        .data_bytes(outgoing)
        .await
        .map_err(|_| ScanError::Channel)?;
    channel.eof().await.map_err(|_| ScanError::Channel)
}
