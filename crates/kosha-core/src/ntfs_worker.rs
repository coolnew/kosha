// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Fast NTFS worker messages. Production callers must authenticate both
//! endpoints before passing a transport to these functions.

use std::io::{self, Read, Write};
use std::path::Path;

const PROTOCOL: &[u8; 8] = b"KOSNTFS1";
const GUID_LEN: usize = 36;
const REQUEST_LEN: usize = PROTOCOL.len() + 1 + GUID_LEN;

#[cfg(all(windows, feature = "fast-ntfs-worker"))]
mod launch;
#[cfg(all(windows, feature = "fast-ntfs-worker"))]
pub use launch::{request_for_root, scan_with_worker, worker_available, worker_main};
#[cfg(windows)]
mod peer;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
pub use peer::{PackageMode, PipeEnd, verify_peer};
#[cfg(windows)]
pub use pipe::{LocalServer, open_local_client};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    drive: u8,
    guid: [u8; GUID_LEN],
}

impl Request {
    /// Construct a request from the selected drive and mount-manager identity.
    pub fn new(drive: u8, identity: &str) -> io::Result<Self> {
        let guid = identity
            .strip_prefix("windows:")
            .and_then(|value| value.as_bytes().try_into().ok())
            .ok_or_else(invalid_request)?;
        Self::validated(drive.to_ascii_uppercase(), guid)
    }

    fn validated(drive: u8, guid: [u8; GUID_LEN]) -> io::Result<Self> {
        if !drive.is_ascii_uppercase()
            || !guid.iter().enumerate().all(|(index, &byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
                }
            })
        {
            return Err(invalid_request());
        }
        Ok(Self { drive, guid })
    }

    pub fn read(mut input: impl Read) -> io::Result<Self> {
        let mut frame = [0; REQUEST_LEN];
        input.read_exact(&mut frame)?;
        if &frame[..PROTOCOL.len()] != PROTOCOL {
            return Err(invalid_request());
        }
        let mut guid = [0; GUID_LEN];
        guid.copy_from_slice(&frame[PROTOCOL.len() + 1..]);
        Self::validated(frame[PROTOCOL.len()], guid)
    }

    pub fn write(&self, mut output: impl Write) -> io::Result<()> {
        let mut frame = [0; REQUEST_LEN];
        frame[..PROTOCOL.len()].copy_from_slice(PROTOCOL);
        frame[PROTOCOL.len()] = self.drive;
        frame[PROTOCOL.len() + 1..].copy_from_slice(&self.guid);
        output.write_all(&frame)
    }

    fn root(&self) -> String {
        format!("{}:\\", char::from(self.drive))
    }

    /// Reuse the snapshot decoder and verify the result belongs to this drive
    /// and came from a completed MFT scan.
    pub fn read_result(
        &self,
        input: impl Read,
    ) -> Result<crate::SizeTree, crate::snapshot::ReadError> {
        let (meta, tree) = crate::snapshot::read(input)?;
        let root = self.root();
        if meta.root != Path::new(&root)
            || tree.stats().strategy != crate::Strategy::MasterFileTable
            || tree.is_incomplete()
        {
            return Err(crate::snapshot::ReadError::Damaged(
                "worker result does not match the requested MFT scan",
            ));
        }
        Ok(tree)
    }
}

fn invalid_request() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid fast NTFS worker request",
    )
}

/// Serve exactly one request over an already authenticated channel. All
/// persistent storage and presentation remain with the caller.
#[cfg(windows)]
pub fn scan_once(
    input: impl Read,
    mut output: impl Write,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), WorkerError> {
    let request = Request::read(input)?;
    let root = request.root();
    let guid = std::str::from_utf8(&request.guid).map_err(io::Error::other)?;
    let identity = format!("windows:{guid}");
    let tree = crate::scan_selected_volume(Path::new(&root), &identity, &|_| {}, cancelled)?;
    let meta = crate::snapshot::Meta::of(&tree, std::time::SystemTime::now(), "Fast NTFS scan");
    crate::snapshot::write(&tree, &meta, &mut output)?;
    output.flush()?;
    Ok(())
}

#[cfg(windows)]
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Scan(#[from] crate::ScanError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "windows:01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn request_round_trips_without_consuming_the_next_frame() {
        let request = Request::new(b'k', ID).unwrap();
        let mut output = Vec::new();
        request.write(&mut output).unwrap();
        output.extend_from_slice(b"x");
        let mut input = output.as_slice();
        assert_eq!(Request::read(&mut input).unwrap(), request);
        assert_eq!(input, b"x");
        assert_eq!(request.root(), "K:\\");
    }

    #[test]
    fn malformed_requests_are_rejected_before_scanning() {
        assert!(Request::new(b'?', ID).is_err());
        assert!(Request::new(b'K', "linux:123").is_err());
        let mut encoded = Vec::new();
        Request::new(b'K', ID).unwrap().write(&mut encoded).unwrap();
        assert!(Request::read(&encoded[..REQUEST_LEN - 1]).is_err());
        encoded[0] = b'X';
        assert!(Request::read(encoded.as_slice()).is_err());
        encoded[0] = PROTOCOL[0];
        encoded[PROTOCOL.len() + 1] = b'/';
        assert!(Request::read(encoded.as_slice()).is_err());
    }
}
