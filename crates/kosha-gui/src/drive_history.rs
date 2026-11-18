// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The last finished drive scan remains browsable while persistence catches up.

use kosha_core::{
    SizeTree,
    snapshot::{Meta, store},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

#[derive(Clone)]
pub struct DriveRecord {
    pub tree: Arc<SizeTree>,
    pub meta: Meta,
    pub path: PathBuf,
}

/// These arenas are already held by background save tasks. Keep an additional
/// Arc until each write ends so selecting a faster drive cannot hide a pending
/// record for another drive. Successful writes then use the disk cache.
#[derive(Default)]
pub struct PendingRecords(Vec<DriveRecord>);

impl PendingRecords {
    pub fn remember(&mut self, record: DriveRecord) {
        self.0.retain(|old| !old.matches(&record.meta.root));
        self.0.push(record);
    }

    pub fn get(&self, root: &Path) -> Option<&DriveRecord> {
        self.0.iter().find(|record| record.matches(root))
    }

    pub fn finished(&mut self, root: &Path, stamp: u64) {
        self.0
            .retain(|record| !record.matches(root) || record.meta.at_ms() != stamp);
    }
}

impl DriveRecord {
    pub fn new(tree: Arc<SizeTree>, taken: SystemTime) -> Option<Self> {
        if tree.is_incomplete() {
            return None;
        }
        let path = store::automatic_path_in(&store::automatic_dir()?, tree.root_path());
        let meta = Meta::of(&tree, taken, "Automatic drive scan");
        Some(Self { tree, meta, path })
    }

    pub fn matches(&self, root: &Path) -> bool {
        store::same_root(self.tree.root_path(), root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kosha_core::{ScanOptions, Scanner};

    #[test]
    fn a_finished_scan_is_available_before_any_file_is_written() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("one"), b"one").unwrap();
        let tree = Arc::new(
            Scanner::new(ScanOptions::default())
                .scan(source.path())
                .unwrap(),
        );
        let record = DriveRecord::new(tree.clone(), SystemTime::now()).unwrap();
        assert!(record.matches(tree.root_path()));
        assert!(Arc::ptr_eq(&tree, &record.tree), "no duplicate arena");
        let mut interrupted = Scanner::new(ScanOptions::default())
            .scan(source.path())
            .unwrap();
        interrupted.mark_cancelled();
        assert!(DriveRecord::new(Arc::new(interrupted), SystemTime::now()).is_none());
    }

    #[test]
    fn switching_drives_preserves_pending_records_and_newer_replacements() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let record = |root: &Path, seconds| {
            let tree = Arc::new(Scanner::new(ScanOptions::default()).scan(root).unwrap());
            DriveRecord::new(
                tree,
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
            )
            .unwrap()
        };
        let a = record(first.path(), 10);
        let b = record(second.path(), 20);
        let mut pending = PendingRecords::default();
        pending.remember(a.clone());
        pending.remember(b);
        assert!(
            pending.get(&a.meta.root).is_some(),
            "returning to the first drive while its save runs"
        );
        let newer_a = record(first.path(), 30);
        pending.remember(newer_a.clone());
        pending.finished(&a.meta.root, a.meta.at_ms());
        assert_eq!(
            pending.get(&a.meta.root).unwrap().meta.at_ms(),
            newer_a.meta.at_ms()
        );
        pending.finished(&a.meta.root, newer_a.meta.at_ms());
        assert!(pending.get(&a.meta.root).is_none());
    }
}
