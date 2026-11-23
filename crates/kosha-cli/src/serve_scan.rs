// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, bounded};
use kosha_core::snapshot::{self, Meta, ReadLimits};
use kosha_core::{ScanOptions, Scanner, SizeTree};
use kosha_scan_wire::{self as wire, Hello, Progress};

pub fn stdio() -> Result<(), Box<dyn std::error::Error>> {
    serve(
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

fn serve(
    input: &mut impl Read,
    output: &mut impl Write,
    progress: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let hello = Hello::new(concat!("kosha ", env!("CARGO_PKG_VERSION")));
    wire::write_hello(output, &hello)?;
    output.flush()?;
    let request = wire::read_negotiated_request(input, &hello)?;
    // The client half-closes stdin after its single request.
    if input.read(&mut [0])? != 0 {
        return Err(io::Error::other("unexpected bytes after the scan request").into());
    }
    let options = ScanOptions {
        collect_owners: request.owners,
        cross_filesystems: request.cross_filesystems,
        dedup_hardlinks: Some(request.dedup_hardlinks),
        max_depth: request.max_depth,
        ..ScanOptions::default()
    };
    let tree = scan_with_progress(Path::new(&request.path), options, progress)?;
    let limits = ReadLimits::default();
    if tree.len() as u64 > limits.max_nodes
        || tree.names_bytes().len() as u64 > limits.max_name_bytes
    {
        return Err(io::Error::other("remote scan exceeds the snapshot limits").into());
    }
    let meta = Meta::of(&tree, SystemTime::now(), "");
    send_progress(
        progress,
        &Progress {
            files: meta.files,
            directories: meta.dirs,
        },
    )?;
    snapshot::write(
        &tree,
        &meta,
        BoundedOutput {
            output,
            remaining: limits.max_wire_bytes,
        },
    )?;
    output.flush()?;
    Ok(())
}

fn scan_with_progress(
    path: &Path,
    options: ScanOptions,
    output: &mut impl Write,
) -> Result<SizeTree, Box<dyn std::error::Error>> {
    let (sender, receiver) = bounded(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let scanner = Scanner::new(options)
        .with_cancel(Arc::clone(&cancel))
        .with_progress(sender);
    thread::scope(|scope| {
        let worker = scope.spawn(move || scanner.scan(path));
        forward_progress(&receiver, Duration::from_secs(1), &cancel, |counts| {
            send_progress(output, counts)
        })?;
        Ok(worker
            .join()
            .map_err(|_| io::Error::other("scan worker failed"))??)
    })
}

fn forward_progress(
    receiver: &Receiver<kosha_core::Progress>,
    interval: Duration,
    cancel: &AtomicBool,
    mut emit: impl FnMut(&Progress) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut counts = Progress {
        files: 0,
        directories: 0,
    };
    let mut deadline = Instant::now() + interval;
    loop {
        match receiver.recv_deadline(deadline) {
            Ok(update) => {
                if let Some((files, directories)) = update.recorded_counts {
                    counts = Progress { files, directories };
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= deadline {
            if let Err(error) = emit(&counts) {
                cancel.store(true, Ordering::Relaxed);
                return Err(error);
            }
            deadline = Instant::now() + interval;
        }
    }
}

fn send_progress(
    output: &mut impl Write,
    progress: &Progress,
) -> Result<(), Box<dyn std::error::Error>> {
    wire::write_progress(output, progress)?;
    output.flush()?;
    Ok(())
}

struct BoundedOutput<'a, W> {
    output: &'a mut W,
    remaining: u64,
}

impl<W: Write> Write for BoundedOutput<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "remote scan exceeds the snapshot byte limit",
            ));
        }
        let written = self.output.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_progress_uses_committed_counts_and_cancels_on_broken_pipe() {
        for broken_pipe in [false, true] {
            let (sender, receiver) = bounded(1);
            sender
                .send(kosha_core::Progress {
                    recorded_counts: Some((7, 3)),
                    dirs_scanned: Some(2),
                    entries: 10,
                    dirs_pending: 1,
                    depth: 1,
                })
                .unwrap();
            let mut sender = Some(sender);
            let cancel = AtomicBool::new(false);
            let mut reports = 0;
            let result = forward_progress(&receiver, Duration::from_millis(1), &cancel, |counts| {
                reports += 1;
                assert_eq!((counts.files, counts.directories), (7, 3));
                drop(sender.take());
                if broken_pipe {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe).into());
                }
                Ok(())
            });
            assert_eq!(reports, 1);
            assert_eq!(result.is_err(), broken_pipe);
            assert_eq!(cancel.load(Ordering::Relaxed), broken_pipe);
        }
    }
}
