// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Undoing compression, inside hard caps, before any parser sees a byte.
//!
//! **Every codec goes through here and nowhere else.** A compressed file is a
//! promise of more bytes than it holds, and a hostile one is a promise of
//! almost unlimited bytes: a 66 KB zstd stream decodes to 2 GiB of zeros. So
//! the parsers never touch a decoder; they read from what this module hands
//! back, which counts every byte it produces and stops at the cap.
//!
//! - **Input**: the raw bytes read are capped at `max_input_bytes`.
//! - **Output**: the decompressed bytes are capped at the same number, so a
//!   gzip or zstd bomb ends in a sentence at the cap rather than in memory.
//! - **zstd window**: at most [`MAX_WINDOW`], checked by the library from the
//!   frame header before it allocates anything. The library default is
//!   128 MiB and must never be used.
//! - **zstd frames**: any number, decoded in a loop; skippable frames are
//!   skipped (their bytes count against the input cap); a file with no data
//!   frame is refused; bytes after the last frame that are not a frame are
//!   refused; a declared content size above what is left of the output cap is
//!   refused before decoding; each frame's content checksum, when it has one,
//!   is compared here, because `ruzstd` reads it and does not check it.
//! - **Dictionaries** are not supported; a frame that needs one is refused.

use std::io::{self, BufRead, BufReader, Chain, Cursor, Read};

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

/// The largest zstd window accepted. ncdu's own frames are far smaller; the
/// library default (128 MiB) is a large allocation a hostile header can ask
/// for in a dozen bytes.
pub const MAX_WINDOW: u64 = 8 << 20;

/// The ncdu binary export's signature (`ncdu -O`), from its specification.
pub const NCDU_BINARY: &[u8; 8] = b"\xbfncduEX1";

const GZIP: [u8; 2] = [0x1f, 0x8b];
const ZSTD: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// How the input arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Zstd,
}

impl Compression {
    pub fn label(self) -> &'static str {
        match self {
            Compression::None => "uncompressed",
            Compression::Gzip => "gzip",
            Compression::Zstd => "zstd",
        }
    }
}

/// Why decoding stopped. Carried through `io::Error` so that a parser reading
/// a stream can be stopped by it, and recovered on the far side.
#[derive(Debug, thiserror::Error)]
pub enum Stop {
    #[error("the input is larger than the import limit of {0} bytes")]
    InputTooLarge(u64),
    #[error("the decompressed data is larger than the import limit of {0} bytes")]
    OutputTooLarge(u64),
    #[error("the zstd data asks for a {requested}-byte window, more than the {limit}-byte limit")]
    Window { requested: u64, limit: u64 },
    #[error("the zstd data is damaged: {0}")]
    Zstd(String),
    #[error("a zstd frame's checksum does not match its contents")]
    Checksum,
    #[error("the zstd data holds no data frame")]
    NoFrames,
}

impl From<Stop> for io::Error {
    fn from(stop: Stop) -> io::Error {
        io::Error::other(stop)
    }
}

/// Recover a [`Stop`] from an `io::Error` that carries one.
pub fn stop_of(error: &io::Error) -> Option<&Stop> {
    error.get_ref()?.downcast_ref::<Stop>()
}

/// Caps on one decode.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_window: u64,
}

/// A reader that counts what passes through it and refuses to pass more than
/// `limit` bytes. Shared counts, so the caller can say how much was read.
pub struct Limited<R> {
    inner: R,
    limit: u64,
    count: std::rc::Rc<std::cell::Cell<u64>>,
    over: fn(u64) -> Stop,
}

impl<R: Read> Limited<R> {
    pub fn new(
        inner: R,
        limit: u64,
        over: fn(u64) -> Stop,
    ) -> (Self, std::rc::Rc<std::cell::Cell<u64>>) {
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            Limited {
                inner,
                limit,
                count: count.clone(),
                over,
            },
            count,
        )
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let used = self.count.get();
        // One byte past the limit is enough to tell "exactly at" from "over".
        let room = self.limit.saturating_sub(used).saturating_add(1);
        let want = buf.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buf[..want])?;
        let total = used + read as u64;
        if total > self.limit {
            return Err((self.over)(self.limit).into());
        }
        self.count.set(total);
        Ok(read)
    }
}

/// A stream with bytes already read from its front put back.
type PutBack<R> = Chain<Cursor<Vec<u8>>, R>;

/// Up to `n` bytes from the front of a stream, and the stream with them put
/// back.
fn peek<R: Read>(mut input: R, n: usize) -> io::Result<(Vec<u8>, PutBack<R>)> {
    let mut prefix = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match input.read(&mut prefix[got..]) {
            Ok(0) => break,
            Ok(read) => got += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    prefix.truncate(got);
    let head = prefix.clone();
    Ok((head, Cursor::new(prefix).chain(input)))
}

/// What the first bytes say a stream is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Empty,
    Gzip,
    Zstd,
    NcduBinary,
    Other,
}

pub fn sniff(prefix: &[u8]) -> Kind {
    if prefix.is_empty() {
        Kind::Empty
    } else if prefix.starts_with(&GZIP) {
        Kind::Gzip
    } else if prefix.starts_with(&ZSTD)
        || (prefix.len() >= 4 && prefix[0] & 0xf0 == 0x50 && prefix[1..4] == [0x2a, 0x4d, 0x18])
    {
        // A zstd frame, or a skippable frame (0x184D2A50 to 0x184D2A5F).
        Kind::Zstd
    } else if prefix.starts_with(NCDU_BINARY) {
        Kind::NcduBinary
    } else {
        Kind::Other
    }
}

/// What [`open`] found and the stream to parse.
pub struct Opened<'a> {
    pub compression: Compression,
    pub reader: Box<dyn BufRead + 'a>,
    /// Raw bytes read so far, and decompressed bytes produced so far.
    pub input_count: std::rc::Rc<std::cell::Cell<u64>>,
    pub output_count: std::rc::Rc<std::cell::Cell<u64>>,
}

/// Why [`open`] refused before parsing.
#[derive(Debug)]
pub enum Refusal {
    Empty,
    NcduBinary,
    CompressedTwice,
    Io(io::Error),
}

impl From<io::Error> for Refusal {
    fn from(error: io::Error) -> Self {
        Refusal::Io(error)
    }
}

/// Detect the compression and return a capped stream of the content.
pub fn open<'a, R: Read + 'a>(input: R, caps: Caps) -> Result<Opened<'a>, Refusal> {
    let (raw, input_count) = Limited::new(input, caps.max_input_bytes, Stop::InputTooLarge);
    let (prefix, raw) = peek(raw, 8)?;
    let (compression, decoded): (Compression, Box<dyn Read + 'a>) = match sniff(&prefix) {
        Kind::Empty => return Err(Refusal::Empty),
        Kind::NcduBinary => return Err(Refusal::NcduBinary),
        Kind::Other => (Compression::None, Box::new(raw)),
        Kind::Gzip => (
            Compression::Gzip,
            Box::new(flate2::read::MultiGzDecoder::new(BufReader::new(raw))),
        ),
        Kind::Zstd => (
            Compression::Zstd,
            Box::new(Zstd::new(
                BufReader::with_capacity(64 * 1024, raw),
                caps.max_output_bytes,
                caps.max_window,
                caps.max_input_bytes,
            )),
        ),
    };
    let (decoded, output_count) =
        Limited::new(decoded, caps.max_output_bytes, Stop::OutputTooLarge);
    let (inner, decoded) = peek(decoded, 8)?;
    if compression != Compression::None {
        match sniff(&inner) {
            Kind::Empty => return Err(Refusal::Empty),
            Kind::NcduBinary => return Err(Refusal::NcduBinary),
            Kind::Gzip | Kind::Zstd => return Err(Refusal::CompressedTwice),
            Kind::Other => {}
        }
    }
    Ok(Opened {
        compression,
        reader: Box::new(BufReader::with_capacity(64 * 1024, decoded)),
        input_count,
        output_count,
    })
}

/// Zstandard frames, decoded one block at a time.
pub struct Zstd<R> {
    source: R,
    decoder: FrameDecoder,
    in_frame: bool,
    frames: u64,
    out: Vec<u8>,
    at: usize,
    done: bool,
    budget: u64,
    max_output: u64,
    max_window: u64,
    skippable_budget: u64,
}

impl<R: BufRead> Zstd<R> {
    pub fn new(source: R, max_output: u64, max_window: u64, max_input: u64) -> Self {
        let mut decoder = FrameDecoder::new();
        decoder.set_max_window_size(max_window);
        Zstd {
            source,
            decoder,
            in_frame: false,
            frames: 0,
            out: Vec::new(),
            at: 0,
            done: false,
            budget: max_output,
            max_output,
            max_window,
            skippable_budget: max_input,
        }
    }

    fn damaged(error: FrameDecoderError) -> Stop {
        match error {
            FrameDecoderError::WindowSizeTooBig { requested, max } => Stop::Window {
                requested,
                limit: max,
            },
            FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::BadMagicNumber(_)) => {
                Stop::Zstd("bytes after the last frame are not a zstd frame".into())
            }
            FrameDecoderError::DictNotProvided { .. } => {
                Stop::Zstd("a frame needs a dictionary, which is not supported".into())
            }
            other => Stop::Zstd(other.to_string()),
        }
    }

    /// Make progress: start a frame, skip one, decode some blocks or finish.
    fn advance(&mut self) -> io::Result<()> {
        if !self.in_frame {
            if self.source.fill_buf()?.is_empty() {
                if self.frames == 0 {
                    return Err(Stop::NoFrames.into());
                }
                self.done = true;
                return Ok(());
            }
            // Set again before every frame: the cap is the decoder's field,
            // but nothing here is allowed to depend on that surviving a reset.
            self.decoder.set_max_window_size(self.max_window);
            return match self.decoder.reset(&mut self.source) {
                Ok(()) => {
                    let declared = self.decoder.content_size();
                    if declared > self.budget {
                        return Err(Stop::OutputTooLarge(self.max_output).into());
                    }
                    self.in_frame = true;
                    Ok(())
                }
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => {
                    let length = u64::from(length);
                    if length > self.skippable_budget {
                        return Err(Stop::InputTooLarge(self.skippable_budget).into());
                    }
                    self.skippable_budget -= length;
                    let skipped = io::copy(&mut (&mut self.source).take(length), &mut io::sink())?;
                    if skipped != length {
                        return Err(Stop::Zstd("a skippable frame is cut short".into()).into());
                    }
                    Ok(())
                }
                Err(error) => Err(Self::damaged(error).into()),
            };
        }

        if !self.decoder.is_finished() {
            self.decoder
                .decode_blocks(
                    &mut self.source,
                    BlockDecodingStrategy::UptoBytes(256 * 1024),
                )
                .map_err(Self::damaged)?;
        }
        self.out.clear();
        self.at = 0;
        self.decoder
            .collect_to_writer(&mut self.out)
            .map_err(|error| Stop::Zstd(error.to_string()))?;
        let produced = self.out.len() as u64;
        if produced > self.budget {
            return Err(Stop::OutputTooLarge(self.max_output).into());
        }
        self.budget -= produced;
        if self.decoder.is_finished() && self.decoder.can_collect() == 0 {
            // ruzstd reads the frame's checksum and does not compare it.
            if let Some(expected) = self.decoder.get_checksum_from_data()
                && self.decoder.get_calculated_checksum() != Some(expected)
            {
                return Err(Stop::Checksum.into());
            }
            self.in_frame = false;
            self.frames += 1;
        }
        Ok(())
    }
}

impl<R: BufRead> Read for Zstd<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.at < self.out.len() {
                let n = buf.len().min(self.out.len() - self.at);
                buf[..n].copy_from_slice(&self.out[self.at..self.at + n]);
                self.at += n;
                return Ok(n);
            }
            if self.done || buf.is_empty() {
                return Ok(0);
            }
            self.advance()?;
        }
    }
}

/// Decode a whole buffer with the given caps. For tests and the fuzz target.
pub fn decode_all(input: &[u8], caps: Caps) -> Result<Vec<u8>, io::Error> {
    let mut opened = match open(input, caps) {
        Ok(opened) => opened,
        Err(Refusal::Io(error)) => return Err(error),
        Err(other) => return Err(io::Error::other(format!("{other:?}"))),
    };
    let mut out = Vec::new();
    opened.reader.read_to_end(&mut out)?;
    Ok(out)
}
