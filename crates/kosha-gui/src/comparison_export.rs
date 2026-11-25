// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Portable reports of the comparison on screen, not a new scan or tree export.

use crate::state::Comparison;
use kosha_core::compare::Kind;
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Jsonl,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Jsonl => "jsonl",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Jsonl => "JSONL",
        }
    }
}

pub fn kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Added => "added",
        Kind::Removed => "removed",
        Kind::Grew => "grew",
        Kind::Shrank => "shrank",
    }
}

pub(crate) fn text(value: &str) -> String {
    kosha_core::format::printable(value)
        .chars()
        .map(|ch| match ch {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '|' => "&#124;".into(),
            '`' => "&#96;".into(),
            '\\' => "&#92;".into(),
            '[' => "&#91;".into(),
            ']' => "&#93;".into(),
            '*' => "&#42;".into(),
            '_' => "&#95;".into(),
            ch => ch.to_string(),
        })
        .collect()
}

pub fn write(comparison: &Comparison, format: Format, mut out: impl Write) -> io::Result<usize> {
    let metadata = serde_json::json!({
        "type": "comparison", "schema_version": 1,
        "baseline": {"root": comparison.against_root.to_string_lossy(), "label": comparison.against.as_ref(), "taken_unix_ms": comparison.against_taken_ms},
        "newer": {"root": comparison.now_root.to_string_lossy(), "label": comparison.now.as_ref(), "taken_unix_ms": comparison.now_taken_ms},
        "scope": "visible_changes", "rows": comparison.visible(), "total_changes_above_threshold": comparison.changes.len(),
        "minimum_delta_bytes": comparison.floor, "changes_below_threshold": comparison.below_floor,
        "net_file_delta_bytes": comparison.net, "net_scope": "all_file_changes_including_below_threshold",
        "filters": {"directories": comparison.show_dirs, "files": comparison.show_files, "change_kind": comparison.change_kind.map(kind)},
        "warnings": comparison.footing.as_ref().map(|s| s.as_ref()),
        "comparison_duration_ms": comparison.elapsed.as_millis(), "rescanned": false,
    });
    if format == Format::Jsonl {
        serde_json::to_writer(&mut out, &metadata).map_err(io::Error::other)?;
        writeln!(out)?;
        for change in comparison.rows() {
            let row = serde_json::json!({"type": "change", "path": change.path,
                "entry_type": if change.is_dir { "directory" } else { "file" },
                "kind": kind(change.kind), "before_bytes": change.before, "after_bytes": change.after,
                "delta_bytes": change.delta()});
            serde_json::to_writer(&mut out, &row).map_err(io::Error::other)?;
            writeln!(out)?;
        }
    } else {
        writeln!(out, "# Disk usage comparison\n")?;
        for (label, root, date, stamp) in [
            (
                "Before",
                &comparison.against_root,
                &comparison.against,
                comparison.against_taken_ms,
            ),
            (
                "After",
                &comparison.now_root,
                &comparison.now,
                comparison.now_taken_ms,
            ),
        ] {
            writeln!(
                out,
                "- **{label}:** {} — {}",
                text(&root.to_string_lossy()),
                text(date)
            )?;
            writeln!(
                out,
                "  - Observation timestamp (Unix milliseconds): {}",
                stamp.map_or_else(
                    || "current in-memory scan; timestamp not recorded".into(),
                    |s| s.to_string()
                )
            )?;
        }
        writeln!(
            out,
            "\nNo new filesystem scan was run for this comparison.\n"
        )?;
        writeln!(
            out,
            "Net file change: **{:+} bytes** across all file changes, including those below the threshold. Directory totals are not added to this number.\n",
            comparison.net
        )?;
        writeln!(
            out,
            "Showing {} of {} changes above a {}-byte threshold; {} changes are below it.",
            comparison.visible(),
            comparison.changes.len(),
            comparison.floor,
            comparison.below_floor
        )?;
        writeln!(
            out,
            "Filters: directories={}, files={}, change kind={}.\n",
            comparison.show_dirs,
            comparison.show_files,
            comparison.change_kind.map(kind).unwrap_or("all")
        )?;
        if let Some(warning) = &comparison.footing {
            writeln!(out, "**Comparison limitations:** {}\n", text(warning))?;
        }
        for (directories, heading) in [
            (true, "Directories — where space changed"),
            (false, "Files — individual changes"),
        ] {
            writeln!(
                out,
                "## {heading}\n\n| Path | Change | Before (bytes) | After (bytes) | Delta (bytes) |\n| --- | --- | ---: | ---: | ---: |"
            )?;
            let mut count = 0;
            for change in comparison.rows().filter(|c| c.is_dir == directories) {
                writeln!(
                    out,
                    "| {} | {} | {} | {} | {:+} |",
                    text(&change.path),
                    kind(change.kind),
                    change.before,
                    change.after,
                    change.delta()
                )?;
                count += 1;
            }
            if count == 0 {
                writeln!(out, "\nNo changes shown for this group.")?;
            }
            writeln!(out)?;
        }
    }
    Ok(comparison.visible())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kosha_core::compare::Change;
    use std::sync::Arc;

    fn model() -> Comparison {
        let mut model = Comparison {
            against: "old".into(),
            now: "saved".into(),
            against_root: "C:/".into(),
            now_root: "C:/".into(),
            against_taken_ms: Some(1001),
            now_taken_ms: Some(2002),
            footing: Some("Access was partial".into()),
            show_dirs: true,
            show_files: true,
            net: 4,
            changes: Arc::new(vec![
                Change {
                    path: "dir".into(),
                    kind: Kind::Grew,
                    is_dir: true,
                    before: 1,
                    after: 5,
                },
                Change {
                    path: "<script>|two\nlines".into(),
                    kind: Kind::Grew,
                    is_dir: false,
                    before: 1,
                    after: 5,
                },
            ]),
            ..Default::default()
        };
        model.rebuild_visible();
        model
    }

    #[test]
    fn jsonl_preserves_paths_dates_warnings_and_frozen_filters() {
        let mut model = model();
        model.show_dirs = false;
        model.rebuild_visible();
        let frozen = model.clone();
        model.change_kind = Some(Kind::Removed);
        model.rebuild_visible();
        let mut bytes = Vec::new();
        assert_eq!(write(&frozen, Format::Jsonl, &mut bytes).unwrap(), 1);
        let rows: Vec<serde_json::Value> = String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["baseline"]["taken_unix_ms"], 1001);
        assert_eq!(rows[0]["newer"]["taken_unix_ms"], 2002);
        assert_eq!(rows[0]["warnings"], "Access was partial");
        assert_eq!(rows[0]["rescanned"], false);
        assert_eq!(rows[0]["net_file_delta_bytes"], 4);
        assert_eq!(rows[1]["path"], "<script>|two\nlines");
        assert_eq!(rows[1]["delta_bytes"], 4);
    }

    #[test]
    fn markdown_escapes_paths_and_separates_overlapping_directory_totals() {
        let mut bytes = Vec::new();
        assert_eq!(write(&model(), Format::Markdown, &mut bytes).unwrap(), 2);
        let report = String::from_utf8(bytes).unwrap();
        assert!(report.contains("&lt;script&gt;&#124;two?lines"));
        assert!(!report.contains("<script>"));
        assert!(report.contains("## Directories"));
        assert!(report.contains("## Files"));
        assert!(report.contains("**+4 bytes**"));
        assert!(report.contains("Access was partial"));
    }
}
