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

use std::io::{self, BufRead, Read};
use std::time::SystemTime;

use kosha_core::SizeTree;
use kosha_core::snapshot::Meta;

pub mod checksums;
pub mod decompress;
mod ncdu;
mod qdirstat;
mod rclone;

pub use decompress::Compression;
pub use rclone::{RcloneSource, import_rclone};

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
    #[error("unrecognized listing format; specify --from ncdu, --from rclone or --from qdirstat")]
    ListingFormat,
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
    on_import_worker(move || {
        ncdu::import(
            open_listing(input, &limits)?,
            source_name,
            label,
            now,
            &limits,
        )
    })
}

/// Import a plain or compressed QDirStat cache.
pub fn import_qdirstat<R: Read + Send>(
    input: R,
    source_name: &str,
    label: &str,
    now: SystemTime,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    on_import_worker(move || {
        qdirstat::import(
            open_listing(input, limits)?,
            source_name,
            label,
            now,
            limits,
        )
    })
}

/// Detect a supported listing after decompression and read it from the same stream.
pub fn import_auto<R: Read + Send>(
    input: R,
    source: &RcloneSource<'_>,
    now: SystemTime,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    let limits = Limits {
        max_depth: limits.max_depth.min(MAX_DEPTH),
        ..*limits
    };
    on_import_worker(move || {
        let mut opened = open_listing(input, &limits)?;
        let compression = opened.compression;
        let first = |reader: &mut dyn BufRead| {
            first_nonblank(reader).map_err(|error| io_error(error, compression))
        };
        if first(&mut *opened.reader)? != Some(b'[') {
            return Err(ImportError::ListingFormat);
        }
        opened.reader.consume(1);
        let format = first(&mut *opened.reader)?;
        opened.reader = Box::new(io::Cursor::new(b"[").chain(opened.reader));
        match format {
            Some(b'0'..=b'9') => {
                ncdu::import(opened, source.source_name, source.label, now, &limits)
            }
            Some(b'{' | b']') => rclone::import_opened(opened, source, now, &limits),
            Some(b'q') => qdirstat::import(opened, source.source_name, source.label, now, &limits),
            _ => Err(ImportError::ListingFormat),
        }
    })
}

/// Skip JSON whitespace without buffering the skipped bytes.
fn first_nonblank(reader: &mut dyn BufRead) -> io::Result<Option<u8>> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(None);
        }
        let blank = available
            .iter()
            .take_while(|&&byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            .count();
        let first = available.get(blank).copied();
        reader.consume(blank);
        if first.is_some() {
            return Ok(first);
        }
    }
}

fn on_import_worker(
    run: impl FnOnce() -> Result<Imported, ImportError> + Send,
) -> Result<Imported, ImportError> {
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("kosha-import".into())
            .stack_size(64 << 20)
            .spawn_scoped(scope, run);
        match worker {
            Ok(handle) => handle.join().unwrap_or(Err(ImportError::Internal)),
            Err(error) => Err(ImportError::Io(error)),
        }
    })
}

fn open_listing<'a, R: Read + 'a>(
    input: R,
    limits: &Limits,
) -> Result<decompress::Opened<'a>, ImportError> {
    use decompress::{Caps, Refusal};

    let caps = Caps {
        max_input_bytes: limits.max_input_bytes,
        max_output_bytes: limits.max_input_bytes,
        max_window: limits.max_window,
    };
    decompress::open(input, caps).map_err(|refusal| match refusal {
        Refusal::Empty => ImportError::Empty,
        Refusal::NcduBinary => ImportError::NcduBinary,
        Refusal::CompressedTwice => ImportError::CompressedTwice,
        Refusal::Io(error) => io_error(error, Compression::None),
    })
}

fn io_error(error: io::Error, compression: Compression) -> ImportError {
    use decompress::Stop;

    if let Some(stop) = decompress::stop_of(&error) {
        return match stop {
            Stop::InputTooLarge(limit) => ImportError::InputTooLarge(*limit),
            Stop::OutputTooLarge(limit) => ImportError::DecompressedTooLarge(*limit),
            Stop::Window { requested, limit } => ImportError::ZstdWindow {
                requested: *requested,
                limit: *limit,
            },
            other => ImportError::Compressed(other.to_string()),
        };
    }
    match compression {
        Compression::None => ImportError::Io(error),
        compressed if error.kind() == io::ErrorKind::UnexpectedEof => {
            ImportError::Compressed(format!(
                "the {} data ends early; it may be truncated",
                compressed.label()
            ))
        }
        compressed => ImportError::Compressed(format!(
            "the {} data is damaged: {error}",
            compressed.label()
        )),
    }
}

fn listing_json_error(error: serde_json::Error, compression: Compression) -> ImportError {
    match error.classify() {
        serde_json::error::Category::Io => io_error(error.into(), compression),
        serde_json::error::Category::Eof => ImportError::Truncated(error.to_string()),
        _ => {
            let text = error.to_string();
            if text.contains("invalid unicode code point") {
                ImportError::NotUtf8(text)
            } else {
                ImportError::Malformed(text)
            }
        }
    }
}

#[cfg(test)]
mod auto_tests {
    use super::*;

    #[test]
    fn detects_both_array_shapes_and_preserves_the_stream() {
        let source = RcloneSource {
            source_name: "listing.json",
            label: "",
            root: "",
            tool: "",
            observed: None,
        };
        for (json, expected) in [
            ("  [  ]", "rclone-lsjson"),
            (r#" [ 1,0,{},[{"name":"/tmp"}]] "#, "ncdu-json"),
        ] {
            let result = import_auto(
                json.as_bytes(),
                &source,
                SystemTime::UNIX_EPOCH,
                &Limits::default(),
            )
            .unwrap();
            assert_eq!(result.bytes_read, json.len() as u64);
            assert_eq!(result.meta.import.unwrap().format, expected);
        }
        assert!(matches!(
            import_auto(
                &b"[false]"[..],
                &source,
                SystemTime::UNIX_EPOCH,
                &Limits::default()
            ),
            Err(ImportError::ListingFormat)
        ));
    }
}
