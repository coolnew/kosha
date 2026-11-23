// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Bounded handshake, request and progress messages for the remote scan protocol.

use std::io::{self, BufRead, Read, Write};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod reply;
pub use reply::{ReplyError, ReplyReceiver};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 32 << 10;
pub const MAX_PATH_BYTES: usize = 4096;
/// Maximum progress line length, including the terminating LF.
pub const MAX_PROGRESS_LINE_BYTES: usize = 1024;
pub const SNAPSHOT_V2: &str = "snapshot-v2";

const MAX_TOOL_BYTES: usize = 128;
const MAX_CAPABILITIES: usize = 16;
const MAX_CAPABILITY_BYTES: usize = 48;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol: u16,
    pub tool: String,
    pub capabilities: Vec<String>,
}

impl Hello {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            tool: tool.into(),
            capabilities: vec![SNAPSHOT_V2.into()],
        }
    }

    /// Check that both ends can exchange a version 2 scan snapshot.
    pub fn require_compatible(&self, peer: &Self) -> Result<(), Error> {
        self.validate()?;
        peer.validate()?;
        if self.protocol != PROTOCOL_VERSION || peer.protocol != self.protocol {
            return Err(Error::Protocol {
                local: self.protocol,
                local_tool: self.tool.clone(),
                peer: peer.protocol,
                peer_tool: peer.tool.clone(),
            });
        }
        if [self, peer].into_iter().any(|end| {
            !end.capabilities
                .iter()
                .any(|capability| capability == SNAPSHOT_V2)
        }) {
            return Err(Error::MissingSnapshotCapability);
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), Error> {
        if self.tool.is_empty()
            || self.tool.len() > MAX_TOOL_BYTES
            || !self
                .tool
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(Error::InvalidHello(
                "tool must be 1–128 printable ASCII bytes",
            ));
        }
        if self.capabilities.len() > MAX_CAPABILITIES
            || self.capabilities.iter().any(|capability| {
                capability.is_empty()
                    || capability.len() > MAX_CAPABILITY_BYTES
                    || !capability
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err(Error::InvalidHello("invalid capability list"));
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// A UTF-8 path for the server OS. The codec never opens or resolves it.
    pub path: String,
    /// `null` requests the server's default depth behavior.
    #[serde(deserialize_with = "read_depth")]
    pub max_depth: Option<u8>,
    pub owners: bool,
    pub cross_filesystems: bool,
    pub dedup_hardlinks: bool,
}

fn read_depth<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<u8>, D::Error> {
    Option::deserialize(de)
}

impl Request {
    fn validate(&self) -> Result<(), Error> {
        if self.path.is_empty() || self.path.len() > MAX_PATH_BYTES || self.path.contains('\0') {
            return Err(Error::InvalidPath);
        }
        Ok(())
    }
}

/// Counts measured by the peer during the scan. No peer text is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub files: u64,
    pub directories: u64,
}

/// Incremental decoder for a scan's SSH stderr chunks.
pub struct ProgressDecoder {
    pending: Vec<u8>,
    rate: ProgressRate,
}

impl ProgressDecoder {
    pub fn new(now: Instant) -> Self {
        Self {
            pending: Vec::new(),
            rate: ProgressRate::new(now),
        }
    }

    /// Feed a chunk at its receive time. Complete lines are delivered in order.
    /// On error the caller must close the channel and discard this decoder.
    pub fn push(
        &mut self,
        chunk: &[u8],
        received_at: Instant,
        mut emit: impl FnMut(Progress),
    ) -> Result<(), Error> {
        for part in chunk.split_inclusive(|&byte| byte == b'\n') {
            let remaining = MAX_PROGRESS_LINE_BYTES - self.pending.len();
            let complete = part.ends_with(b"\n");
            if part.len() > remaining || (part.len() == remaining && !complete) {
                return Err(Error::ProgressLineTooLong);
            }
            if complete {
                self.pending.extend_from_slice(&part[..part.len() - 1]);
                let progress = decode_progress(&self.pending, &mut self.rate, received_at)?;
                self.pending.clear();
                emit(progress);
            } else {
                self.pending.extend_from_slice(part);
            }
        }
        Ok(())
    }

    /// Check that stderr ended at a line boundary.
    pub fn finish(self) -> Result<(), Error> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(Error::Truncated)
        }
    }
}

/// Per-scan receive budget for stderr progress lines. A burst of four lines
/// is allowed, with one line replenished every 250 ms.
pub struct ProgressRate {
    credit: Duration,
    last: Instant,
}

impl ProgressRate {
    const CAPACITY: Duration = Duration::from_secs(1);
    const COST: Duration = Duration::from_millis(250);

    pub fn new(now: Instant) -> Self {
        Self {
            credit: Self::CAPACITY,
            last: now,
        }
    }

    /// Charge each received line, including malformed lines, before parsing.
    /// The transport must close the channel on error.
    fn charge_line(&mut self, now: Instant) -> Result<(), Error> {
        self.credit = self
            .credit
            .saturating_add(now.saturating_duration_since(self.last))
            .min(Self::CAPACITY);
        self.last = self.last.max(now);
        self.credit = self
            .credit
            .checked_sub(Self::COST)
            .ok_or(Error::ProgressRateExceeded)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("remote scan frame is truncated")]
    Truncated,
    #[error("remote scan frame size must be 1–{MAX_FRAME_BYTES} bytes (got {0})")]
    FrameSize(usize),
    #[error("remote scan frame is not valid for this message type")]
    InvalidJson,
    #[error("remote scan progress line exceeds {MAX_PROGRESS_LINE_BYTES} bytes")]
    ProgressLineTooLong,
    #[error("remote scan sent progress lines too quickly")]
    ProgressRateExceeded,
    #[error("remote scan stream failed: {0}")]
    Io(io::Error),
    #[error("invalid remote scan hello: {0}")]
    InvalidHello(&'static str),
    #[error(
        "remote scan protocol mismatch: local {local} ({local_tool}), peer {peer} ({peer_tool})"
    )]
    Protocol {
        local: u16,
        local_tool: String,
        peer: u16,
        peer_tool: String,
    },
    #[error("both peers must advertise snapshot-v2")]
    MissingSnapshotCapability,
    #[error("remote scan path must contain 1–{MAX_PATH_BYTES} UTF-8 bytes and no NUL")]
    InvalidPath,
}

pub fn read_hello(input: &mut impl Read) -> Result<Hello, Error> {
    let hello: Hello = read_frame(input)?;
    hello.validate()?;
    Ok(hello)
}

pub fn write_hello(output: &mut impl Write, hello: &Hello) -> Result<(), Error> {
    hello.validate()?;
    write_frame(output, hello)
}

pub fn read_request(input: &mut impl Read) -> Result<Request, Error> {
    let request: Request = read_frame(input)?;
    request.validate()?;
    Ok(request)
}

/// Read a client's hello and request after sending the server hello. Refuse
/// incompatible peers before consuming their request bytes.
pub fn read_negotiated_request(input: &mut impl Read, local: &Hello) -> Result<Request, Error> {
    let peer = read_hello(input)?;
    local.require_compatible(&peer)?;
    read_request(input)
}

pub fn write_request(output: &mut impl Write, request: &Request) -> Result<(), Error> {
    request.validate()?;
    write_frame(output, request)
}

/// Read one LF-terminated stderr message using the scan's shared rate budget.
/// The caller closes the channel on error and enforces transport timeouts.
pub fn read_progress(
    input: &mut impl BufRead,
    rate: &mut ProgressRate,
) -> Result<Option<Progress>, Error> {
    read_progress_at(input, rate, Instant::now)
}

fn read_progress_at(
    input: &mut impl BufRead,
    rate: &mut ProgressRate,
    now: impl FnOnce() -> Instant,
) -> Result<Option<Progress>, Error> {
    let mut line = Vec::new();
    input
        .take(MAX_PROGRESS_LINE_BYTES as u64)
        .read_until(b'\n', &mut line)
        .map_err(Error::Io)?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.pop() != Some(b'\n') {
        return Err(if line.len() + 1 == MAX_PROGRESS_LINE_BYTES {
            Error::ProgressLineTooLong
        } else {
            Error::Truncated
        });
    }
    decode_progress(&line, rate, now()).map(Some)
}

fn decode_progress(line: &[u8], rate: &mut ProgressRate, now: Instant) -> Result<Progress, Error> {
    rate.charge_line(now)?;
    serde_json::from_slice(line).map_err(|_| Error::InvalidJson)
}

pub fn write_progress(output: &mut impl Write, progress: &Progress) -> Result<(), Error> {
    let mut line = serde_json::to_vec(progress).map_err(|_| Error::InvalidJson)?;
    line.push(b'\n');
    output.write_all(&line).map_err(Error::Io)
}

fn read_frame<T: DeserializeOwned>(input: &mut impl Read) -> Result<T, Error> {
    let read_error = |error: io::Error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            Error::Io(error)
        }
    };
    let mut header = [0; 4];
    input.read_exact(&mut header).map_err(read_error)?;
    let size = u32::from_be_bytes(header) as usize;
    check_frame_size(size)?;
    let mut body = vec![0; size];
    input.read_exact(&mut body).map_err(read_error)?;
    serde_json::from_slice(&body).map_err(|_| Error::InvalidJson)
}

fn write_frame(output: &mut impl Write, value: &impl Serialize) -> Result<(), Error> {
    let body = serde_json::to_vec(value).map_err(|_| Error::InvalidJson)?;
    check_frame_size(body.len())?;
    output
        .write_all(&(body.len() as u32).to_be_bytes())
        .map_err(Error::Io)?;
    output.write_all(&body).map_err(Error::Io)
}

fn check_frame_size(size: usize) -> Result<(), Error> {
    if (1..=MAX_FRAME_BYTES).contains(&size) {
        Ok(())
    } else {
        Err(Error::FrameSize(size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn incompatible_client_leaves_the_request_unread() {
        let server = Hello::new("server");
        let mut peer = Hello::new("client");
        peer.protocol += 1;
        let mut bytes = Vec::new();
        write_hello(&mut bytes, &peer).unwrap();
        let hello_len = bytes.len();
        bytes.extend_from_slice(b"unread");
        let mut input = Cursor::new(bytes);
        assert!(matches!(
            read_negotiated_request(&mut input, &server),
            Err(Error::Protocol { .. })
        ));
        assert_eq!(input.position(), hello_len as u64);
    }

    #[test]
    fn progress_decoder_accepts_every_chunk_boundary() {
        let now = Instant::now();
        let first = Progress {
            files: 1,
            directories: 2,
        };
        let second = Progress {
            files: 3,
            directories: 4,
        };
        let mut wire = Vec::new();
        write_progress(&mut wire, &first).unwrap();
        write_progress(&mut wire, &second).unwrap();
        for split in 0..=wire.len() {
            let mut decoder = ProgressDecoder::new(now);
            let mut seen = Vec::new();
            decoder
                .push(&wire[..split], now, |value| seen.push(value))
                .unwrap();
            decoder
                .push(&wire[split..], now, |value| seen.push(value))
                .unwrap();
            decoder.finish().unwrap();
            assert_eq!(seen, [first, second]);
        }
    }

    #[test]
    fn progress_decoder_rejects_partial_eof_oversized_lines_and_bursts() {
        let now = Instant::now();
        let mut decoder = ProgressDecoder::new(now);
        decoder
            .push(&[b' '; MAX_PROGRESS_LINE_BYTES - 1], now, |_| {})
            .unwrap();
        assert!(matches!(
            decoder.push(b" ", now, |_| {}),
            Err(Error::ProgressLineTooLong)
        ));

        let mut decoder = ProgressDecoder::new(now);
        decoder.push(b"{", now, |_| {}).unwrap();
        assert!(matches!(decoder.finish(), Err(Error::Truncated)));

        let mut decoder = ProgressDecoder::new(now);
        let line = b"{\"files\":0,\"directories\":1}\n";
        let mut delivered = 0;
        assert!(matches!(
            decoder.push(&line.repeat(5), now, |_| delivered += 1),
            Err(Error::ProgressRateExceeded)
        ));
        assert_eq!(delivered, 4);
    }

    #[test]
    fn progress_rate_bounds_bursts_and_refills_at_exact_intervals() {
        let start = Instant::now();
        let mut rate = ProgressRate::new(start);
        for _ in 0..4 {
            rate.charge_line(start).unwrap();
        }
        assert!(matches!(
            rate.charge_line(start + Duration::from_millis(249)),
            Err(Error::ProgressRateExceeded)
        ));
        let first_refill = start + Duration::from_millis(250);
        rate.charge_line(first_refill).unwrap();
        for time in [start, first_refill] {
            assert!(matches!(
                rate.charge_line(time),
                Err(Error::ProgressRateExceeded)
            ));
        }
        let after_idle = start + Duration::from_secs(60);
        for _ in 0..4 {
            rate.charge_line(after_idle).unwrap();
        }
        assert!(matches!(
            rate.charge_line(after_idle),
            Err(Error::ProgressRateExceeded)
        ));
    }

    #[test]
    fn progress_round_trips_without_reading_the_next_line() {
        let progress = Progress {
            files: u64::MAX,
            directories: 2,
        };
        let mut stream = Vec::new();
        write_progress(&mut stream, &progress).unwrap();
        write_progress(&mut stream, &progress).unwrap();
        let mut input = Cursor::new(stream);
        let mut rate = ProgressRate::new(Instant::now());
        assert_eq!(
            read_progress(&mut input, &mut rate).unwrap(),
            Some(progress)
        );
        assert_eq!(
            read_progress(&mut input, &mut rate).unwrap(),
            Some(progress)
        );
        assert_eq!(read_progress(&mut input, &mut rate).unwrap(), None);
    }

    #[test]
    fn progress_enforces_the_line_limit_and_discards_peer_text() {
        let mut line = br#"{"files":0,"directories":0}"#.to_vec();
        line.resize(MAX_PROGRESS_LINE_BYTES - 1, b' ');
        line.push(b'\n');
        let mut rate = ProgressRate::new(Instant::now());
        assert!(matches!(
            read_progress(&mut line.as_slice(), &mut rate),
            Ok(Some(_))
        ));
        line.insert(0, b' ');
        let mut input = Cursor::new(line);
        assert!(matches!(
            read_progress(&mut input, &mut rate),
            Err(Error::ProgressLineTooLong)
        ));
        assert_eq!(input.position(), MAX_PROGRESS_LINE_BYTES as u64);
        assert!(matches!(
            read_progress(&mut &b"{"[..], &mut rate),
            Err(Error::Truncated)
        ));
        let mut unknown_field =
            b"{\"files\":0,\"directories\":0,\"message\":\"peer text\"}\n".as_slice();
        assert!(matches!(
            read_progress(&mut unknown_field, &mut rate),
            Err(Error::InvalidJson)
        ));
    }

    #[test]
    fn progress_charges_complete_lines_before_decoding_and_leaves_eof_free() {
        let now = Instant::now();
        let mut rate = ProgressRate::new(now);
        for _ in 0..4 {
            assert!(matches!(
                read_progress_at(&mut b"{\n".as_slice(), &mut rate, || now),
                Err(Error::InvalidJson)
            ));
        }
        assert_eq!(
            read_progress_at(&mut b"".as_slice(), &mut rate, || now).unwrap(),
            None
        );
        assert!(matches!(
            read_progress_at(&mut b"{\n".as_slice(), &mut rate, || now),
            Err(Error::ProgressRateExceeded)
        ));
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(body);
        bytes
    }

    fn sample(path: &str) -> Request {
        Request {
            path: path.into(),
            max_depth: Some(3),
            owners: true,
            cross_filesystems: false,
            dedup_hardlinks: true,
        }
    }

    #[test]
    fn round_trips_independent_messages_and_os_paths() {
        for path in [
            r"\\server\share\name with spaces",
            "/tmp/space ' quote ; 文",
        ] {
            let hello = Hello::new("kosha 0.14.0");
            let request = sample(path);
            let mut bytes = Vec::new();
            write_hello(&mut bytes, &hello).unwrap();
            write_request(&mut bytes, &request).unwrap();
            let mut input = bytes.as_slice();
            let peer = read_hello(&mut input).unwrap();
            hello.require_compatible(&peer).unwrap();
            assert_eq!(read_request(&mut input).unwrap(), request);
            assert!(input.is_empty());
        }
    }

    #[test]
    fn mismatched_protocols_report_both_versions_and_capability_is_required() {
        let local = Hello::new("local");
        let mut peer = Hello::new("peer");
        peer.protocol += 1;
        assert_eq!(
            local.require_compatible(&peer).unwrap_err().to_string(),
            "remote scan protocol mismatch: local 1 (local), peer 2 (peer)"
        );
        peer.protocol = PROTOCOL_VERSION;
        peer.capabilities.clear();
        assert!(matches!(
            local.require_compatible(&peer),
            Err(Error::MissingSnapshotCapability)
        ));
        assert!(matches!(
            write_hello(&mut Vec::new(), &Hello::new("\x1b")),
            Err(Error::InvalidHello(_))
        ));
    }

    #[test]
    fn checks_length_before_consuming_or_allocating_payload() {
        for size in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
            let mut data = size.to_be_bytes().to_vec();
            data.extend_from_slice(b"not read");
            let mut input = Cursor::new(data);
            assert!(matches!(read_request(&mut input), Err(Error::FrameSize(_))));
            assert_eq!(input.position(), 4);
        }
        let mut bytes = Vec::new();
        write_request(&mut bytes, &sample("abc")).unwrap();
        for end in 0..bytes.len() {
            assert!(matches!(
                read_request(&mut &bytes[..end]),
                Err(Error::Truncated)
            ));
        }
    }

    #[test]
    fn validates_options_before_writing_anything() {
        let mut bytes = Vec::new();
        for path in ["", "bad\0path", &"x".repeat(MAX_PATH_BYTES + 1)] {
            assert!(matches!(
                write_request(&mut bytes, &sample(path)),
                Err(Error::InvalidPath)
            ));
            assert!(bytes.is_empty());
        }
        // JSON escaping at the path limit must still fit the frame.
        let request = sample(&"\u{1}".repeat(MAX_PATH_BYTES));
        write_request(&mut bytes, &request).unwrap();
        assert_eq!(read_request(&mut bytes.as_slice()).unwrap(), request);
    }

    #[test]
    fn refuses_ambiguous_or_unexpected_request_fields_and_trailing_json() {
        for body in [
            r#"{"path":"/","owners":false,"cross_filesystems":false,"dedup_hardlinks":true}"#,
            r#"{"path":"/","max_depth":256,"owners":false,"cross_filesystems":false,"dedup_hardlinks":true}"#,
            r#"{"path":"/","path":"/other","max_depth":null,"owners":false,"cross_filesystems":false,"dedup_hardlinks":true}"#,
            r#"{"path":"/","max_depth":null,"owners":false,"cross_filesystems":false,"dedup_hardlinks":true,"command":"ls"}"#,
            r#"{"path":"/","max_depth":null,"owners":false,"cross_filesystems":false,"dedup_hardlinks":true}[]"#,
        ] {
            assert!(
                matches!(
                    read_request(&mut frame(body.as_bytes()).as_slice()),
                    Err(Error::InvalidJson)
                ),
                "{body}"
            );
        }
        let body = br#"{"path":"/","max_depth":null,"owners":false,"cross_filesystems":false,"dedup_hardlinks":true}"#;
        assert_eq!(
            read_request(&mut frame(body).as_slice()).unwrap().max_depth,
            None
        );
        let hostile = frame(br#"{"\u001b[2J":"unexpected"}"#);
        let error = read_request(&mut hostile.as_slice()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "remote scan frame is not valid for this message type"
        );
        assert!(std::error::Error::source(&error).is_none());
    }
}
