// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `kosha-import` — read a listing another disk-usage tool wrote into a
//! [`SizeTree`] that kosha can browse, search and compare, and that says, in
//! every place it is shown, that kosha did not measure it.
//!
//! Increment 1 reads the ncdu JSON export (`ncdu -o FILE`), uncompressed,
//! gzip-compressed or zstd-compressed (`ncdu -c`).
//!
//! **The input is somebody else's file.** Everything here is pure over a
//! reader: no network, no process is started, and no path that appears inside
//! the listing is ever opened, checked or stat-ed. The caller opens the one
//! file the user named, and writes the result through the snapshot store.
//! Every count, length and depth is bounded before it can cost memory; see
//! [`Limits`] and [`decompress`].

use std::io::Read;
use std::time::SystemTime;

use kosha_core::SizeTree;
use kosha_core::snapshot::Meta;

pub mod decompress;
mod ncdu;

pub use decompress::Compression;

/// Bounds on one import. The defaults suit a whole server; each refusal
/// names the limit it hit.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Raw bytes read, and separately the bytes a compressed input decodes
    /// to. Default 8 GiB.
    pub max_input_bytes: u64,
    /// Entries, the root included. Default 50 million.
    pub max_entries: u64,
    /// Directory levels below the root. Default and maximum 120: the JSON
    /// reader stops at 128 levels of nesting, and an ncdu directory costs one
    /// level plus one for its information object.
    pub max_depth: u32,
    /// Bytes in one name. Default 4096.
    pub max_name_bytes: usize,
    /// Bytes in one metadata string (program name, version, `excluded`).
    pub max_meta_bytes: usize,
    /// Largest zstd window accepted. Default 8 MiB.
    pub max_window: u64,
}

/// The deepest an ncdu listing can nest here; see [`Limits::max_depth`].
pub const MAX_DEPTH: u32 = 120;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input_bytes: 8 << 30,
            max_entries: 50_000_000,
            max_depth: MAX_DEPTH,
            max_name_bytes: 4096,
            max_meta_bytes: 4096,
            max_window: decompress::MAX_WINDOW,
        }
    }
}

/// Why an import was refused. Nothing is written when it is.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("the input is empty")]
    Empty,
    #[error(
        "this is ncdu's binary export (`ncdu -O`), which kosha does not read; \
         export JSON instead with `ncdu -o FILE` (or `ncdu -co FILE` for zstd)"
    )]
    NcduBinary,
    #[error("the input is compressed twice; decompress it once first")]
    CompressedTwice,
    #[error("this is not an ncdu JSON export: {0}")]
    NotNcdu(String),
    #[error(
        "this ncdu export is format version {major}.{minor}, and kosha reads version 1 \
         (minor 0 to 10000)"
    )]
    Version { major: u64, minor: u64 },
    #[error(
        "the export is not valid UTF-8 ({0}). ncdu writes names exactly as the \
         filesystem reports them, so names that are not UTF-8 make the file invalid \
         JSON; convert it first, for example `iconv -f latin1 -t utf-8 in.json > out.json`"
    )]
    NotUtf8(String),
    #[error("the export ends early; it may be truncated ({0})")]
    Truncated(String),
    #[error("the export is not well-formed JSON: {0}")]
    Malformed(String),
    #[error("entry {entry}: {reason}")]
    Entry { entry: u64, reason: String },
    #[error("the export has more than {0} entries, the import limit")]
    TooManyEntries(u64),
    #[error("entry {entry} is nested more than {limit} directories deep, the import limit")]
    TooDeep { entry: u64, limit: u32 },
    #[error(
        "the input is larger than the import limit of {0} bytes; \
         raise it with --max-input-bytes"
    )]
    InputTooLarge(u64),
    #[error(
        "the decompressed export is larger than the import limit of {0} bytes; \
         raise it with --max-input-bytes"
    )]
    DecompressedTooLarge(u64),
    #[error(
        "the zstd data needs a {requested}-byte window, more than the {limit}-byte \
         limit; decompress it with `zstd -d` and import the JSON"
    )]
    ZstdWindow { requested: u64, limit: u64 },
    #[error("{0}")]
    Compressed(String),
    #[error("could not read the input: {0}")]
    Io(std::io::Error),
    #[error("the importer stopped unexpectedly; nothing was written")]
    Internal,
}

/// What an import produced.
pub struct Imported {
    pub tree: SizeTree,
    /// The record's header, with its provenance, ready for the snapshot store.
    pub meta: Meta,
    pub compression: Compression,
    /// Raw bytes read.
    pub bytes_read: u64,
    /// Bytes after decompression (equal to `bytes_read` when uncompressed).
    pub bytes_decoded: u64,
}

/// Import an ncdu JSON export.
///
/// `source_name` is the input's file name (never a full path) and `now` the
/// clock to record as the import time; neither is read from the environment.
/// Runs on a worker thread with its own stack, so a panic in a decoder ends
/// in [`ImportError::Internal`] rather than taking the caller down.
pub fn import_ncdu<R: Read + Send>(
    input: R,
    source_name: &str,
    label: &str,
    now: SystemTime,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    let limits = Limits {
        max_depth: limits.max_depth.min(MAX_DEPTH),
        ..*limits
    };
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("kosha-import".into())
            .stack_size(64 << 20)
            .spawn_scoped(scope, move || {
                ncdu::import(input, source_name, label, now, &limits)
            });
        match worker {
            Ok(handle) => handle.join().unwrap_or(Err(ImportError::Internal)),
            Err(error) => Err(ImportError::Io(error)),
        }
    })
}
