// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix::{open_relative, open_root, stamp};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::{open_relative, open_root, stamp};

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    identity: u128,
    len: u64,
    mtime: u32,
    times: [i64; 4],
}

const WINDOW: usize = 64 * 1024;

pub(super) struct Verifier<'a> {
    roots: [File; 2],
    stop: &'a AtomicBool,
}

impl<'a> Verifier<'a> {
    pub(super) fn new(roots: [&Path; 2], stop: &'a AtomicBool) -> io::Result<Self> {
        Ok(Self {
            roots: [open_root(roots[0])?, open_root(roots[1])?],
            stop,
        })
    }

    pub(super) fn compare(&self, path: &[u8], expected: [(u64, u32); 2]) -> io::Result<bool> {
        let mut left = open_relative(&self.roots[0], path)?;
        let mut right = open_relative(&self.roots[1], path)?;
        let before = [stamp(&left)?, stamp(&right)?];
        for (metadata, (size, mtime)) in before.iter().zip(expected) {
            if metadata.len != size || metadata.mtime != mtime {
                return Err(changed());
            }
        }
        let mut left_buffer = [0; WINDOW];
        let mut right_buffer = [0; WINDOW];
        let mut remaining = expected[0].0;
        let mut equal = true;
        while remaining > 0 {
            if self.stop.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "comparison cancelled",
                ));
            }
            let count = remaining.min(WINDOW as u64) as usize;
            left.read_exact(&mut left_buffer[..count])?;
            right.read_exact(&mut right_buffer[..count])?;
            if left_buffer[..count] != right_buffer[..count] {
                equal = false;
                break;
            }
            remaining -= count as u64;
        }
        for ((root, handle), original) in self.roots.iter().zip([left, right]).zip(before) {
            if stamp(&handle)? != original || stamp(&open_relative(root, path)?)? != original {
                return Err(changed());
            }
        }
        Ok(equal)
    }
}

fn changed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "file changed during comparison")
}
