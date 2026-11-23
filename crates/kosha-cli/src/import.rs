// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `kosha import`: read another tool's listing into the snapshot store.
//!
//! Opens exactly the one file named (or reads standard input), and writes one
//! new record through the snapshot store. Nothing inside the listing is ever
//! opened, checked or scanned, and nothing is executed: the user runs ncdu,
//! over ssh or anywhere else, and hands kosha the file it wrote.

use std::io::Read;
use std::path::{Path, PathBuf};

use kosha_core::{format, sayln, snapshot};
use kosha_import::{Limits, RcloneSource};

pub const USAGE: &str = "\
kosha import — keep another tool's listing as a record kosha can browse

USAGE:
    kosha import --from ncdu FILE [--label TEXT] [--max-input-bytes N]
    kosha import --from rclone FILE|- [--label TEXT] [--root TEXT]
    kosha import --from qdirstat FILE|- [--label TEXT]

Reads an ncdu JSON export (`ncdu -o FILE`), uncompressed, gzip-compressed or
zstd-compressed (`ncdu -co FILE`), and keeps it in the snapshot folder as an
imported record. The record says, wherever it is shown, that it was imported
and not measured by kosha. Nothing named inside the listing is opened, and no
program is run. ncdu's binary export (`ncdu -O`) is not read.

OPTIONS:
    --from FORMAT        The listing's format: ncdu, rclone, qdirstat or auto
    --out FILE           Write a new snapshot to FILE instead of the snapshot store
    --label TEXT         Name the record (optional)
    --root TEXT          Source root for rclone (optional)
    --tool TEXT          Source tool version for rclone (optional)
    --observed-at TIME   When the rclone listing was observed (RFC 3339)
    --max-input-bytes N  Refuse input, or decompressed input, larger than N
                         bytes (default 8589934592, 8 GiB)
    -h, --help           Show this message

EXAMPLES:
    ncdu -o srv.json /srv
    kosha import --from ncdu srv.json --label \"build-box /srv\"
    ssh build-box ncdu -co- /srv | kosha import --from ncdu - --label \"build-box /srv\"
    kosha --snapshots
    kosha SAVED.kosha --stats
";

#[derive(Debug)]
pub struct Args {
    pub input: PathBuf,
    format: ListingFormat,
    pub out: Option<PathBuf>,
    pub root: Option<String>,
    pub tool: Option<String>,
    pub observed: Option<u64>,
    pub label: String,
    pub limits: Limits,
}

#[derive(Debug, PartialEq, Eq)]
enum ListingFormat {
    Ncdu,
    Rclone,
    Qdirstat,
    Auto,
}

#[derive(Debug)]
pub enum Parsed {
    Run(Box<Args>),
    Help,
}

pub fn parse(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut argv = argv.peekable();
    let mut from = None;
    let mut input = None;
    let mut out = None;
    let mut root = None;
    let mut tool = None;
    let mut observed = None;
    let mut label = String::new();
    let mut limits = Limits::default();
    let mut positional = false;
    while let Some(arg) = argv.next() {
        if positional {
            if input.replace(PathBuf::from(arg)).is_some() {
                return Err("kosha import takes one input".into());
            }
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--" => positional = true,
            "--from" => {
                from = Some(
                    argv.next()
                        .ok_or("--from needs a format: ncdu, rclone, qdirstat or auto")?,
                )
            }
            "--label" => label = argv.next().ok_or("--label needs a name")?,
            "--out" => out = Some(PathBuf::from(argv.next().ok_or("--out needs a path")?)),
            "--root" => root = Some(argv.next().ok_or("--root needs a source root")?),
            "--tool" => tool = Some(argv.next().ok_or("--tool needs a source tool name")?),
            "--observed-at" => {
                let value = argv
                    .next()
                    .ok_or("--observed-at needs an RFC 3339 timestamp")?;
                observed = Some(
                    format::parse_rfc3339(&value)
                        .and_then(|seconds| u64::try_from(seconds).ok())
                        .ok_or("--observed-at needs an RFC 3339 timestamp at or after 1970")?,
                );
            }
            "--max-input-bytes" => {
                let value = argv.next().ok_or("--max-input-bytes needs a number")?;
                limits.max_input_bytes = value.parse::<u64>().ok().filter(|&n| n > 0).ok_or_else(
                    || {
                        format!(
                            "--max-input-bytes needs a number of bytes, and `{value}` is not one"
                        )
                    },
                )?;
            }
            "-" => {
                if input.replace(PathBuf::from("-")).is_some() {
                    return Err("kosha import takes one input".into());
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}` for kosha import"));
            }
            other => {
                if input.replace(PathBuf::from(other)).is_some() {
                    return Err("kosha import takes one input".into());
                }
            }
        }
    }
    let format = match from.as_deref() {
        Some("ncdu") => ListingFormat::Ncdu,
        Some("rclone") => ListingFormat::Rclone,
        Some("qdirstat") => ListingFormat::Qdirstat,
        Some("auto") => ListingFormat::Auto,
        Some(other) => {
            return Err(format!(
                "unknown import format `{other}`; choose ncdu, rclone, qdirstat or auto"
            ));
        }
        None => {
            return Err(
                "kosha import needs --from ncdu, --from rclone, --from qdirstat or --from auto"
                    .into(),
            );
        }
    };
    if matches!(format, ListingFormat::Ncdu | ListingFormat::Qdirstat)
        && (root.is_some() || tool.is_some() || observed.is_some())
    {
        return Err("--root, --tool and --observed-at require --from rclone or --from auto".into());
    }
    let input = input.ok_or("kosha import needs a file to read, or - for standard input")?;
    Ok(Parsed::Run(Box::new(Args {
        input,
        format,
        out,
        root,
        tool,
        observed,
        label,
        limits,
    })))
}

/// The file's own name, never the path to it: the record travels, and the
/// folder the export sat in on this machine is nobody else's business.
fn source_name(input: &Path) -> String {
    if input == Path::new("-") {
        return "standard input".into();
    }
    input
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "input".into())
}

pub fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let reader: Box<dyn Read + Send> = if args.input == Path::new("-") {
        Box::new(std::io::stdin())
    } else {
        if std::fs::metadata(&args.input).is_ok_and(|about| about.is_dir()) {
            return Err(format!(
                "{} is a directory; kosha import reads a listing file (to scan a folder, \
                 run kosha on it directly)",
                format::printable(&args.input.to_string_lossy())
            )
            .into());
        }
        Box::new(std::fs::File::open(&args.input).map_err(|error| {
            format!(
                "could not read {}: {error}",
                format::printable(&args.input.to_string_lossy())
            )
        })?)
    };
    let name = source_name(&args.input);
    let now = std::time::SystemTime::now();
    let source = RcloneSource {
        source_name: &name,
        label: &args.label,
        root: args.root.as_deref().unwrap_or_default(),
        tool: args.tool.as_deref().unwrap_or_default(),
        observed: args.observed,
    };
    let imported = match args.format {
        ListingFormat::Ncdu => {
            kosha_import::import_ncdu(reader, &name, &args.label, now, &args.limits)
        }
        ListingFormat::Rclone => kosha_import::import_rclone(reader, &source, now, &args.limits),
        ListingFormat::Qdirstat => {
            kosha_import::import_qdirstat(reader, &name, &args.label, now, &args.limits)
        }
        ListingFormat::Auto => kosha_import::import_auto(reader, &source, now, &args.limits),
    }
    .map_err(|error| format!("{}: {error}", format::printable(&name)))?;
    if imported
        .meta
        .import
        .as_ref()
        .is_some_and(|import| import.format != "rclone-lsjson")
        && (args.root.is_some() || args.tool.is_some() || args.observed.is_some())
    {
        return Err("--root, --tool and --observed-at apply to rclone listings".into());
    }
    let path = if let Some(destination) = &args.out {
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        snapshot::write(&imported.tree, &imported.meta, temp.as_file_mut())?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(destination)
            .map_err(|error| error.error)?;
        destination.clone()
    } else {
        snapshot::store::save_record(&imported.tree, &imported.meta)?
    };

    let meta = &imported.meta;
    let import = meta.import.as_ref().expect("an import has provenance");
    let tree = &imported.tree;
    let stats = tree.stats();
    let approx = if tree.is_partial() { "~" } else { "" };
    sayln!(
        "IMPORTED  {}",
        if meta.label.is_empty() {
            format::printable(&meta.root.to_string_lossy()).into_owned()
        } else {
            format::printable(&meta.label).into_owned()
        }
    );
    sayln!("  {}", import.sentence());
    sayln!(
        "  {:<20}{}",
        "listing of",
        format::printable(&meta.root.to_string_lossy())
    );
    sayln!(
        "  {:<20}{} ({}, {})",
        "entries",
        format::count(stats.entries),
        format::plural(stats.dirs as usize, "directory"),
        format::plural(stats.files as usize, "file")
    );
    sayln!(
        "  {:<20}{approx}{}",
        "apparent",
        format::bytes(tree.total_logical())
    );
    if import.allocated_known {
        sayln!(
            "  {:<20}{approx}{} (as the source tool reported it)",
            "on disk",
            format::bytes(tree.total_allocated())
        );
    } else {
        sayln!("  {:<20}not recorded (apparent sizes only)", "on disk");
    }
    sayln!(
        "  {:<20}{}, {}, {} read",
        "input",
        format::printable(&name),
        imported.compression.label(),
        format::bytes(imported.bytes_read)
    );
    for (what, count) in [
        ("hard links grouped", stats.hardlinks_deduped),
        ("unreadable", stats.unreadable),
        ("other filesystems", stats.skipped_other_device),
        ("excluded by source", import.excluded_by_source),
        ("sizes not recorded", import.unknown_size),
        ("duplicate names", import.duplicate_paths),
        ("synthesized folders", import.synthesized_dirs),
        ("lossy names", import.lossy_names),
    ] {
        if count > 0 {
            sayln!("  {what:<20}{}", format::count(count));
        }
    }
    if !import.mtime_known {
        sayln!("  {:<20}not recorded", "modified times");
    }
    if import.format == "rclone-lsjson" && import.unknown_size > 0 {
        sayln!("  apparent totals are lower bounds because some sizes were unknown");
    }
    if tree.is_partial() {
        sayln!("  the listing marks entries as unreadable or excluded, so totals are a floor");
    }
    sayln!("  {:<20}{}", "imported to", path.display());
    if let Some(file) = path.file_name() {
        sayln!(
            "  {:<20}kosha \"{}\" --stats",
            "open with",
            file.to_string_lossy()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_line(line: &str) -> Result<Parsed, String> {
        parse(line.split_whitespace().map(str::to_string))
    }

    #[test]
    fn the_format_and_one_input_are_required() {
        assert!(parse_line("x.json").unwrap_err().contains("--from ncdu"));
        assert!(matches!(
            parse_line("--from rclone x.json --root drive: --observed-at 2026-01-01T00:00:00Z"),
            Ok(Parsed::Run(args)) if args.format == ListingFormat::Rclone && args.root.as_deref() == Some("drive:")
        ));
        assert!(
            parse_line("--from ncdu")
                .unwrap_err()
                .contains("standard input")
        );
        assert!(
            parse_line("--from ncdu a b")
                .unwrap_err()
                .contains("one input")
        );
        assert!(
            parse_line("--from ncdu --wat a")
                .unwrap_err()
                .contains("--wat")
        );
        assert!(matches!(parse_line("--help"), Ok(Parsed::Help)));
        match parse_line("--from ncdu - --label box --max-input-bytes 1000") {
            Ok(Parsed::Run(args)) => {
                assert_eq!(args.input, PathBuf::from("-"));
                assert_eq!(args.label, "box");
                assert_eq!(args.limits.max_input_bytes, 1000);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse_line("--from ncdu a --max-input-bytes 0").is_err());
    }

    #[test]
    fn auto_accepts_an_explicit_output_path() {
        assert!(matches!(
            parse_line("--from auto - --out kept.kosha"),
            Ok(Parsed::Run(args)) if args.format == ListingFormat::Auto
                && args.out.as_deref() == Some(Path::new("kept.kosha"))
        ));
    }

    #[test]
    fn only_the_file_name_is_recorded() {
        let deep = Path::new("some")
            .join("private")
            .join("place")
            .join("srv.json");
        assert_eq!(source_name(&deep), "srv.json");
        assert_eq!(source_name(Path::new("-")), "standard input");
    }

    #[test]
    fn every_option_the_help_screen_lists_is_accepted() {
        for line in USAGE.lines() {
            let Some(rest) = line.trim_start().strip_prefix("--") else {
                continue;
            };
            let flag = format!("--{}", rest.split_whitespace().next().unwrap());
            let outcome = parse_line(&format!("--from ncdu x {flag} 1"));
            assert!(
                !matches!(&outcome, Err(e) if e.contains("unknown option")),
                "{flag} is on the help screen and refused"
            );
        }
    }
}
