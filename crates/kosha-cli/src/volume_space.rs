// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::path::Path;
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use kosha_core::volume_space::{SpaceCapture, SpaceGap, SpaceSummary};
use kosha_core::{ScanOptions, Scanner, format, sayln};
use serde_json::json;

pub fn run(args: &[String]) -> ExitCode {
    if args.is_empty() || matches!(args, [help] if matches!(help.as_str(), "-h" | "--help")) {
        sayln!(
            "kosha space ROOT [--format text|jsonl]\n\nScan a mounted local volume and compare its filesystem counters with scanned allocation."
        );
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
    let (root, jsonl) = match args {
        [root] => (root, false),
        [root, flag, kind] if flag == "--format" && matches!(kind.as_str(), "text" | "jsonl") => {
            (root, kind == "jsonl")
        }
        _ => return Err("see kosha space --help for supported options".into()),
    };
    let root = Path::new(root);
    let capture =
        SpaceCapture::start(root).ok_or("expected a supported mounted local volume root")?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let tree = Scanner::new(ScanOptions {
        exact_allocated: true,
        dedup_hardlinks: Some(true),
        ..ScanOptions::default()
    })
    .with_cancel(Arc::clone(&stop))
    .scan(root)?;
    let summary = capture
        .finish(&tree)
        .filter(|_| !stop.load(Ordering::Relaxed))
        .ok_or("scan did not complete on the original volume")?;
    let gap_bytes = match summary.gap {
        SpaceGap::Unexplained(bytes) => Some(bytes),
        _ => None,
    };
    if jsonl {
        sayln!(
            "{}",
            json!({
                "schema": "kosha.volume-space.v1",
                "root": tree.root_path().to_string_lossy(),
                "filesystem": summary.usage.map(|usage| json!({
                    "capacity_bytes": usage.total,
                    "free_bytes": usage.free,
                    "available_bytes": usage.available,
                    "used_bytes": summary.reported_used(),
                    "used_scope": if cfg!(windows) { "caller" } else { "filesystem" },
                })),
                "scan": {
                    "allocated_bytes": summary.scanned_bytes,
                    "volume_trash_bytes_included": summary.scanned_trash_bytes,
                    "estimated": summary.estimated,
                    "partial": summary.partial,
                    "unreadable_entries": summary.unreadable,
                    "skipped_mounts": summary.skipped_mounts,
                    "depth_limited": summary.depth_limited,
                },
                "gap": { "status": summary.gap.code(), "bytes": gap_bytes },
                "filesystem_snapshots": { "count": summary.filesystem_snapshot_count },
            })
        );
    } else {
        let size =
            |bytes: Option<u64>| bytes.map_or_else(|| "Unavailable".to_owned(), format::bytes);
        sayln!(
            "{}  {}",
            SpaceSummary::usage_label(),
            size(summary.reported_used())
        );
        sayln!("Scanned allocation  {}", size(summary.scanned_bytes));
        if let Some(bytes) = summary.scanned_trash_bytes {
            sayln!(
                "Volume Trash folders (included in scan)  {}",
                format::bytes(bytes)
            );
        }
        if let Some(usage) = summary.usage {
            sayln!("Free  {}", format::bytes(usage.free));
            sayln!("Available to you  {}", format::bytes(usage.available));
        }
        sayln!("Unreadable entries  {}", summary.unreadable);
        sayln!("Other mounts skipped  {}", summary.skipped_mounts);
        if let Some(count) = summary.filesystem_snapshot_count {
            sayln!("Filesystem snapshots  {count} (space unavailable)");
        }
        if let Some(bytes) = gap_bytes {
            sayln!("Unexplained  {}", format::bytes(bytes));
        }
        sayln!("{}", summary.gap.explanation());
    }
    Ok(())
}
