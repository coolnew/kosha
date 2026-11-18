// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Writing to standard output, for a reader that is allowed to leave.
//!
//! `println!` panics if the write fails, and the most ordinary thing anybody
//! does to a command-line tool makes it fail:
//!
//! ```text
//! $ kosha /usr | head -4
//! /usr
//! 7.95 GiB on disk · 168,981 entries in 243 ms
//!
//! thread 'main' panicked at library/std/src/io/stdio.rs:1166:9:
//! failed printing to stdout: Broken pipe (os error 32)
//! note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
//! $ echo ${PIPESTATUS[0]}
//! 101
//! ```
//!
//! `head` read its four lines and closed the pipe, which is what `head` is for.
//! Every tool on the machine handles that silently and this one printed a Rust
//! backtrace note and exited 101 — in the binary whose entire argument is that
//! you can script it. `| head`, `| grep -q`, and quitting out of `| less` all
//! did it, on every platform.
//!
//! **Why not the usual one-line fix.** On Unix the idiomatic answer is to
//! restore the default `SIGPIPE` handler, which Rust disables at startup, and
//! let the signal kill the process. That is one line and it is half an answer:
//! Windows has no `SIGPIPE`, and `kosha-gui --version` in PowerShell — the
//! documented way to get a version for a bug report — is exactly this failure
//! on the platform the signal does not exist on. So the error is handled where
//! it happens, once, and the behaviour is the same on all three platforms.
//!
//! **A closed pipe is not a failure; a full disk is.** The two arrive as the
//! same `Err` from the same call and must not be treated alike. `BrokenPipe`
//! means the reader chose to stop listening, which is a normal end to a
//! pipeline and exits 0. Anything else — no space left, a disconnected network
//! drive — is a real failure to write output somebody asked for, and it says so
//! on stderr and exits 1 rather than being swallowed.

use std::fmt;
use std::io::{self, ErrorKind, Write};

/// `println!` for output that can be piped. Use this, never `println!`.
#[macro_export]
macro_rules! sayln {
    () => { $crate::say::write_out(format_args!(""), true) };
    ($($arg:tt)*) => { $crate::say::write_out(format_args!($($arg)*), true) };
}

/// `print!` for output that can be piped. Use this, never `print!`.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => { $crate::say::write_out(format_args!($($arg)*), false) };
}

/// The one place a write to standard output is decided about.
///
/// Public because the macros above expand to a call to it, and hidden because
/// nothing should call it directly.
#[doc(hidden)]
pub fn write_out(args: fmt::Arguments, newline: bool) {
    // Locked once per line rather than per fragment: `writeln!` on the
    // unlocked handle takes and releases the lock for every piece of the
    // format, which interleaves with any other thread printing.
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let wrote = if newline {
        writeln!(out, "{args}")
    } else {
        write!(out, "{args}")
    };

    let Err(error) = wrote else {
        return;
    };

    // Both remaining paths leave through `process::exit`, which runs the
    // runtime's cleanup — and the cleanup flushes standard output, which wants
    // this lock. It is re-entrant and the same thread may take it twice, so
    // this is not a deadlock today; it is a deadlock that depends on a detail
    // of the standard library, in the one path that only runs when something
    // has already gone wrong. Released first instead.
    drop(out);

    if reader_is_gone(&error) {
        // The reader went away: `| head`, `| grep -q`, `q` in `less`. There is
        // nowhere left to write and nothing went wrong, so this is the end of
        // an ordinary pipeline and not an error to report — a message on
        // stderr here would land on the terminal of somebody who deliberately
        // asked for less output.
        std::process::exit(0);
    }

    // Anything else is a real failure to produce output that was asked for,
    // and must not be quiet about it.
    let _ = writeln!(io::stderr(), "kosha: could not write output: {error}");
    std::process::exit(1);
}

/// Did this write fail because whoever was reading stopped?
///
/// **The raw code is checked as well as the kind**, and that is not belt and
/// braces. Windows reports a pipe whose reader has closed as either
/// `ERROR_BROKEN_PIPE` (109) or `ERROR_NO_DATA` (232) depending on which end
/// noticed, and what the standard library maps each of those to has changed
/// between releases. Trusting the mapping alone would make the difference
/// between "a pipeline ended" and "the disk is full" depend on the compiler
/// version — and the wrong answer there is a tool that prints an error and
/// exits 1 every time somebody pipes it into `head`.
pub fn reader_is_gone(error: &io::Error) -> bool {
    let reader_closed = error.kind() == ErrorKind::BrokenPipe;
    #[cfg(windows)]
    let reader_closed = reader_closed || matches!(error.raw_os_error(), Some(109) | Some(232));
    reader_closed
}

/// The same question, of an error that has been boxed and possibly wrapped.
///
/// A write failure that happened three layers down — the exporter writing a
/// row, say — arrives at the top of a front end as somebody else's error type
/// with the real one as its source. The chain is walked rather than the
/// outermost layer being asked, because which layer is holding it is an
/// implementation detail of whichever path produced the output.
pub fn reader_is_gone_boxed(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut level: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(one) = level {
        if let Some(io) = one.downcast_ref::<io::Error>()
            && reader_is_gone(io)
        {
            return true;
        }
        level = one.source();
    }
    false
}
