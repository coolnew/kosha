// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Explicit, read-only access to the same automatic records used by the GUI.

use kosha_core::{SizeTree, format, snapshot};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Cache {
    pub enabled: bool,
    pub dir: Option<PathBuf>,
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
