// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::time::Instant;

use crate::{Error, Progress, ProgressDecoder};

#[derive(Debug, thiserror::Error)]
pub enum ReplyError {
    #[error(transparent)]
    Progress(#[from] Error),
    #[error("remote scan reply exceeds {0} bytes")]
    SnapshotLimit(usize),
    #[error("unexpected remote scan channel event")]
    UnexpectedEvent,
    #[error("remote scan exited with status {0}")]
    Exit(u32),
    #[error("remote scan reply ended before successful completion")]
    Incomplete,
}

/// Collects reply events after the handshake. Discard this receiver and close
/// the channel on any error. A successful reply still needs snapshot decoding
/// and validation before it can be presented.
pub struct ReplyReceiver {
    snapshot: Vec<u8>,
    max_bytes: usize,
    progress: Option<ProgressDecoder>,
    succeeded: bool,
    failed: bool,
}

impl ReplyReceiver {
    pub fn new(max_bytes: usize, now: Instant) -> Self {
        Self {
            snapshot: Vec::new(),
            max_bytes,
            progress: Some(ProgressDecoder::new(now)),
            succeeded: false,
            failed: false,
        }
    }

    pub fn stdout(&mut self, bytes: &[u8]) -> Result<(), ReplyError> {
        if self.progress.is_none() {
            return self.fail(ReplyError::UnexpectedEvent);
        }
        if bytes.len() > self.max_bytes - self.snapshot.len() {
            return self.fail(ReplyError::SnapshotLimit(self.max_bytes));
        }
        self.snapshot.extend_from_slice(bytes);
        Ok(())
    }

    pub fn stderr(
        &mut self,
        bytes: &[u8],
        now: Instant,
        emit: impl FnMut(Progress),
    ) -> Result<(), ReplyError> {
        let result = match self.progress.as_mut() {
            Some(progress) => progress.push(bytes, now, emit).map_err(ReplyError::from),
            None => Err(ReplyError::UnexpectedEvent),
        };
        result.or_else(|error| self.fail(error))
    }

    pub fn eof(&mut self) -> Result<(), ReplyError> {
        let result = match self.progress.take() {
            Some(progress) => progress.finish().map_err(ReplyError::from),
            None => Err(ReplyError::UnexpectedEvent),
        };
        result.or_else(|error| self.fail(error))
    }

    pub fn exit_status(&mut self, status: u32) -> Result<(), ReplyError> {
        if self.failed || self.succeeded {
            return self.fail(ReplyError::UnexpectedEvent);
        }
        if status != 0 {
            return self.fail(ReplyError::Exit(status));
        }
        self.succeeded = true;
        Ok(())
    }

    /// Call when the channel closes. EOF and success may arrive in either order.
    pub fn finish(self) -> Result<Vec<u8>, ReplyError> {
        if self.failed || self.progress.is_some() || !self.succeeded || self.snapshot.is_empty() {
            return Err(ReplyError::Incomplete);
        }
        Ok(self.snapshot)
    }

    fn fail(&mut self, error: ReplyError) -> Result<(), ReplyError> {
        self.failed = true;
        self.snapshot = Vec::new();
        self.progress = None;
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_keeps_interleaved_streams_separate() {
        let now = Instant::now();
        let mut reply = ReplyReceiver::new(4, now);
        let mut seen = Vec::new();
        reply.stdout(b"ab").unwrap();
        reply.stderr(b"{\"files\":", now, |p| seen.push(p)).unwrap();
        reply.stdout(b"cd").unwrap();
        reply
            .stderr(b"1,\"directories\":2}\n", now, |p| seen.push(p))
            .unwrap();
        reply.exit_status(0).unwrap();
        reply.eof().unwrap();
        assert_eq!(reply.finish().unwrap(), b"abcd");
        assert_eq!(
            seen,
            [Progress {
                files: 1,
                directories: 2
            }]
        );
    }

    #[test]
    fn reply_requires_complete_success_and_caps_stdout() {
        let now = Instant::now();
        let mut reply = ReplyReceiver::new(2, now);
        reply.stdout(b"ab").unwrap();
        assert!(matches!(
            reply.stdout(b"c"),
            Err(ReplyError::SnapshotLimit(2))
        ));

        let mut reply = ReplyReceiver::new(2, now);
        reply.stdout(b"ab").unwrap();
        reply.eof().unwrap();
        assert!(matches!(
            reply.stdout(b"c"),
            Err(ReplyError::UnexpectedEvent)
        ));
        assert!(matches!(reply.finish(), Err(ReplyError::Incomplete)));

        let mut reply = ReplyReceiver::new(2, now);
        assert!(matches!(reply.exit_status(1), Err(ReplyError::Exit(1))));
        assert!(matches!(
            reply.exit_status(0),
            Err(ReplyError::UnexpectedEvent)
        ));
        assert!(matches!(reply.finish(), Err(ReplyError::Incomplete)));

        let mut reply = ReplyReceiver::new(2, now);
        reply.stdout(b"ab").unwrap();
        reply.exit_status(0).unwrap();
        reply.stderr(b"{", now, |_| {}).unwrap();
        assert!(matches!(
            reply.eof(),
            Err(ReplyError::Progress(Error::Truncated))
        ));
        assert!(matches!(reply.finish(), Err(ReplyError::Incomplete)));
    }
}
