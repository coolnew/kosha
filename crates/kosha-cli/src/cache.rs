// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Explicit, read-only access to the same automatic records used by the GUI.

use kosha_core::{SizeTree, format, sayln, snapshot};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Cache {
    pub enabled: bool,
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct ListArgs {
    dir: Option<PathBuf>,
    output: String,
}

pub fn parse_list(args: impl IntoIterator<Item = String>) -> Result<ListArgs, String> {
    let mut result = ListArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let value = args.next().filter(|value| !value.starts_with("--"));
        match arg.as_str() {
            "--cache-dir" => {
                result.dir = Some(value.ok_or("--cache-dir needs a directory")?.into())
            }
            "--export" => {
                result.output = value.ok_or("--caches --export needs json or jsonl")?;
                if !matches!(result.output.as_str(), "json" | "jsonl") {
                    return Err("--caches supports --export json or jsonl".into());
                }
            }
            _ => {
                return Err(format!(
                    "--caches does not accept `{arg}`; use --cache-dir DIR or --export json|jsonl"
                ));
            }
        }
    }
    Ok(result)
}

/// Header inventory only: listing a multi-million-entry cache must not inflate
/// its tree or read the original drive. A recorded header is not a promise that
/// the compressed body is healthy; opening with --cache performs that check.
pub fn list(args: &ListArgs) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::json;
    let dir = match &args.dir {
        Some(dir) => std::path::absolute(dir)?,
        None => {
            snapshot::store::automatic_dir().ok_or("application data directory is unavailable")?
        }
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => Some(entries),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let now = u64::from(format::now_unix());
    let mut rows = Vec::new();
    for entry in entries.into_iter().flatten() {
        let entry = entry?;
        let path = entry.path();
        if !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("drive-"))
            || !path
                .extension()
                .is_some_and(|ext| ext == snapshot::EXTENSION)
        {
            continue;
        }
        let mut row = json!({"schema":"kosha.cache.entry.v1", "path":path.to_string_lossy(), "header_only":true});
        let metadata = std::fs::symlink_metadata(&path)?;
        row["record_bytes"] = json!(metadata.len());
        let meta = if entry.file_type()?.is_file() {
            snapshot::store::read_meta_at(&path).map_err(|error| error.to_string())
        } else {
            Err("not a regular cache file".into())
        };
        match meta {
            Ok(meta) => {
                let matches = snapshot::store::automatic_path_in(&dir, &meta.root).file_name()
                    == path.file_name();
                let incomplete = meta.cancelled || meta.depth_limited;
                row["root"] = json!(meta.root.to_string_lossy());
                row["taken_unix"] = json!(meta.taken);
                row["taken_ms"] = json!(meta.taken_ms);
                row["age_seconds"] = json!(now.saturating_sub(meta.taken));
                row["entries"] = json!(meta.entries);
                row["allocated_bytes"] = json!(meta.bytes_allocated);
                row["partial"] = json!(meta.partial);
                row["incomplete"] = json!(incomplete);
                row["status"] = json!(if !matches {
                    "root-mismatch"
                } else if incomplete {
                    "incomplete"
                } else {
                    "recorded"
                });
            }
            Err(error) => {
                row["status"] = json!("unreadable");
                row["error"] = json!(error);
            }
        }
        rows.push(row);
    }
    rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    match args.output.as_str() {
        "json" => sayln!(
            "{}",
            serde_json::to_string(
                &json!({"schema":"kosha.cache.list.v1", "directory":dir.to_string_lossy(), "header_only":true, "records":rows})
            )?
        ),
        "jsonl" => {
            for row in rows {
                sayln!("{}", serde_json::to_string(&row)?);
            }
        }
        _ => {
            sayln!("DRIVE CACHE  {}", format::printable(&dir.to_string_lossy()));
            sayln!(
                "Headers only; opening with --cache validates the full record. No drive is scanned."
            );
            if rows.is_empty() {
                sayln!("No cached drive records.");
            }
            for row in &rows {
                let root = row["root"].as_str().unwrap_or("unknown root");
                let status = row["status"].as_str().unwrap_or("unknown");
                sayln!("\n{}  [{}]", format::printable(root), status);
                if let Some(taken) = row["taken_unix"].as_u64() {
                    let seconds = row["age_seconds"].as_u64().unwrap_or(0);
                    let age = if seconds >= 86_400 {
                        format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3600)
                    } else if seconds >= 3600 {
                        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
                    } else {
                        format!("{}m {}s", seconds / 60, seconds % 60)
                    };
                    sayln!(
                        "  Taken: {} UTC; age: {}; entries: {}; partial: {}; incomplete: {}",
                        super::format_when(taken),
                        age,
                        row["entries"],
                        row["partial"],
                        row["incomplete"]
                    );
                }
                sayln!(
                    "  Record: {} ({} bytes)",
                    format::printable(row["path"].as_str().unwrap_or("")),
                    row["record_bytes"]
                );
                if let Some(error) = row["error"].as_str() {
                    sayln!("  Error: {}", format::printable(error));
                }
            }
            sayln!(
                "\n{} cached record(s). Explicit snapshots: kosha --snapshots",
                rows.len()
            );
        }
    }
    Ok(())
}

impl Cache {
    pub fn validate(&self) -> Result<(), String> {
        if self.dir.is_some() && !self.enabled {
            return Err(
                "--cache-dir requires --cache; ordinary scans never consult a cache".into(),
            );
        }
        Ok(())
    }

    pub fn open(
        &self,
        root: &Path,
    ) -> Result<(snapshot::Meta, SizeTree), Box<dyn std::error::Error>> {
        let absolute = std::path::absolute(root)?;
        let loaded = match &self.dir {
            Some(dir) => snapshot::store::open_automatic_in(dir, &absolute),
            None => snapshot::store::open_automatic(&absolute),
        };
        let (path, meta, tree) = loaded.map_err(|error| {
            format!(
                "could not use cached scan for {}: {error}; omit --cache for a fresh scan. The cache must contain a completed record for this exact root",
                format::printable(&absolute.to_string_lossy())
            )
        })?;
        eprintln!(
            "source: cache; taken_unix={}; age_seconds={}; partial={}; incomplete={}; file={}",
            meta.taken,
            u64::from(format::now_unix()).saturating_sub(meta.taken),
            tree.is_partial(),
            tree.is_incomplete(),
            format::printable(&path.to_string_lossy())
        );
        Ok((meta, tree))
    }
}
