// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::{future::Future, io};
use tokio::sync::oneshot;

/// Drive an SSH operation on the caller's worker thread.
pub fn run_ssh<F: Future>(operation: F) -> io::Result<F::Output> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(operation))
}

/// Keep this handle alive while the scan should continue.
pub struct ScanHandle {
    _sender: oneshot::Sender<()>,
}

pub struct ScanCancellation(oneshot::Receiver<()>);

pub fn scan_cancellation() -> (ScanHandle, ScanCancellation) {
    let (sender, receiver) = oneshot::channel();
    (ScanHandle { _sender: sender }, ScanCancellation(receiver))
}

impl ScanCancellation {
    /// Return None when the scan handle is dropped, even if work is also ready.
    pub fn run<F: Future>(self, operation: F) -> io::Result<Option<F::Output>> {
        run_ssh(async {
            tokio::select! {
                biased;
                _ = self.0 => None,
                result = operation => Some(result),
            }
        })
    }
}
