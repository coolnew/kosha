// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::File;
use std::path::Path;
use std::process::ExitCode;

use kosha_core::snapshot::{self, Meta};
use kosha_core::snapshot_compare::{Comparison, Row, Status};
use kosha_core::{SizeTree, format, say, sayln};
use serde_json::{Value, json};

const USAGE: &str = "kosha compare LEFT RIGHT [--mtime-tolerance SECONDS] [--format text|jsonl]

Compare recorded metadata in two Kosha snapshot files. Paths are relative
to their own roots and matched byte for byte. File comparison uses apparent
size and modification time. The default modification-time tolerance is 2
seconds. Exit status is 0 for matching metadata, 1 for known differences,
or 2 for an error or an indeterminate result without known differences.
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
    let mut paths = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        match arg {
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
        return Err("compare needs two snapshot files. Run kosha compare --help.".into());
    };
    let (left_meta, left_tree) = read(left)?;
    let (right_meta, right_tree) = read(right)?;
    let comparison = Comparison::new(
        (&left_meta, &left_tree),
        (&right_meta, &right_tree),
        tolerance,
    )?;
    if jsonl {
        sayln!(
            "{}",
            json!({
                "type": "header",
                "schema": "kosha.snapshot-compare.v1",
                "mtime_tolerance_seconds": tolerance,
                "left": source(left, &left_meta),
                "right": source(right, &right_meta)
            })
        );
    } else {
        sayln!("Snapshot metadata comparison (mtime tolerance: {tolerance} s)");
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
    let summary = comparison.visit(|row| {
        if jsonl {
            print_json_row(row);
        } else if row.status != Status::MetadataMatch {
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
    });
    let code = summary.exit_code();
    if jsonl {
        sayln!(
            "{}",
            json!({
                "type": "summary",
                "left_only": summary.left_only,
                "right_only": summary.right_only,
                "changed": summary.changed,
                "metadata_match": summary.matched,
                "indeterminate": summary.indeterminate,
                "left_incomplete": summary.incomplete[0],
                "right_incomplete": summary.incomplete[1],
                "exit_code": code
            })
        );
    } else {
        if text_rows > TEXT_ROWS {
            sayln!(
                "{} more differences or indeterminate paths",
                text_rows - TEXT_ROWS
            );
        }
        for (label, incomplete) in ["Left", "Right"].into_iter().zip(summary.incomplete) {
            if incomplete {
                sayln!("{label} snapshot has incomplete coverage.");
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
    }
    Ok(code)
}

fn read(path: &str) -> Result<(Meta, SizeTree), Box<dyn std::error::Error>> {
    let input = Path::new(path);
    let display = format::printable(path);
    if !input
        .metadata()
        .map_err(|error| format!("{display}: {error}"))?
        .is_file()
    {
        return Err(format!("{display}: expected a snapshot file").into());
    }
    let file = File::open(input).map_err(|error| format!("{display}: {error}"))?;
    snapshot::read(file).map_err(|error| format!("{display}: {error}").into())
}

fn source(path: &str, meta: &Meta) -> Value {
    json!({
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
    })
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
