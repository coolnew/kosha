// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! A scan, written out for something other than a person to read.
//!
//! The point of exporting is comparison — two scans of the same tree a month
//! apart, or a directory against the backup that is supposed to contain it. So
//! the output is built for a diff first and a spreadsheet second:
//!
//! - **CSV/JSON paths default to relative to the scan root.** Comparing a directory with a copy
//!   of it somewhere else is the main thing anybody wants this for, and absolute
//!   paths make every single line differ. The root is recorded in the JSON
//!   header; CSV is rows only, and says so in its documentation. Callers may
//!   request full paths. TXT always writes full native paths, one per line;
//!   JSONL writes one entry object per line for streaming consumers.
//! - **Rows come out in a fixed order** — path, ascending — not in the order the
//!   scanner happened to produce or the table happens to be sorted by. Two
//!   exports of an unchanged tree must be byte-identical, or a diff is useless.
//! - **Sizes are integers.** `1.4 GiB` is for reading; `1503238553` is for
//!   subtracting.
//!
//! The writers take no serialisation dependency. CSV is RFC 4180 by hand,
//! which is a dozen lines and needs to be exactly right because paths contain
//! commas and quotes. JSON escaping is [`crate::json`], shared with the audit
//! log for the same reason.
//!
//! ## Names that are not text
//!
//! Filenames are stored as OS bytes, which on Unix need not be UTF-8 and on
//! Windows may contain unpaired surrogates. All output formats are text. So a
//! name that will not convert is written lossily — U+FFFD where the bytes were
//! — and the row is marked, in CSV with a `lossy` flag and in JSON with
//! `"lossy": true`. TXT reports the count to the caller because it has no
//! metadata columns, and rejects embedded line breaks; CSV/JSONL can encode them.
//! Silently emitting a mangled path that no longer names a
//! real file, in a document whose purpose is to be compared against reality, is
//! the one outcome worth going out of the way to prevent.

use std::io::{self, Write};

use crate::node::{Flags, NodeId};
use crate::tree::SizeTree;
use crate::{format, json};

/// Which serialisation to write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// RFC 4180, one header row. Rows only — see the module note on paths.
    Csv,
    /// One object, with a header describing the scan and an `entries` array.
    Json,
    /// One full filesystem path per line, without a header.
    Txt,
    /// One JSON entry object per line, without a surrounding document.
    Jsonl,
}

impl Format {
    /// The conventional extension, for naming a file nobody was asked about.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Txt => "txt",
            Format::Jsonl => "jsonl",
        }
    }

    /// A format name, ignoring case; `text` and `ndjson` are accepted aliases.
    pub fn parse(word: &str) -> Option<Format> {
        match word.trim().to_ascii_lowercase().as_str() {
            "csv" => Some(Format::Csv),
            "json" => Some(Format::Json),
            "txt" | "text" => Some(Format::Txt),
            "jsonl" | "ndjson" => Some(Format::Jsonl),
            _ => None,
        }
    }
}

/// What to include.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    pub format: Format,
    /// Full filesystem paths instead of paths relative to the scan root.
    /// TXT always uses full paths, independently of this option.
    pub absolute_paths: bool,
    /// Skip entries whose on-disk size is below this.
    ///
    /// A whole-volume export is millions of rows, most of them a few kilobytes;
    /// a threshold is how it becomes a file anybody opens.
    pub min_size: u64,
    /// Include directories as rows of their own.
    ///
    /// On by default: a directory row carries the subtree total, which is the
    /// number most comparisons are actually about.
    pub directories: bool,
    /// Include files.
    pub files: bool,
    /// The imported source recorded apparent sizes only.
    pub apparent_only: bool,
    /// Some imported sizes were unknown.
    pub unknown_sizes: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            format: Format::Csv,
            absolute_paths: false,
            min_size: 0,
            directories: true,
            files: true,
            apparent_only: false,
            unknown_sizes: false,
        }
    }
}

/// What was written.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Written {
    pub rows: usize,
    /// Rows whose path did not survive conversion to text.
    pub lossy: usize,
    /// On-disk bytes across the rows written — files only, so that summing a
    /// directory row and its contents cannot double-count.
    pub bytes: u64,
}

/// Write an export to a path, and never leave a half one there.
///
/// **A file being written is not a file, and this one is somebody's baseline.**
/// The destination was opened with `File::create`, which empties it before the
/// first row is written, so an export that did not finish replaced a good
/// record with a truncated one. Measured: an export of 228,058 entries to a
/// slow destination, killed four and a half seconds in. What was there — a
/// baseline of seven rows — was gone, and in its place sat 15,487 rows of
/// perfectly valid CSV. A comparison against that file reports every entry
/// past the cut as *new*, which is the failure `compare`'s own note warns
/// about: "a comparison that silently misreads a baseline reports changes that
/// never happened".
///
/// A cut in the middle of a row is caught — the reader refuses a line with the
/// wrong number of fields — so this is specifically about a cut at a line
/// boundary, which is most of them, and about a format that deliberately
/// carries no checksum because it exists to be read by other programs.
///
/// The same dance as [`crate::snapshot::store::save`], for the reason it gives
/// there: write beside it, flush and sync, then rename into place, which
/// replaces the destination in one step on both platforms. Any failure takes
/// the temporary with it and leaves what was there alone.
pub fn to_path(
    tree: &SizeTree,
    rows: Option<&[NodeId]>,
    opts: &Options,
    path: &std::path::Path,
) -> io::Result<Written> {
    atomic_write(path, |out| match rows {
        Some(rows) => write_rows(tree, rows, opts, out),
        None => write_tree(tree, opts, out),
    })
}

/// Write an explicitly requested export through a flushed temporary file.
/// A failed report must leave the existing destination untouched.
pub fn atomic_write<T>(
    path: &std::path::Path,
    write: impl FnOnce(&mut io::BufWriter<std::fs::File>) -> io::Result<T>,
) -> io::Result<T> {
    // Named for this process, and `create_new` on top of that. Two exports
    // aimed at one path must not interleave into one temporary and then rename
    // the result over each other — and a fixed name would do something worse
    // than that: measured, an export killed part way left its temporary behind,
    // and every later export to the same path refused because the name was
    // taken. A file that cannot be written until somebody deletes something is
    // not an improvement on a file that was replaced too early.
    let temp = path.with_extension(format!("kosha-writing-{}", std::process::id()));
    let file = std::fs::File::create_new(&temp)?;

    let written = (|| {
        let mut out = io::BufWriter::new(file);
        let written = write(&mut out)?;
        // Explicitly, because a `BufWriter` dropped on the way out of scope
        // swallows the error from its last flush — and `sync_all` because a
        // rename that beats the data to the disk is the same half file with
        // extra steps.
        out.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()?;
        Ok(written)
    })();

    match written.and_then(|written| std::fs::rename(&temp, path).map(|()| written)) {
        Ok(written) => Ok(written),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// Export the whole tree.
pub fn write_tree<W: Write>(tree: &SizeTree, opts: &Options, out: W) -> io::Result<Written> {
    let all: Vec<NodeId> = (0..tree.len()).map(NodeId::from_index).collect();
    write_rows(tree, &all, opts, out)
}

/// Export exactly these nodes and nothing else.
///
/// This is what a query's results go through, so "export what I am looking at"
/// and "export everything" are the same code and cannot drift apart.
pub fn write_rows<W: Write>(
    tree: &SizeTree,
    rows: &[NodeId],
    opts: &Options,
    mut out: W,
) -> io::Result<Written> {
    let mut kept: Vec<(String, NodeId, bool)> = Vec::with_capacity(rows.len());
    let current_dir =
        if (opts.absolute_paths || opts.format == Format::Txt) && !tree.root_path().is_absolute() {
            Some(std::env::current_dir()?)
        } else {
            None
        };

    for &id in rows {
        // The root is the thing every path is relative to; a row for it would
        // have an empty path and a total already implied by the header.
        if id == tree.root() {
            continue;
        }
        let node = tree.node(id);
        let is_dir = node.is_dir();
        if (is_dir && !opts.directories) || (!is_dir && !opts.files) {
            continue;
        }
        if node.allocated < opts.min_size {
            continue;
        }
        let (path, lossy) = if opts.absolute_paths || opts.format == Format::Txt {
            let full = tree.path_of(id);
            let full = if full.is_absolute() {
                full
            } else {
                current_dir
                    .as_ref()
                    .expect("relative root resolved above")
                    .join(full)
            };
            let text = full.to_string_lossy();
            (text.to_string(), text.contains('\u{fffd}'))
        } else {
            relative_path(tree, id)
        };
        // A line-oriented path file cannot represent embedded line breaks.
        // Refuse it rather than creating extra paths that do not exist.
        if opts.format == Format::Txt && path.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "TXT cannot represent a path containing a line break; use CSV or JSONL",
            ));
        }
        kept.push((path, id, lossy));
    }

    // Fixed order, so two exports of an unchanged tree are byte-identical and a
    // diff shows only what actually changed.
    kept.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.index().cmp(&b.1.index())));

    let mut written = Written::default();
    for (_, _, lossy) in &kept {
        if *lossy {
            written.lossy += 1;
        }
    }
    for (_, id, _) in &kept {
        if !tree.node(*id).is_dir() {
            written.bytes = written.bytes.saturating_add(tree.node(*id).allocated);
        }
    }
    written.rows = kept.len();

    match opts.format {
        Format::Csv => write_csv(tree, &kept, opts, &mut out)?,
        Format::Json => write_json(tree, &kept, opts, &mut out)?,
        Format::Txt => {
            for (path, _, _) in &kept {
                writeln!(out, "{path}")?;
            }
        }
        Format::Jsonl => write_jsonl(tree, &kept, opts, &mut out)?,
    }
    out.flush()?;
    Ok(written)
}

// --------------------------------------------------------------------- CSV

const CSV_HEADER: &str = "path,name,kind,on_disk,apparent,items,modified,flags,lossy\n";

fn write_csv<W: Write>(
    tree: &SizeTree,
    rows: &[(String, NodeId, bool)],
    opts: &Options,
    out: &mut W,
) -> io::Result<()> {
    out.write_all(CSV_HEADER.as_bytes())?;

    let mut line = String::with_capacity(256);
    for (path, id, lossy) in rows {
        let node = tree.node(*id);
        line.clear();

        csv_field(&mut line, path);
        line.push(',');
        csv_field(&mut line, &tree.name_lossy(*id));
        line.push(',');
        line.push_str(if node.is_dir() { "dir" } else { "file" });
        line.push(',');
        if !opts.apparent_only {
            line.push_str(&node.allocated.to_string());
        }
        line.push(',');
        line.push_str(&node.logical.to_string());
        line.push(',');
        // Files have no item count. Empty rather than 0, so a spreadsheet does
        // not average a column of meaningless zeroes.
        if node.is_dir() {
            line.push_str(&node.subtree_files.to_string());
        }
        line.push(',');
        line.push_str(&format::rfc3339(i64::from(node.mtime)));
        line.push(',');
        csv_field(&mut line, &flag_list(node.flags));
        line.push(',');
        line.push_str(if *lossy { "lossy" } else { "" });
        line.push('\n');

        out.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// One RFC 4180 field.
///
/// Quoted only when it has to be, which keeps ordinary paths readable, and
/// always when it contains a comma, a quote or a line break — all three of
/// which are legal in filenames on at least one supported platform.
/// Write one field, quoted where RFC 4180 needs it — and where a spreadsheet
/// does.
///
/// **A leading `=`, `+`, `-`, `@` or tab makes a spreadsheet treat the field as a
/// formula**, and a filename may begin with any of them. `=cmd|'…'!A1` is a legal
/// name on every platform this runs on, and a tool whose output is meant to be
/// opened in a spreadsheet should not be the thing that hands it over as code.
///
/// Quoting is the whole of what is done about it, and it is worth being exact
/// about what that buys. RFC 4180 says a quoted field's value is its contents
/// without the quotes, so this changes no byte of any path: the reader in
/// [`crate::compare`] gives back exactly what went in, and a diff of two exports
/// is unaffected. An importer that follows the standard — LibreOffice, `csv`
/// in Python, the reader in this crate — treats the field as text. Excel does
/// not always, and there is no version of this that both preserves the path and
/// stops Excel evaluating it, because the only thing that stops Excel is
/// changing the text.
///
/// Paths stay exact so exported records still name the original files. Treat
/// CSV cells as text when importing into a spreadsheet to avoid evaluation.
fn csv_field(out: &mut String, value: &str) {
    let formula = matches!(
        value.as_bytes().first(),
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r')
    );
    if formula
        || value
            .bytes()
            .any(|b| matches!(b, b',' | b'"' | b'\n' | b'\r'))
    {
        out.push('"');
        for ch in value.chars() {
            // A quote inside a quoted field is doubled. This is the whole of
            // RFC 4180's escaping, and getting it wrong shifts every column
            // after it.
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    } else {
        out.push_str(value);
    }
}

// -------------------------------------------------------------------- JSON

fn allocated_json(opts: &Options, value: u64) -> String {
    if opts.apparent_only {
        "null".into()
    } else {
        value.to_string()
    }
}

fn write_jsonl<W: Write>(
    tree: &SizeTree,
    rows: &[(String, NodeId, bool)],
    opts: &Options,
    out: &mut W,
) -> io::Result<()> {
    let mut line = String::with_capacity(384);
    let imported = tree.stats().strategy == crate::tree::Strategy::Imported;
    for (path, id, lossy) in rows {
        let node = tree.node(*id);
        line.clear();
        line.push_str("{\"path\":");
        json::write_escaped(&mut line, path);
        line.push_str(",\"name\":");
        json::write_escaped(&mut line, &tree.name_lossy(*id));
        line.push_str(",\"kind\":");
        json::write_escaped(&mut line, if node.is_dir() { "dir" } else { "file" });
        line.push_str(&format!(
            ",\"on_disk\":{},\"apparent\":{},\"items\":",
            allocated_json(opts, node.allocated),
            node.logical
        ));
        if node.is_dir() {
            line.push_str(&node.subtree_files.to_string());
        } else {
            line.push_str("null");
        }
        line.push_str(",\"modified\":");
        json::write_escaped(&mut line, &format::rfc3339(i64::from(node.mtime)));
        line.push_str(",\"created\":");
        if let Some(created) = tree
            .created(*id)
            .and_then(|value| i64::try_from(value).ok())
        {
            json::write_escaped(&mut line, &format::rfc3339(created));
        } else {
            line.push_str("null");
        }
        if imported {
            line.push_str(",\"imported\":true,\"measured_by_kosha\":false");
            if opts.unknown_sizes {
                line.push_str(",\"lower_bound\":true");
            }
        }
        line.push_str(",\"flags\":");
        json::write_escaped(&mut line, &flag_list(node.flags));
        line.push_str(if *lossy {
            ",\"lossy\":true}\n"
        } else {
            ",\"lossy\":false}\n"
        });
        out.write_all(line.as_bytes())?;
    }
    Ok(())
}

fn write_json<W: Write>(
    tree: &SizeTree,
    rows: &[(String, NodeId, bool)],
    opts: &Options,
    out: &mut W,
) -> io::Result<()> {
    let stats = tree.stats();
    let (root, root_lossy) = {
        let raw = tree.root_path().to_string_lossy();
        (raw.to_string(), raw.contains('\u{fffd}'))
    };

    let mut head = String::with_capacity(256);
    head.push_str("{\n  \"kosha\": ");
    json::write_escaped(&mut head, env!("CARGO_PKG_VERSION"));
    head.push_str(",\n  \"exported\": ");
    json::write_escaped(&mut head, &format::now_rfc3339());
    head.push_str(",\n  \"root\": ");
    json::write_escaped(&mut head, &root);
    if root_lossy {
        head.push_str(",\n  \"root_lossy\": true");
    }
    // A listing another tool wrote: its root is a label for somewhere else,
    // and none of these numbers was measured by kosha.
    if stats.strategy == crate::tree::Strategy::Imported {
        head.push_str(",\n  \"imported\": true,\n  \"measured_by_kosha\": false");
    }
    head.push_str(&format!(
        ",\n  \"entries_scanned\": {},\n  \"total_on_disk\": {},\n  \"total_apparent\": {},\n  \"partial\": {},\n  \"access_limited\": {},\n  \"incomplete\": {},\n  \"cancelled\": {},\n  \"depth_limited\": {},\n  \"unreadable_entries\": {},\n  \"lower_bound\": {},\n  \"entries\": [\n",
        stats.entries,
        allocated_json(opts, tree.total_allocated()),
        tree.total_logical(),
        tree.is_partial(),
        stats.unreadable > 0 || (tree.is_partial() && !tree.is_incomplete()),
        tree.is_incomplete(),
        stats.cancelled,
        stats.depth_limited,
        stats.unreadable,
        tree.is_partial() || tree.is_incomplete() || opts.unknown_sizes,
    ));
    out.write_all(head.as_bytes())?;

    let mut line = String::with_capacity(384);
    for (index, (path, id, lossy)) in rows.iter().enumerate() {
        let node = tree.node(*id);
        line.clear();
        line.push_str("    {\"path\": ");
        json::write_escaped(&mut line, path);
        line.push_str(", \"name\": ");
        json::write_escaped(&mut line, &tree.name_lossy(*id));
        line.push_str(", \"kind\": ");
        json::write_escaped(&mut line, if node.is_dir() { "dir" } else { "file" });
        line.push_str(&format!(
            ", \"on_disk\": {}, \"apparent\": {}",
            allocated_json(opts, node.allocated),
            node.logical
        ));
        if node.is_dir() {
            line.push_str(&format!(", \"items\": {}", node.subtree_files));
        }
        line.push_str(", \"modified\": ");
        json::write_escaped(&mut line, &format::rfc3339(i64::from(node.mtime)));

        let flags = flag_list(node.flags);
        if !flags.is_empty() {
            line.push_str(", \"flags\": [");
            for (n, flag) in flags.split(';').enumerate() {
                if n > 0 {
                    line.push_str(", ");
                }
                json::write_escaped(&mut line, flag);
            }
            line.push(']');
        }
        if *lossy {
            line.push_str(", \"lossy\": true");
        }
        line.push('}');
        // No trailing comma: JSON has no tolerance for one, and a document that
        // will not parse is worse than no document.
        if index + 1 < rows.len() {
            line.push(',');
        }
        line.push('\n');
        out.write_all(line.as_bytes())?;
    }

    out.write_all(b"  ]\n}\n")
}

// ------------------------------------------------------------------ shared

/// A node's path relative to the scan root, and whether it converted cleanly.
fn relative_path(tree: &SizeTree, id: NodeId) -> (String, bool) {
    let full = tree.path_of(id);
    let relative = full.strip_prefix(tree.root_path()).unwrap_or(&full);
    let text = relative.to_string_lossy();
    // The replacement character is what `to_string_lossy` leaves behind, and a
    // real filename may contain one legitimately — but then the path is still
    // exactly what the OS reports, so treating it as lossy costs a flag and
    // never a wrong answer.
    let lossy = text.contains('\u{fffd}');
    (crate::path::slashed(&text).into_owned(), lossy)
}

/// The set flags, worth naming, as `sparse;partial`.
///
/// `dir` is omitted: it is already the `kind` column, and repeating it would
/// invite the two disagreeing.
fn flag_list(flags: Flags) -> String {
    let mut out = String::new();
    for (flag, name) in [
        (Flags::SYMLINK, "symlink"),
        (Flags::OTHER_DEVICE, "other-device"),
        (Flags::UNREADABLE, "unreadable"),
        (Flags::HARDLINK_DUP, "hardlink-dup"),
        (Flags::SPARSE, "sparse"),
        (Flags::PARTIAL, "partial"),
        (Flags::OFFLINE, "offline"),
    ] {
        if flags.contains(flag) {
            if !out.is_empty() {
                out.push(';');
            }
            out.push_str(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apparent_only_exports_mark_missing_allocated_sizes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one"), b"abc").unwrap();
        let tree = crate::Scanner::new(crate::ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let text = export(
            &tree,
            &Options {
                format: Format::Json,
                apparent_only: true,
                unknown_sizes: true,
                ..Options::default()
            },
        );
        let document: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(document["total_on_disk"].is_null());
        assert_eq!(document["total_apparent"], 3);
        assert_eq!(document["lower_bound"], true);
        assert!(document["entries"][0]["on_disk"].is_null());
        assert_eq!(document["entries"][0]["apparent"], 3);
    }

    #[test]
    fn path_lists_and_jsonl_export_every_selected_entry() {
        let (_dir, tree) = fixture();
        let selected: Vec<_> = (1..tree.len()).map(NodeId::from_index).collect();
        let mut out = Vec::new();
        let written = write_rows(
            &tree,
            &selected,
            &Options {
                format: Format::Txt,
                ..Default::default()
            },
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), selected.len());
        assert_eq!(written.rows, selected.len());
        for line in text.lines() {
            assert!(std::path::Path::new(line).is_absolute());
            assert!(std::path::Path::new(line).starts_with(tree.root_path()));
            assert!(std::path::Path::new(line).exists());
        }

        let jsonl = export(
            &tree,
            &Options {
                format: Format::Jsonl,
                absolute_paths: true,
                ..Default::default()
            },
        );
        let entries: Vec<serde_json::Value> = jsonl
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(entries.len(), selected.len());
        for entry in entries {
            assert!(std::path::Path::new(entry["path"].as_str().unwrap()).is_absolute());
            assert!(entry["on_disk"].is_u64());
            assert!(entry["lossy"].is_boolean());
            assert!(entry.get("created").is_some());
        }
        let csv = export(
            &tree,
            &Options {
                absolute_paths: true,
                ..Default::default()
            },
        );
        assert!(
            csv.contains(
                &tree
                    .path_of(selected[0])
                    .to_string_lossy()
                    .replace('"', "\"\"")
            )
        );
        assert_eq!(Format::parse("NDJSON"), Some(Format::Jsonl));
        assert_eq!(Format::parse("text"), Some(Format::Txt));
    }

    #[cfg(unix)]
    #[test]
    fn text_path_list_refuses_line_breaks_without_replacing_destination() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("line\nbreak"), b"test").unwrap();
        let tree = Scanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let output = tempfile::tempdir().unwrap();
        let target = output.path().join("paths.txt");
        std::fs::write(&target, "original").unwrap();
        let error = to_path(
            &tree,
            None,
            &Options {
                format: Format::Txt,
                ..Default::default()
            },
            &target,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "original");
        let jsonl = export(
            &tree,
            &Options {
                format: Format::Jsonl,
                ..Default::default()
            },
        );
        assert_eq!(jsonl.lines().count(), 1);
        let entry: serde_json::Value = serde_json::from_str(jsonl.trim()).unwrap();
        assert_eq!(entry["path"], "line\nbreak");
    }

    #[test]
    fn failed_report_keeps_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comparison.md");
        std::fs::write(&path, "original").unwrap();
        let result: io::Result<()> = atomic_write(&path, |out| {
            out.write_all(b"partial report")?;
            Err(io::Error::other("report failed"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    use crate::{ScanOptions, Scanner};

    /// ```text
    /// root/
    ///   a/
    ///     one.bin      4096 bytes
    ///     large.bin    256 KiB
    ///   b,tricky/      a comma in a directory name
    ///     two"odd.txt  a quote in a file name
    ///   plain.txt
    /// ```
    fn fixture() -> (tempfile::TempDir, SizeTree) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/one.bin"), vec![0u8; 4096]).unwrap();
        std::fs::write(root.join("a/large.bin"), vec![0u8; 256 * 1024]).unwrap();
        std::fs::create_dir_all(root.join("b,tricky")).unwrap();
        // A quote is legal in a filename on Unix and not on Windows, so the
        // awkward name is only created where it can be.
        #[cfg(unix)]
        std::fs::write(root.join("b,tricky/two\"odd.txt"), b"hello").unwrap();
        #[cfg(not(unix))]
        std::fs::write(root.join("b,tricky/two-odd.txt"), b"hello").unwrap();
        std::fs::write(root.join("plain.txt"), b"hi").unwrap();

        let tree = Scanner::new(ScanOptions::default()).scan(root).unwrap();
        (dir, tree)
    }

    fn export(tree: &SizeTree, opts: &Options) -> String {
        let mut out = Vec::new();
        write_tree(tree, opts, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// An export lands whole or not at all.
    ///
    /// **Measured, and it is somebody's baseline at that path.** An export of
    /// 228,058 entries to a slow destination, killed four and a half seconds
    /// in: the seven-row baseline that had been there was gone, and in its
    /// place sat 15,487 rows of valid CSV — a file the reader accepts and the
    /// comparison believes, reporting every entry past the cut as *new*. A cut
    /// in the middle of a row is caught, because the reader refuses a line
    /// with the wrong number of fields; a cut at a line boundary is not, and
    /// most of them are.
    #[test]
    fn an_export_replaces_the_old_file_only_once_it_is_whole() {
        let (_dir, tree) = fixture();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("baseline.csv");
        std::fs::write(&path, "path,name,kind\nold,old,file\n").unwrap();

        let written = to_path(&tree, None, &Options::default(), &path).unwrap();
        assert!(written.rows > 0);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("path,name,kind,on_disk"), "{text:.60}");
        assert!(
            !text.contains("old,old,file"),
            "the old rows are still there"
        );
        assert_eq!(
            beside(home.path()),
            0,
            "a temporary file was left beside the export"
        );
    }

    /// The file that was there is *replaced*, not rewritten in place.
    ///
    /// Which is the whole of it: `File::create` empties the destination before
    /// the first row is written, so everything between that moment and the
    /// last row is a window in which the baseline is already gone. Held open
    /// from before the export, the old file is still readable through that
    /// handle afterwards and still says what it always said — because what the
    /// export wrote was a different file, and the rename only changed which
    /// name points where.
    ///
    /// Unix-only because it is the platform that will let a test watch an
    /// unlinked file: on Windows the same call is made in the same order, and
    /// the evidence for it there is the measurement in `to_path`'s own note —
    /// a killed export, the baseline intact, and the half-written file sitting
    /// beside it under its own name.
    #[cfg(unix)]
    #[test]
    fn the_old_file_is_replaced_rather_than_rewritten() {
        use std::io::{Read, Seek};

        let (_dir, tree) = fixture();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("baseline.csv");
        std::fs::write(
            &path,
            "path,name,kind
old,old,file
",
        )
        .unwrap();
        let mut held = std::fs::File::open(&path).unwrap();

        to_path(&tree, None, &Options::default(), &path).unwrap();

        let mut was = String::new();
        held.seek(std::io::SeekFrom::Start(0)).unwrap();
        held.read_to_string(&mut was).unwrap();
        assert_eq!(
            was,
            "path,name,kind
old,old,file
",
            "the export was written over the file that was already there"
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("plain.txt"),
            "and the new one is at the path"
        );
    }

    /// And when it cannot land, what was there is untouched.
    ///
    /// The destination here is a directory, which is a rename that fails on
    /// both platforms — the same shape as a full disk or a permission, and the
    /// one a test can produce without either.
    #[test]
    fn an_export_that_cannot_land_leaves_what_was_there_alone() {
        let (_dir, tree) = fixture();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("in-the-way");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("evidence.txt"), b"still here").unwrap();

        let refused = to_path(&tree, None, &Options::default(), &path);
        assert!(refused.is_err(), "an export landed on a directory");
        assert!(path.is_dir(), "the directory was replaced");
        assert_eq!(
            std::fs::read_to_string(path.join("evidence.txt")).unwrap(),
            "still here"
        );
        assert_eq!(
            beside(home.path()),
            0,
            "the temporary file outlived the failure"
        );
    }

    /// A row selection goes through the same door as the whole tree.
    #[test]
    fn an_export_of_chosen_rows_writes_those_rows() {
        let (_dir, tree) = fixture();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("some.csv");
        let one = tree
            .find(&tree.root_path().join("plain.txt"))
            .expect("plain.txt");

        let written = to_path(&tree, Some(&[one]), &Options::default(), &path).unwrap();
        assert_eq!(written.rows, 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("plain.txt"), "{text}");
        assert!(!text.contains("large.bin"), "{text}");
    }

    /// How many half-written files are sitting in there.
    fn beside(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("kosha-writing")
            })
            .count()
    }

    #[test]
    fn csv_starts_with_its_header() {
        let (_dir, tree) = fixture();
        let csv = export(&tree, &Options::default());
        assert!(csv.starts_with(CSV_HEADER), "{csv}");
    }

    #[test]
    fn every_csv_row_has_the_same_number_of_columns() {
        let (_dir, tree) = fixture();
        let csv = export(&tree, &Options::default());
        let columns = CSV_HEADER.trim_end().split(',').count();

        for line in csv.lines().skip(1) {
            assert_eq!(
                count_csv_fields(line),
                columns,
                "wrong column count in: {line}"
            );
        }
    }

    /// Count RFC 4180 fields, respecting quoting — which is the whole point.
    fn count_csv_fields(line: &str) -> usize {
        let mut fields = 1;
        let mut quoted = false;
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    chars.next();
                }
                '"' => quoted = !quoted,
                ',' if !quoted => fields += 1,
                _ => {}
            }
        }
        fields
    }

    #[test]
    fn a_comma_in_a_name_does_not_shift_the_columns() {
        let (_dir, tree) = fixture();
        let csv = export(&tree, &Options::default());

        let row = csv
            .lines()
            .find(|line| line.contains("tricky"))
            .expect("the awkward directory is missing");
        assert!(
            row.starts_with("\"b,tricky\","),
            "the comma was not quoted: {row}"
        );
        assert_eq!(count_csv_fields(row), 9);
    }

    /// A filename can begin with the character that makes a spreadsheet run it.
    #[test]
    fn a_name_a_spreadsheet_would_evaluate_is_quoted() {
        for name in [
            "=cmd|'/c calc'!A1",
            "+1+1",
            "-2+3",
            "@SUM(A1)",
            "\treally",
            "=HYPERLINK(\"http://elsewhere/\")",
        ] {
            let written = field(name);
            assert!(
                written.starts_with('"') && written.ends_with('"'),
                "{name:?} went out as {written:?}, which a spreadsheet reads as a formula"
            );
        }
        // And nothing else is quoted for the sake of it: a leading character is
        // only dangerous where it is leading.
        for name in ["report=final.pdf", "a+b.txt", "x@y.txt", "ordinary.txt"] {
            assert_eq!(field(name), name, "{name:?} was quoted for no reason");
        }
    }

    /// Quoting is not mangling, and this is the assertion that says so: the
    /// reader that consumes these exports gives the path back exactly.
    #[test]
    fn quoting_a_formula_field_changes_no_byte_of_the_path() {
        let dir = tempfile::tempdir().unwrap();
        // Legal on every platform, and the first character of each is the point.
        for name in ["=one.txt", "+two.txt", "@three.txt", "ordinary.txt"] {
            std::fs::write(dir.path().join(name), b"xx").unwrap();
        }
        let tree = crate::Scanner::new(crate::ScanOptions::default())
            .scan(dir.path())
            .unwrap();

        let csv = export(&tree, &Options::default());
        let baseline = crate::compare::read_csv(std::io::Cursor::new(csv.as_bytes()))
            .expect("the export reads back");

        for name in ["=one.txt", "+two.txt", "@three.txt", "ordinary.txt"] {
            assert!(
                baseline.get(name).is_some(),
                "{name} did not come back out of its own export"
            );
        }
    }

    #[test]
    fn a_quote_is_doubled_not_dropped() {
        assert_eq!(field(r#"say "hi""#), r#""say ""hi""""#);
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("a\nb"), "\"a\nb\"");
        // Nothing special: left alone, so ordinary paths stay readable.
        assert_eq!(field("plain.txt"), "plain.txt");
    }

    fn field(value: &str) -> String {
        let mut out = String::new();
        csv_field(&mut out, value);
        out
    }

    #[test]
    fn the_same_tree_exports_identically_twice() {
        // The whole point of exporting is diffing two of them, so an export
        // that reorders itself between runs is worse than useless.
        let (_dir, tree) = fixture();
        let opts = Options::default();
        assert_eq!(export(&tree, &opts), export(&tree, &opts));

        let json = Options {
            format: Format::Json,
            ..opts
        };
        // The JSON header carries a timestamp, so compare the entries only.
        let (a, b) = (export(&tree, &json), export(&tree, &json));
        let entries = |s: &str| s[s.find("\"entries\"").unwrap()..].to_string();
        assert_eq!(entries(&a), entries(&b));
    }

    #[test]
    fn paths_are_relative_and_use_forward_slashes() {
        let (_dir, tree) = fixture();
        let csv = export(&tree, &Options::default());

        for line in csv.lines().skip(1) {
            let path = line.split(',').next().unwrap();
            assert!(
                !path.contains('\\'),
                "a backslash survived into a path: {line}"
            );
        }
        assert!(csv.contains("a/one.bin"), "{csv}");
    }

    /// A directory's figures are its whole subtree, which is what makes the
    /// column useful and what stops it adding up.
    ///
    /// Both halves are asserted because both are load-bearing: a row for a
    /// directory has to say what deleting it would free, and anybody summing the
    /// column double-counts descendants included in directory totals.
    /// Measured on `/usr`: 30,484,357,120 summed against a tree
    /// holding 8,658,124,800.
    #[test]
    fn a_directory_row_carries_its_whole_subtree_and_so_the_column_does_not_add_up() {
        let (_dir, tree) = fixture();
        let csv = export(&tree, &Options::default());

        let mut rows: Vec<(String, String, u64)> = Vec::new();
        for line in csv.lines().skip(1) {
            let fields = split_csv(line);
            rows.push((
                fields[0].clone(),
                fields[2].clone(),
                fields[3].parse().unwrap(),
            ));
        }
        assert!(rows.len() > 4, "the fixture is too small to nest");

        // Every directory row holds at least what its own children hold.
        let mut nested = 0usize;
        for (path, kind, on_disk) in &rows {
            if kind != "dir" {
                continue;
            }
            let prefix = format!("{path}/");
            let below: u64 = rows
                .iter()
                .filter(|(p, _, _)| p.starts_with(&prefix))
                .map(|(_, _, bytes)| bytes)
                .sum();
            if below == 0 {
                continue;
            }
            nested += 1;
            assert!(
                *on_disk >= below,
                "`{path}` says {on_disk} and the rows under it say {below}"
            );
        }
        assert!(nested > 0, "no directory in the fixture had rows below it");

        // And therefore the column over-counts, which is the thing to warn about
        // rather than to hide.
        let summed: u64 = rows.iter().map(|(_, _, bytes)| bytes).sum();
        assert!(
            summed > tree.total_allocated(),
            "summing {summed} did not exceed the tree's {} — if this ever \
             becomes false, a directory row stopped carrying its subtree and the \
             treemap is reading the wrong number",
            tree.total_allocated()
        );
    }

    /// RFC 4180 by hand, for the test that needs to read the fields back.
    fn split_csv(line: &str) -> Vec<String> {
        let mut out = vec![String::new()];
        let mut quoted = false;
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    chars.next();
                    out.last_mut().unwrap().push('"');
                }
                '"' => quoted = !quoted,
                ',' if !quoted => out.push(String::new()),
                _ => out.last_mut().unwrap().push(ch),
            }
        }
        out
    }

    #[test]
    fn the_root_is_not_a_row() {
        let (_dir, tree) = fixture();
        let csv = export(&tree, &Options::default());
        // Its relative path would be empty, and its total is in the header.
        for line in csv.lines().skip(1) {
            assert!(!line.starts_with(','), "the root became a row: {line}");
        }
    }

    #[test]
    fn filters_leave_out_exactly_what_they_say() {
        let (_dir, tree) = fixture();

        let files_only = export(
            &tree,
            &Options {
                directories: false,
                ..Options::default()
            },
        );
        assert!(files_only.lines().skip(1).all(|l| l.contains(",file,")));

        let dirs_only = export(
            &tree,
            &Options {
                files: false,
                ..Options::default()
            },
        );
        assert!(dirs_only.lines().skip(1).all(|l| l.contains(",dir,")));

        // The floor is on-disk size, not apparent size — so it has to be well
        // clear of one allocation unit to mean anything. A two-byte file
        // occupies a whole cluster, which is 4 KiB on most NTFS and ext4
        // volumes, and would sail past a 4 KiB threshold quite correctly.
        let big = export(
            &tree,
            &Options {
                min_size: 128 * 1024,
                ..Options::default()
            },
        );
        assert!(big.contains("a/large.bin"));
        assert!(
            !big.contains("a/one.bin"),
            "a 4 KiB file passed a 128 KiB floor"
        );
        assert!(
            !big.contains("plain.txt"),
            "a 2-byte file passed a 128 KiB floor"
        );
    }

    #[test]
    fn the_report_counts_what_it_wrote() {
        let (_dir, tree) = fixture();
        let mut out = Vec::new();
        let opts = Options {
            directories: false,
            ..Options::default()
        };
        let written = write_tree(&tree, &opts, &mut out).unwrap();

        let rows = String::from_utf8(out).unwrap().lines().count() - 1;
        assert_eq!(written.rows, rows);
        assert_eq!(written.lossy, 0);
        // Files only, so this is the sum of the file sizes and nothing double
        // counted through a directory total.
        assert!(written.bytes >= 4096);
    }

    #[test]
    fn json_parses_and_carries_the_scan_it_came_from() {
        let (_dir, tree) = fixture();
        let json = export(
            &tree,
            &Options {
                format: Format::Json,
                ..Options::default()
            },
        );

        // No parser here, so check the shape that a parser would reject.
        assert!(json.starts_with('{') && json.trim_end().ends_with('}'));
        assert!(json.contains("\"root\":"));
        assert!(json.contains("\"exported\":"));
        assert!(json.contains("\"entries\": ["));
        assert!(
            !json.contains(",\n  ]"),
            "a trailing comma before the closing bracket"
        );
        assert_eq!(
            json.matches("\"path\":").count(),
            export(&tree, &Options::default()).lines().count() - 1,
            "the two formats disagree about how many rows there are"
        );
    }

    #[test]
    fn json_distinguishes_access_errors_from_stopped_and_depth_limited_scans() {
        let (_dir, tree) = fixture();
        for (partial, unreadable, cancelled, depth_limited, access_limited) in [
            (false, 0, false, false, false),
            (true, 2, false, false, true),
            (true, 0, true, false, false),
            (true, 0, false, true, false),
            (true, 3, true, true, true),
            // Older records may carry only the inclusive partial flag.
            (true, 0, false, false, true),
        ] {
            let mut stats = tree.stats().clone();
            stats.partial = partial;
            stats.unreadable = unreadable;
            stats.cancelled = cancelled;
            stats.depth_limited = depth_limited;
            let record = SizeTree::from_parts(
                tree.nodes_slice().to_vec(),
                crate::NameArena::from_bytes(tree.names_bytes().to_vec()),
                crate::Owners::default(),
                tree.root_path().to_path_buf(),
                stats,
            );
            let output = export(
                &record,
                &Options {
                    format: Format::Json,
                    ..Default::default()
                },
            );
            let object: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(
                object["partial"], partial,
                "legacy inclusive flag remains compatible"
            );
            assert_eq!(object["access_limited"], access_limited);
            assert_eq!(object["incomplete"], cancelled || depth_limited);
            assert_eq!(object["cancelled"], cancelled);
            assert_eq!(object["depth_limited"], depth_limited);
            assert_eq!(object["unreadable_entries"], unreadable);
            assert_eq!(object["lower_bound"], partial || cancelled || depth_limited);
        }
    }

    #[test]
    fn an_empty_selection_still_produces_a_valid_document() {
        let (_dir, tree) = fixture();

        let csv = {
            let mut out = Vec::new();
            write_rows(&tree, &[], &Options::default(), &mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(csv, CSV_HEADER, "an empty CSV lost its header");

        let json = {
            let mut out = Vec::new();
            write_rows(
                &tree,
                &[],
                &Options {
                    format: Format::Json,
                    ..Options::default()
                },
                &mut out,
            )
            .unwrap();
            String::from_utf8(out).unwrap()
        };
        assert!(json.contains("\"entries\": [\n  ]"), "{json}");
    }

    #[test]
    fn flags_are_named_and_dir_is_not_among_them() {
        assert_eq!(flag_list(Flags::DIR), "");
        assert_eq!(flag_list(Flags::SPARSE), "sparse");
        assert_eq!(flag_list(Flags::SPARSE | Flags::PARTIAL), "sparse;partial");
        // A cloud placeholder, from Windows attributes or macOS `SF_DATALESS`.
        assert_eq!(flag_list(Flags::OFFLINE | Flags::SPARSE), "sparse;offline");
    }

    /// Raw Unix names, including bytes a host filesystem may refuse to create.
    ///
    /// The data model's claim is that a filename is bytes rather than text, so
    /// one that is not valid UTF-8 survives a round trip and one containing a
    /// newline does not corrupt the document. Construct the invalid name in the
    /// arena so this also exercises exports on macOS filesystems requiring UTF-8.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_text_survives_the_export() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cafe.txt"), b"x").unwrap();
        // A newline is legal in a filename and breaks every line-based tool.
        std::fs::write(dir.path().join("two\nlines.txt"), b"y").unwrap();

        let tree = Scanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let id = tree
            .child_named(tree.root(), OsStr::new("cafe.txt"))
            .unwrap();
        let mut names = crate::NameArena::from_bytes(tree.names_bytes().to_vec());
        // A lone 0xE9 — latin-1 'é', and not a valid UTF-8 sequence by itself.
        let (offset, len) = names.push(OsStr::from_bytes(b"caf\xe9.txt"));
        let mut nodes = tree.nodes_slice().to_vec();
        nodes[id.index()].name_off = offset;
        nodes[id.index()].name_len = len;
        let tree = SizeTree::from_parts(
            nodes,
            names,
            crate::Owners::default(),
            tree.root_path().to_path_buf(),
            tree.stats().clone(),
        );
        let csv = export(&tree, &Options::default());

        // The undecodable name is written lossily and *marked*, so a reader
        // knows this row cannot be used to reopen the file.
        let lossy_rows: Vec<&str> = csv
            .lines()
            .skip(1)
            .filter(|line| line.ends_with(",lossy"))
            .collect();
        assert_eq!(lossy_rows.len(), 1, "expected exactly one lossy row: {csv}");
        assert!(lossy_rows[0].contains('\u{fffd}'));

        // The newline is preserved rather than stripped — quoted, per RFC 4180,
        // so the row is still one record and the columns still line up.
        assert!(
            csv.contains("\"two\nlines.txt\""),
            "the newline was not preserved and quoted: {csv}"
        );
        let mut written = Vec::new();
        let report = write_tree(&tree, &Options::default(), &mut written).unwrap();
        assert_eq!(report.lossy, 1, "the count did not match the marked rows");

        // Display sanitising is a separate concern and must not have leaked
        // into the data: `?` is what a terminal sees, never what is exported.
        assert!(!csv.contains("two?lines.txt"));
        assert_eq!(crate::format::printable("two\nlines.txt"), "two?lines.txt");
    }

    /// A backslash is a perfectly legal character in a Unix filename, and
    /// the separator normalisation that makes a Windows export diff against
    /// a Linux one must not touch it. Rewriting it invents a directory level
    /// that does not exist, so the row stops describing the file it names —
    /// and `--since` then compares a path against one nothing will ever have.
    ///
    /// Windows cannot create this name, which is exactly why the rewrite is
    /// safe there and only there.
    #[cfg(unix)]
    #[test]
    fn a_backslash_in_a_unix_name_is_not_a_separator() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/back\\slash.txt"), b"x").unwrap();

        let tree = Scanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let csv = export(&tree, &Options::default());

        assert!(
            csv.contains("real/back\\slash.txt"),
            "the backslash was rewritten into a separator: {csv}"
        );
        assert!(
            !csv.contains("real/back/slash.txt"),
            "the export invented a directory that is not in the tree: {csv}"
        );
    }

    #[test]
    fn formats_round_trip_through_their_names() {
        assert_eq!(Format::parse("csv"), Some(Format::Csv));
        assert_eq!(Format::parse(" JSON "), Some(Format::Json));
        assert_eq!(Format::parse("yaml"), None);
        assert_eq!(Format::Csv.extension(), "csv");
        assert_eq!(Format::Json.extension(), "json");
    }
}
