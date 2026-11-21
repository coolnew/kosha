// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The ncdu JSON export, read as a stream.
//!
//! Format (<https://dev.yorhel.nl/ncdu/jsonfmt>): a top-level array
//! `[1, minor, {metadata}, root]`, where a directory is an array whose first
//! element is its information object and whose other elements are files
//! (objects) and subdirectories (arrays). The root's name is an absolute path;
//! every other name is one component.
//!
//! **Never a document in memory.** `serde_json`'s streaming deserializer
//! drives seeds that push each entry straight into a
//! [`TreeBuilder`](kosha_core::TreeBuilder); no `Value` of the export is built.
//! Each directory level costs one level of recursion, bounded by
//! [`Limits::max_depth`] before `serde_json`'s own limit of 128 is reached.

use std::collections::HashSet;
use std::fmt;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kosha_core::snapshot::{Meta, Provenance};
use kosha_core::{Entry, Flags, ScanStats, Strategy, TreeBuilder};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::decompress::{self, Caps, Compression, Refusal, Stop};
use crate::{ImportError, Imported, Limits};

/// Sizes at or above this are outside the specification's range.
const SIZE_LIMIT: u64 = 1 << 63;
/// Readers accept minor versions 0 to 10000 (specification).
const MINOR_LIMIT: u64 = 10_000;

const S_IFMT: u64 = 0o170_000;
const S_IFLNK: u64 = 0o120_000;

pub(crate) fn import<R: Read>(
    input: R,
    source_name: &str,
    label: &str,
    now: SystemTime,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    let caps = Caps {
        max_input_bytes: limits.max_input_bytes,
        max_output_bytes: limits.max_input_bytes,
        max_window: limits.max_window,
    };
    let opened = match decompress::open(input, caps) {
        Ok(opened) => opened,
        Err(Refusal::Empty) => return Err(ImportError::Empty),
        Err(Refusal::NcduBinary) => return Err(ImportError::NcduBinary),
        Err(Refusal::CompressedTwice) => return Err(ImportError::CompressedTwice),
        Err(Refusal::Io(error)) => return Err(io_error(error, Compression::None)),
    };
    let compression = opened.compression;
    let mut reader = opened.reader;

    starts_with_an_array(&mut reader).map_err(|error| match error {
        Start::Io(error) => io_error(error, compression),
        Start::Refused(refusal) => refusal,
    })?;

    let mut state = State::new(limits);
    let mut de = serde_json::Deserializer::from_reader(&mut reader);
    let parsed = Top { state: &mut state }
        .deserialize(&mut de)
        .and_then(|()| de.end());
    if let Err(error) = parsed {
        return Err(json_error(error, &mut state, compression));
    }
    drop(de);

    let bytes_read = opened.input_count.get();
    let bytes_decoded = opened.output_count.get();
    let tree_and_meta = state.finish(source_name, label, now)?;
    Ok(Imported {
        tree: tree_and_meta.0,
        meta: tree_and_meta.1,
        compression,
        bytes_read,
        bytes_decoded,
    })
}

enum Start {
    Io(io::Error),
    Refused(ImportError),
}

/// The first byte that is not white space must open the top-level array.
fn starts_with_an_array(reader: &mut dyn BufRead) -> Result<(), Start> {
    loop {
        let buf = reader.fill_buf().map_err(Start::Io)?;
        if buf.is_empty() {
            return Err(Start::Refused(ImportError::Empty));
        }
        let blank = buf
            .iter()
            .take_while(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            .count();
        if blank < buf.len() {
            let first = buf[blank];
            reader.consume(blank);
            return if first == b'[' {
                Ok(())
            } else {
                Err(Start::Refused(ImportError::NotNcdu(
                    "it does not start with `[` (an ncdu export starts `[1,`)".into(),
                )))
            };
        }
        let all = buf.len();
        reader.consume(all);
    }
}

fn io_error(error: io::Error, compression: Compression) -> ImportError {
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

fn json_error(
    error: serde_json::Error,
    state: &mut State,
    compression: Compression,
) -> ImportError {
    if let Some(failure) = state.failure.take() {
        return failure;
    }
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

// ---------------------------------------------------------------- the state

#[derive(Clone, Copy)]
struct Parent {
    handle: u32,
    dev: u64,
}

struct State<'l> {
    limits: &'l Limits,
    failure: Option<ImportError>,
    builder: Option<TreeBuilder>,
    root: String,
    entries: u64,
    minor: u64,
    progname: Option<String>,
    progver: Option<String>,
    timestamp: Option<i64>,
    linked: HashSet<(u64, u64)>,
    unreadable: u64,
    other_device: u64,
    excluded: u64,
    unknown_size: u64,
    hardlinks: u64,
    mtime_seen: bool,
    dsize_seen: bool,
    partial: bool,
}

#[derive(Default)]
struct Info {
    name: Option<String>,
    asize: Option<u64>,
    dsize: Option<u64>,
    dev: Option<u64>,
    ino: Option<u64>,
    nlink: Option<u64>,
    hlnkc: bool,
    read_error: bool,
    excluded: Option<String>,
    mode: Option<u64>,
    mtime: Option<i64>,
}

impl<'l> State<'l> {
    fn new(limits: &'l Limits) -> Self {
        State {
            limits,
            failure: None,
            builder: None,
            root: String::new(),
            entries: 0,
            minor: 0,
            progname: None,
            progver: None,
            timestamp: None,
            linked: HashSet::new(),
            unreadable: 0,
            other_device: 0,
            excluded: 0,
            unknown_size: 0,
            hardlinks: 0,
            mtime_seen: false,
            dsize_seen: false,
            partial: false,
        }
    }

    fn entry_error(&self, reason: impl Into<String>) -> ImportError {
        ImportError::Entry {
            entry: self.entries,
            reason: reason.into(),
        }
    }

    /// Add one entry. `parent` is `None` for the root.
    fn add(
        &mut self,
        parent: Option<Parent>,
        info: Info,
        is_dir: bool,
    ) -> Result<Parent, ImportError> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(ImportError::TooManyEntries(self.limits.max_entries));
        }
        let Some(name) = info.name else {
            return Err(self.entry_error("has no name"));
        };
        if name.len() > self.limits.max_name_bytes {
            return Err(self.entry_error(format!(
                "has a name of {} bytes, more than the {}-byte limit",
                name.len(),
                self.limits.max_name_bytes
            )));
        }
        for (size, what) in [(info.asize, "asize"), (info.dsize, "dsize")] {
            if size.is_some_and(|size| size >= SIZE_LIMIT) {
                return Err(
                    self.entry_error(format!("has an {what} outside the range ncdu writes"))
                );
            }
        }

        let dev = info.dev.or(parent.map(|p| p.dev)).unwrap_or(0);
        let mut flags = Flags::default();
        if is_dir {
            flags.insert(Flags::DIR);
        } else if info.mode.is_some_and(|mode| mode & S_IFMT == S_IFLNK) {
            flags.insert(Flags::SYMLINK);
        }
        if info.read_error {
            flags.insert(Flags::UNREADABLE);
            self.unreadable += 1;
            self.partial = true;
        }
        let mut excluded = false;
        if let Some(reason) = &info.excluded {
            match reason.as_str() {
                "otherfs" | "othfs" | "kernfs" => {
                    flags.insert(Flags::OTHER_DEVICE);
                    self.other_device += 1;
                }
                // "pattern", "frmlink", and anything else, which ncdu reads
                // as "pattern".
                _ => {
                    excluded = true;
                    self.excluded += 1;
                    self.partial = true;
                }
            }
        }
        if !is_dir
            && (excluded || info.read_error || info.excluded.is_some())
            && info.asize.is_none()
            && info.dsize.is_none()
        {
            self.unknown_size += 1;
            self.partial = true;
        }
        self.dsize_seen |= info.dsize.is_some();

        let mut logical = info.asize.unwrap_or(0);
        let mut allocated = info.dsize.unwrap_or(0);
        // Hard links: the same (dev, ino) is one file, and the first name met
        // owns its bytes. An absent `ino` counts as 0, which groups every
        // linked file on that device without one, as ncdu itself does.
        let linked = info.hlnkc || info.nlink.is_some_and(|links| links > 1);
        if !is_dir && linked && !self.linked.insert((dev, info.ino.unwrap_or(0))) {
            flags.insert(Flags::HARDLINK_DUP);
            logical = 0;
            allocated = 0;
            self.hardlinks += 1;
        }
        let mtime = match info.mtime {
            Some(seconds) => {
                self.mtime_seen = true;
                u32::try_from(seconds.max(0)).unwrap_or(u32::MAX)
            }
            None => 0,
        };
        let entry = Entry {
            logical,
            allocated,
            mtime,
            flags,
        };

        match parent {
            None => {
                if name.is_empty() || name.contains('\0') {
                    return Err(self.entry_error("is a root with an empty name or a NUL in it"));
                }
                let shown = name
                    .rsplit('/')
                    .find(|part| !part.is_empty())
                    .unwrap_or(&name)
                    .to_string();
                let builder = TreeBuilder::new(&shown, entry)
                    .map_err(|error| self.entry_error(error.to_string()))?;
                self.builder = Some(builder);
                self.root = name;
                Ok(Parent { handle: 0, dev })
            }
            Some(parent) => {
                if let Some(reason) = name_problem(&name) {
                    return Err(self.entry_error(reason));
                }
                let builder = self.builder.as_mut().ok_or(ImportError::Internal)?;
                let handle = builder.push(parent.handle, &name, entry).map_err(|error| {
                    ImportError::Entry {
                        entry: self.entries,
                        reason: error.to_string(),
                    }
                })?;
                Ok(Parent { handle, dev })
            }
        }
    }

    fn finish(
        mut self,
        source_name: &str,
        label: &str,
        now: SystemTime,
    ) -> Result<(kosha_core::SizeTree, Meta), ImportError> {
        let Some(mut builder) = self.builder.take() else {
            return Err(ImportError::NotNcdu("it has no root directory".into()));
        };
        // An export with no on-disk size anywhere is apparent sizes only:
        // stored equal, and recorded as such, never as zero.
        let allocated_known = self.dsize_seen;
        if !allocated_known {
            builder.apparent_only();
        }
        let stats = ScanStats {
            strategy: Strategy::Imported,
            unreadable: self.unreadable,
            skipped_other_device: self.other_device,
            hardlinks_deduped: self.hardlinks,
            dedup_hardlinks: true,
            partial: self.partial,
            ..ScanStats::default()
        };
        let (tree, layout) = builder
            .finish(PathBuf::from(&self.root), stats)
            .map_err(|error| ImportError::Malformed(error.to_string()))?;

        let imported_at = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
        let observed = self
            .timestamp
            .and_then(|secs| u64::try_from(secs).ok())
            .filter(|&secs| secs > 0)
            .and_then(|secs| {
                UNIX_EPOCH
                    .checked_add(Duration::from_secs(secs))
                    .map(|_| secs)
            });
        let taken = match observed {
            Some(secs) => UNIX_EPOCH + Duration::from_secs(secs),
            None => now,
        };
        let tool = match (&self.progname, &self.progver) {
            (Some(name), Some(version)) => format!("{name} {version}"),
            (Some(name), None) => name.clone(),
            (None, Some(version)) => format!("ncdu {version}"),
            (None, None) => String::new(),
        };

        let mut meta = Meta::of(&tree, taken, label);
        meta.exact_allocated = None;
        meta.duration_ms = 0;
        meta.import = Some(Provenance {
            format: "ncdu-json".into(),
            format_version: format!("1.{}", self.minor),
            tool: tool.trim().to_string(),
            observed,
            observed_basis: if observed.is_some() {
                "tool-timestamp".into()
            } else {
                "unknown".into()
            },
            imported_at: imported_at.as_secs(),
            source_name: source_name.to_string(),
            allocated_known,
            mtime_known: self.mtime_seen,
            unknown_size: self.unknown_size,
            excluded_by_source: self.excluded,
            synthesized_dirs: 0,
            lossy_names: 0,
            duplicate_paths: layout.duplicate_names,
        });
        Ok((tree, meta))
    }
}

/// Why `name` cannot be one component of a path, in words. `None` when it can.
pub(crate) fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("has an empty name")
    } else if name == "." || name == ".." {
        Some("is named `.` or `..`, which would step out of its directory")
    } else if name.contains('\0') {
        Some("has a NUL in its name")
    } else if name.contains('/') {
        Some("has a `/` in its name; only the root may be a path")
    } else if !kosha_core::builder::is_valid_component(name) {
        Some("has a `\\..` step in its name, which would leave its directory on Windows")
    } else {
        None
    }
}

/// Record the failure and stop the parser with an error serde will carry out.
fn fail<T, E: de::Error>(state: &mut State, error: ImportError) -> Result<T, E> {
    state.failure = Some(error);
    Err(E::custom("import stopped"))
}

// ---------------------------------------------------------------- the seeds

struct Top<'s, 'l> {
    state: &'s mut State<'l>,
}

impl<'de> DeserializeSeed<'de> for Top<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Top<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an ncdu JSON export")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let state = self.state;
        let major = seq.next_element_seed(Whole)?.flatten();
        let Some(major) = major else {
            return fail(
                state,
                ImportError::NotNcdu("it does not start with a version number".into()),
            );
        };
        let minor = seq.next_element_seed(Whole)?.flatten();
        let Some(minor) = minor else {
            return fail(
                state,
                ImportError::NotNcdu("its second element is not a version number".into()),
            );
        };
        if major != 1 || minor > MINOR_LIMIT {
            return fail(state, ImportError::Version { major, minor });
        }
        state.minor = minor;
        if seq
            .next_element_seed(Metadata { state: &mut *state })?
            .is_none()
        {
            return fail(
                state,
                ImportError::NotNcdu("it has no metadata or root".into()),
            );
        }
        if seq
            .next_element_seed(Root { state: &mut *state })?
            .is_none()
        {
            return fail(
                state,
                ImportError::NotNcdu("it has no root directory".into()),
            );
        }
        // Later minor versions may add elements; they are read past, not kept.
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }
}

/// A non-negative integer, or `None` for anything else (read past).
struct Whole;

impl<'de> DeserializeSeed<'de> for Whole {
    type Value = Option<u64>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Option<u64>, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Whole {
    type Value = Option<u64>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a version number")
    }
    fn visit_u64<E>(self, v: u64) -> Result<Option<u64>, E> {
        Ok(Some(v))
    }
    fn visit_i64<E>(self, _: i64) -> Result<Option<u64>, E> {
        Ok(None)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Option<u64>, E> {
        Ok(None)
    }
    fn visit_bool<E>(self, _: bool) -> Result<Option<u64>, E> {
        Ok(None)
    }
    fn visit_str<E>(self, _: &str) -> Result<Option<u64>, E> {
        Ok(None)
    }
    fn visit_unit<E>(self) -> Result<Option<u64>, E> {
        Ok(None)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Option<u64>, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<u64>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

struct Metadata<'s, 'l> {
    state: &'s mut State<'l>,
}

impl<'de> DeserializeSeed<'de> for Metadata<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Metadata<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the export's metadata object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let max = self.state.limits.max_meta_bytes;
        while let Some(key) = map.next_key::<Key>()? {
            match key {
                Key::Progname => {
                    self.state.progname = Some(map.next_value_seed(Bounded {
                        max,
                        what: "progname",
                    })?)
                }
                Key::Progver => {
                    self.state.progver = Some(map.next_value_seed(Bounded {
                        max,
                        what: "progver",
                    })?)
                }
                Key::Timestamp => self.state.timestamp = map.next_value_seed(Seconds)?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<(), A::Error> {
        fail(
            self.state,
            ImportError::NotNcdu("its third element is not a metadata object".into()),
        )
    }
}

struct Root<'s, 'l> {
    state: &'s mut State<'l>,
}

impl<'de> DeserializeSeed<'de> for Root<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Root<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the root directory")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<(), A::Error> {
        directory(seq, self.state, None, 0)
    }
    fn visit_map<A: MapAccess<'de>>(self, _: A) -> Result<(), A::Error> {
        fail(
            self.state,
            ImportError::NotNcdu("its root is a file, not a directory".into()),
        )
    }
}

/// A directory: its information object, then its children.
fn directory<'de, A: SeqAccess<'de>>(
    mut seq: A,
    state: &mut State,
    parent: Option<Parent>,
    depth: u32,
) -> Result<(), A::Error> {
    if depth > state.limits.max_depth {
        let entry = state.entries + 1;
        let limit = state.limits.max_depth;
        return fail(state, ImportError::TooDeep { entry, limit });
    }
    let Some(info) = seq.next_element_seed(InfoSeed { state: &mut *state })? else {
        let error = ImportError::Entry {
            entry: state.entries + 1,
            reason: "is a directory with no information object".into(),
        };
        return fail(state, error);
    };
    let me = match state.add(parent, info, true) {
        Ok(me) => me,
        Err(error) => return fail(state, error),
    };
    while seq
        .next_element_seed(Child {
            state: &mut *state,
            parent: me,
            depth: depth + 1,
        })?
        .is_some()
    {}
    Ok(())
}

struct Child<'s, 'l> {
    state: &'s mut State<'l>,
    parent: Parent,
    depth: u32,
}

impl<'de> DeserializeSeed<'de> for Child<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Child<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a file object or a directory array")
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<(), A::Error> {
        let info = info(map, self.state)?;
        match self.state.add(Some(self.parent), info, false) {
            Ok(_) => Ok(()),
            Err(error) => fail(self.state, error),
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<(), A::Error> {
        directory(seq, self.state, Some(self.parent), self.depth)
    }
}

struct InfoSeed<'s, 'l> {
    state: &'s mut State<'l>,
}

impl<'de> DeserializeSeed<'de> for InfoSeed<'_, '_> {
    type Value = Info;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Info, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for InfoSeed<'_, '_> {
    type Value = Info;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a directory's information object")
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Info, A::Error> {
        info(map, self.state)
    }
}

/// One information object.
fn info<'de, A: MapAccess<'de>>(mut map: A, state: &mut State) -> Result<Info, A::Error> {
    let mut info = Info::default();
    let max_name = state.limits.max_name_bytes;
    let max_meta = state.limits.max_meta_bytes;
    while let Some(key) = map.next_key::<Key>()? {
        match key {
            Key::Name => {
                // The root's name is an absolute path, so it gets the larger of
                // the two bounds; components are checked against the name
                // limit when the entry is added.
                let max = max_name.max(4096);
                info.name = Some(map.next_value_seed(Bounded { max, what: "name" })?);
            }
            Key::Asize => info.asize = Some(map.next_value()?),
            Key::Dsize => info.dsize = Some(map.next_value()?),
            Key::Dev => info.dev = Some(map.next_value()?),
            Key::Ino => info.ino = Some(map.next_value()?),
            Key::Nlink => info.nlink = Some(map.next_value()?),
            Key::Hlnkc => info.hlnkc = map.next_value()?,
            Key::ReadError => info.read_error = map.next_value()?,
            Key::Excluded => {
                info.excluded = Some(map.next_value_seed(Bounded {
                    max: max_meta,
                    what: "excluded",
                })?)
            }
            Key::Mode => info.mode = Some(map.next_value()?),
            Key::Mtime => info.mtime = map.next_value_seed(Seconds)?,
            // `notreg`, `uid`, `gid` and anything a later ncdu adds.
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
    }
    let _ = state;
    Ok(info)
}

/// Keys of interest, matched without allocating.
enum Key {
    Name,
    Asize,
    Dsize,
    Dev,
    Ino,
    Nlink,
    Hlnkc,
    ReadError,
    Excluded,
    Mode,
    Mtime,
    Progname,
    Progver,
    Timestamp,
    Other,
}

impl<'de> de::Deserialize<'de> for Key {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Key, D::Error> {
        struct KeyVisitor;
        impl Visitor<'_> for KeyVisitor {
            type Value = Key;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a field name")
            }
            fn visit_str<E>(self, key: &str) -> Result<Key, E> {
                Ok(match key {
                    "name" => Key::Name,
                    "asize" => Key::Asize,
                    "dsize" => Key::Dsize,
                    "dev" => Key::Dev,
                    "ino" => Key::Ino,
                    "nlink" => Key::Nlink,
                    "hlnkc" => Key::Hlnkc,
                    "read_error" => Key::ReadError,
                    "excluded" => Key::Excluded,
                    "mode" => Key::Mode,
                    "mtime" => Key::Mtime,
                    "progname" => Key::Progname,
                    "progver" => Key::Progver,
                    "timestamp" => Key::Timestamp,
                    _ => Key::Other,
                })
            }
        }
        d.deserialize_identifier(KeyVisitor)
    }
}

/// A string no longer than `max` bytes.
struct Bounded {
    max: usize,
    what: &'static str,
}

impl<'de> DeserializeSeed<'de> for Bounded {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        d.deserialize_str(self)
    }
}

impl Visitor<'_> for Bounded {
    type Value = String;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "a string for `{}`", self.what)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
        if v.len() > self.max {
            return Err(E::custom(format!(
                "`{}` is {} bytes, more than the {}-byte limit",
                self.what,
                v.len(),
                self.max
            )));
        }
        Ok(v.to_string())
    }
}

/// Unix seconds, integer or fractional (ncdu 1.16 and later may write a
/// fraction). Anything else is treated as not recorded.
struct Seconds;

impl<'de> DeserializeSeed<'de> for Seconds {
    type Value = Option<i64>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Option<i64>, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seconds {
    type Value = Option<i64>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a time in seconds")
    }
    fn visit_u64<E>(self, v: u64) -> Result<Option<i64>, E> {
        Ok(Some(i64::try_from(v).unwrap_or(i64::MAX)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Option<i64>, E> {
        Ok(Some(v))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Option<i64>, E> {
        // `as` saturates, and NaN becomes 0, which is "not recorded".
        Ok(v.is_finite().then_some(v.trunc() as i64))
    }
    fn visit_unit<E>(self) -> Result<Option<i64>, E> {
        Ok(None)
    }
}
