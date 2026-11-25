// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::path::Path;
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use kosha_core::snapshot::store;
use kosha_core::volume_history::{ScanObservation, VolumeId, folder_growth};
use kosha_core::{ScanOptions, Scanner, format, say, sayln};
use serde_json::json;

const USAGE: &str = "\
kosha history record ROOT
kosha history list ROOT [--format text|jsonl]
kosha history growth ROOT [--format text|jsonl]

Record a fresh scan or list previously recorded observations for a mounted
local volume. The volume must have a supported persistent identifier.
Growth ranks root folders by allocation change between the newest two
observations. Use the listed snapshot paths with kosha compare for details.
";

pub fn run(args: &[String]) -> ExitCode {
    if args.is_empty() || matches!(args, [help] if matches!(help.as_str(), "-h" | "--help")) {
        say!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match execute(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn execute(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [command, root, options @ ..] = args else {
        return Err("expected history record, list, or growth and a volume root".into());
    };
    let root = Path::new(root);
    let dir = store::history_dir().ok_or("application data directory is unavailable")?;
    match (command.as_str(), options) {
        ("record", []) => {
            let capture = ScanObservation::start(root)
                .ok_or("expected a mounted local volume root with a persistent identifier")?;
            let stop = Arc::new(AtomicBool::new(false));
            let signal = Arc::clone(&stop);
            ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
            let tree = Scanner::new(ScanOptions::default())
                .with_cancel(Arc::clone(&stop))
                .scan(root)?;
            let observation = capture
                .finish(&tree)
                .filter(|_| !stop.load(Ordering::Relaxed))
                .ok_or("scan did not complete on the original volume; history was not recorded")?;
            store::save_history_in(&dir, &observation)?;
            sayln!("Recorded a drive history observation.");
        }
        ("list" | "growth", []) => read(command, root, &dir, false)?,
        ("list" | "growth", [flag, style])
            if flag == "--format" && matches!(style.as_str(), "text" | "jsonl") =>
        {
            read(command, root, &dir, style == "jsonl")?;
        }
        _ => return Err("see kosha history --help for supported commands".into()),
    }
    Ok(())
}

fn read(
    command: &str,
    root: &Path,
    dir: &Path,
    jsonl: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let volume = VolumeId::at_root(root)
        .ok_or("expected a mounted local volume root with a persistent identifier")?;
    let history = store::list_history_in(dir, &volume)?;
    if command == "growth" {
        return growth(&history, jsonl);
    }
    if history.is_empty() && !jsonl {
        sayln!("No observations recorded for this volume.");
    }
    for entry in history {
        let record = entry.record;
        let path = record.path.to_string_lossy();
        if jsonl {
            sayln!(
                "{}",
                json!({
                    "type": "observation",
                    "schema": "kosha.volume-history.v1",
                    "time_unix_ms": record.meta.at_ms(),
                    "snapshot": path,
                    "root": record.meta.root.to_string_lossy(),
                    "partial": entry.partial,
                    "usage": entry.usage.map(|usage| json!({
                        "total_bytes": usage.total,
                        "free_bytes": usage.free,
                        "available_bytes": usage.available
                    }))
                })
            );
        } else {
            let free = entry.usage.map_or_else(
                || "free space unavailable".to_owned(),
                |usage| format!("{} free", format::bytes(usage.free)),
            );
            sayln!(
                "{}  {}{}  {}",
                record.meta.at_ms(),
                free,
                if entry.partial {
                    " · partial scan"
                } else {
                    ""
                },
                format::printable(&path)
            );
        }
    }
    Ok(())
}

fn growth(history: &[store::HistoryEntry], jsonl: bool) -> Result<(), Box<dyn std::error::Error>> {
    let [.., before, after] = history else {
        return Err("record at least two observations to show folder growth".into());
    };
    let (_, older) = store::open(&before.record.path)?;
    let (_, newer) = store::open(&after.record.path)?;
    let rows = folder_growth(&older, &newer)
        .ok_or("folder growth requires complete observations with compatible scan coverage")?;
    let free_change = before
        .usage
        .zip(after.usage)
        .map(|(a, b)| i128::from(b.free) - i128::from(a.free));
    let capacity_changed = before
        .usage
        .zip(after.usage)
        .is_some_and(|(a, b)| a.total != b.total);
    if jsonl {
        sayln!(
            "{}",
            json!({
                "type": "interval", "schema": "kosha.volume-growth.v1",
                "from_unix_ms": before.record.meta.at_ms(), "to_unix_ms": after.record.meta.at_ms(),
                "from_snapshot": before.record.path.to_string_lossy(),
                "to_snapshot": after.record.path.to_string_lossy(),
                "free_change_bytes": free_change.map(wire_delta).transpose()?,
                "capacity_changed": capacity_changed,
            })
        );
    } else {
        sayln!("Measured allocation changes in top-level folders (bytes)");
        if let Some(change) = free_change {
            sayln!("Volume free space change: {change:+} bytes");
        }
        if capacity_changed {
            sayln!("Volume capacity also changed between observations.");
        }
    }
    if rows.is_empty() && !jsonl {
        sayln!("No allocation changes in top-level folders.");
    }
    for row in rows {
        let name = row.name.to_string_lossy();
        if jsonl {
            sayln!(
                "{}",
                json!({
                    "type": "folder", "name": name, "before_bytes": row.before,
                    "after_bytes": row.after, "delta_bytes": wire_delta(row.delta())?,
                })
            );
        } else {
            sayln!("{:+}  {}", row.delta(), format::printable(&name));
        }
    }
    Ok(())
}

fn wire_delta(value: i128) -> Result<serde_json::Number, &'static str> {
    serde_json::Number::from_i128(value).ok_or("allocation delta exceeds the JSON integer range")
}
