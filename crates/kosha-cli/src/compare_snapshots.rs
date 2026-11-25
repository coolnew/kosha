// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::File;
use std::path::Path;
use std::process::ExitCode;
use std::time::SystemTime;

use kosha_core::snapshot::{self, Meta};
use kosha_core::snapshot_compare::{Comparison, Row, Status};
use kosha_core::{ScanOptions, Scanner, SizeTree, format, say, sayln};
use serde_json::{Value, json};

const USAGE: &str =
    "kosha compare LEFT RIGHT [--mtime-tolerance SECONDS] [--format text|jsonl] [--verify]

Compare metadata from two folders, two Kosha snapshots, or one of each. Paths are relative
to their own roots and matched byte for byte. File comparison uses apparent
size and modification time. The default modification-time tolerance is 2
seconds. Exit status is 0 for matching metadata, 1 for known differences,
or 2 for an error or an indeterminate result without known differences.
Use --verify on Windows, macOS, or Linux to compare bytes in equal-size matching
files from two live folders. Links and unavailable files are skipped.
";

const TEXT_ROWS: u64 = 30;

pub fn run(args: &[String]) -> ExitCode {
    if matches!(args, [help] if matches!(help.as_str(), "--help" | "-h")) {
        say!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match compare(args) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn compare(args: &[String]) -> Result<u8, Box<dyn std::error::Error>> {
    let mut tolerance = 2;
    let mut jsonl = false;
    let mut verify = false;
    let mut paths = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        match arg {
            "--verify" => verify = true,
            "--mtime-tolerance" => {
                tolerance = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--mtime-tolerance needs a nonnegative number of seconds")?;
            }
            "--format" => match args.next() {
                Some("text") => jsonl = false,
                Some("jsonl") => jsonl = true,
                _ => return Err("--format needs text or jsonl".into()),
            },
            "--" => {
                paths.extend(args);
                break;
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown compare option {}", format::printable(option)).into());
            }
            path => paths.push(path),
        }
    }
    let [left, right] = paths.as_slice() else {
        return Err("compare needs two folders or snapshots. Run kosha compare --help.".into());
    };
    #[cfg(not(any(unix, windows)))]
    if verify {
        return Err("--verify is available on Windows, macOS, and Linux".into());
    }
    let (left_meta, left_tree, left_live) = read(left)?;
    let (right_meta, right_tree, right_live) = read(right)?;
    let comparison = Comparison::new(
        (&left_meta, &left_tree),
        (&right_meta, &right_tree),
        tolerance,
    )?;
    #[cfg(any(unix, windows))]
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(any(unix, windows))]
    let comparison = if verify {
        if !left_live || !right_live {
            return Err("--verify needs two live folders".into());
        }
        let signal = std::sync::Arc::clone(&stop);
        ctrlc::set_handler(move || signal.store(true, std::sync::atomic::Ordering::Relaxed))?;
        comparison
            .with_content_verification([left_tree.root_path(), right_tree.root_path()], &stop)?
    } else {
        comparison
    };
    if jsonl {
        sayln!(
            "{}",
            json!({
                "type": "header",
                "schema": "kosha.snapshot-compare.v1",
                "mtime_tolerance_seconds": tolerance,
                "left": source(left, &left_meta, left_live),
                "right": source(right, &right_meta, right_live)
            })
        );
    } else {
        let kind = if left_live || right_live {
            "Folder"
        } else {
            "Snapshot"
        };
        sayln!("{kind} metadata comparison (mtime tolerance: {tolerance} s)");
        sayln!(
            "Left:  {} ({})",
            format::printable(left),
            format::printable(&left_meta.strategy)
        );
        sayln!(
            "Right: {} ({})",
            format::printable(right),
            format::printable(&right_meta.strategy)
        );
    }
    let mut text_rows = 0;
    let summary = comparison.try_visit(|row| {
        #[cfg(any(unix, windows))]
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("comparison cancelled");
        }
        if jsonl {
            print_json_row(row);
        } else if !matches!(row.status, Status::MetadataMatch | Status::ContentMatch) {
            if text_rows < TEXT_ROWS {
                let path = String::from_utf8_lossy(row.path);
                let reasons = row
                    .reasons
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ");
                sayln!(
                    "{:<14} {} {}",
                    row.status.label(),
                    format::printable(&path),
                    reasons
                );
            }
            text_rows += 1;
        }
        Ok::<(), &str>(())
    })?;
    let code = summary.exit_code();
    if jsonl {
        let mut result = json!({
            "type": "summary",
            "left_only": summary.left_only,
            "right_only": summary.right_only,
            "changed": summary.changed,
            "metadata_match": summary.matched,
            "indeterminate": summary.indeterminate,
            "left_incomplete": summary.incomplete[0],
            "right_incomplete": summary.incomplete[1],
            "exit_code": code
        });
        if verify {
            result["content_match"] = summary.content_matched.into();
        }
        sayln!("{result}");
    } else {
        if text_rows > TEXT_ROWS {
            sayln!(
                "{} more differences or indeterminate paths",
                text_rows - TEXT_ROWS
            );
        }
        for ((label, live), incomplete) in [("Left", left_live), ("Right", right_live)]
            .into_iter()
            .zip(summary.incomplete)
        {
            if incomplete {
                let kind = if live { "folder scan" } else { "snapshot" };
                sayln!("{label} {kind} has incomplete coverage.");
            }
        }
        sayln!(
            "{} left-only, {} right-only, {} changed, {} metadata matches, {} indeterminate",
            summary.left_only,
            summary.right_only,
            summary.changed,
            summary.matched,
            summary.indeterminate
        );
        if verify {
            sayln!("{} verified content matches", summary.content_matched);
        }
    }
    Ok(code)
}

fn read(path: &str) -> Result<(Meta, SizeTree, bool), Box<dyn std::error::Error>> {
    let input = Path::new(path);
    let display = format::printable(path);
    let metadata = input
        .metadata()
        .map_err(|error| format!("{display}: {error}"))?;
    if metadata.is_dir() {
        let started = SystemTime::now();
        let tree = Scanner::new(ScanOptions::default())
            .scan(input)
            .map_err(|error| format!("{display}: {error}"))?;
        return Ok((Meta::of(&tree, started, ""), tree, true));
    }
    if !metadata.is_file() {
        return Err(format!("{display}: expected a folder or snapshot file").into());
    }
    let file = File::open(input).map_err(|error| format!("{display}: {error}"))?;
    let (meta, tree) = snapshot::read(file).map_err(|error| format!("{display}: {error}"))?;
    Ok((meta, tree, false))
}

fn source(path: &str, meta: &Meta, live: bool) -> Value {
    let mut source = json!({
        "file": path,
        "root": meta.root.to_string_lossy(),
        "label": meta.label,
        "taken_unix": meta.taken,
        "taken_ms": meta.taken_ms,
        "strategy": meta.strategy,
        "import": meta.import.as_ref().map(|item| json!({
            "format": item.format,
            "format_version": item.format_version,
            "tool": item.tool,
            "observed_unix": item.observed,
            "observed_basis": item.observed_basis,
            "imported_unix": item.imported_at,
            "source_name": item.source_name
        }))
    });
    if live {
        source["file"] = Value::Null;
        source["kind"] = "live-folder".into();
        source["path"] = path.into();
    }
    source
}

fn print_json_row(row: Row<'_>) {
    let path = String::from_utf8_lossy(row.path);
    let reasons: Vec<_> = row.reasons.into_iter().flatten().collect();
    let mut value = json!({
        "type": "entry",
        "path": path,
        "status": row.status.label(),
        "reasons": reasons
    });
    if matches!(path, std::borrow::Cow::Owned(_)) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut raw = String::with_capacity(row.path.len() * 2);
        for byte in row.path {
            raw.push(char::from(HEX[(byte >> 4) as usize]));
            raw.push(char::from(HEX[(byte & 15) as usize]));
        }
        value["path_bytes_hex"] = raw.into();
    }
    sayln!("{value}");
}
