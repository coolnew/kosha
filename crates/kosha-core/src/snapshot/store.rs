// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Where snapshots are kept, and what is in there.
//!
//! One directory, flat, one file per snapshot. No index and no database: the
//! filenames sort into the order the snapshots were taken, and each one's
//! header is a few hundred bytes at the front of the file, so a listing is a
//! directory read and one small read per entry. An index would be a second
//! place for the truth to live and a first place for it to go stale.
//!
//! It is under [`crate::dirs::data_dir`] rather than the configuration
//! directory, on the same argument the audit log is: this is data, it is large,
//! and it is about the machine. Roaming a gigabyte of snapshots with a Windows
//! profile would be a surprising thing to do to somebody.

use std::fs;
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{EXTENSION, Meta, ReadError};
use crate::tree::SizeTree;

/// A snapshot on disk, as a listing sees it.
#[derive(Clone, Debug)]
pub struct Kept {
    pub path: PathBuf,
    pub meta: Meta,
    /// What it occupies. Shown in the window, because the one thing a person
    /// keeping snapshots of a full disk should never have to wonder is what
    /// the snapshots themselves are costing.
    pub bytes: u64,
}

/// The directory snapshots are kept in.
///
/// `None` only where the platform will not say where application data goes,
/// which is the same condition that already leaves the audit log nowhere to
/// live.
pub fn dir() -> Option<PathBuf> {
    Some(crate::dirs::data_dir()?.join("snapshots"))
}

/// Automatic drive records are deliberately outside the explicit snapshot
/// directory: explicit snapshot listings do not enumerate this cache. The CLI
/// has a separate --caches inventory for automatic drive records.
pub fn automatic_dir() -> Option<PathBuf> {
    Some(crate::dirs::data_dir()?.join("drive-cache"))
}

fn root_key(root: &Path) -> Vec<u8> {
    let wsl_root = cfg!(windows)
        .then(|| root.to_str().and_then(crate::wsl::distribution_root_name))
        .flatten()
        .map(crate::wsl::path_of);
    let root = wsl_root.as_deref().unwrap_or(root);
    let mut bytes = root.as_os_str().as_encoded_bytes().to_vec();
    if cfg!(windows) {
        bytes.make_ascii_lowercase();
        for byte in &mut bytes {
            if *byte == b'/' {
                *byte = b'\\';
            }
        }
    }
    while bytes.len() > 1
        && (bytes.last() == Some(&b'/') || (cfg!(windows) && bytes.last() == Some(&b'\\')))
    {
        bytes.pop();
    }
    bytes
}

pub fn same_root(a: &Path, b: &Path) -> bool {
    root_key(a) == root_key(b)
}

/// Stable, bounded filename, even for a long mounted-volume path. The header
/// root is checked when opening, so a hash collision cannot show another tree.
pub fn automatic_path_in(dir: &Path, root: &Path) -> PathBuf {
    let hash = root_key(root)
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    dir.join(format!("drive-{hash:016x}.{EXTENSION}"))
}

pub fn open_automatic(root: &Path) -> Result<(PathBuf, Meta, SizeTree), ReadError> {
    let dir = automatic_dir().ok_or_else(|| io::Error::other("application data is unavailable"))?;
    open_automatic_in(&dir, root)
}

pub fn open_automatic_in(dir: &Path, root: &Path) -> Result<(PathBuf, Meta, SizeTree), ReadError> {
    let path = automatic_path_in(dir, root);
    let (meta, tree) = open(&path)?;
    if !same_root(&meta.root, root) || tree.is_incomplete() || tree.root_unreadable() {
        return Err(io::Error::other("cached drive does not match this completed scan").into());
    }
    Ok((path, meta, tree))
}

pub fn save_automatic(tree: &SizeTree, taken: SystemTime) -> io::Result<PathBuf> {
    let dir = automatic_dir().ok_or_else(|| io::Error::other("application data is unavailable"))?;
    save_automatic_in(&dir, tree, taken)
}

/// The caller selects volume or WSL distribution roots. A cancelled or depth-limited scan
/// must not replace the last usable record. Read errors remain recorded in
/// the tree, just as in an explicit snapshot, and are visible when browsing.
pub fn save_automatic_in(dir: &Path, tree: &SizeTree, taken: SystemTime) -> io::Result<PathBuf> {
    if tree.is_incomplete() || tree.root_unreadable() {
        return Err(io::Error::other(
            "unfinished or unreadable scans cannot replace the drive cache",
        ));
    }
    let destination = automatic_path_in(dir, tree.root_path());
    let meta = Meta::of(tree, taken, "Automatic drive scan");
    // The desktop and CLI can complete or compress scans out of order.
    // Stage the completed record first, then serialize the timestamp check
    // and replacement across processes for the shared cache directory.
    let staged = save_with_meta_in(dir, tree, &meta, taken)?;
    let replaced = (|| {
        let lock = fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".write.lock"))?;
        lock.lock()?;
        if read_meta_at(&destination).is_ok_and(|previous| previous.at_ms() > meta.at_ms()) {
            return Ok(false);
        }
        fs::rename(&staged, &destination)?;
        Ok(true)
    })();
    if !matches!(replaced, Ok(true)) {
        let _ = fs::remove_file(&staged);
    }
    replaced.map(|_| destination)
}

/// A committed observation. Listing reads only each snapshot's header.
#[derive(Debug)]
pub struct HistoryEntry {
    pub record: Kept,
    pub usage: Option<crate::DiskUsage>,
    pub partial: bool,
}

const HISTORY_POINTS: usize = 8;
const HISTORY_BYTES: u64 = 512 * 1024 * 1024;

pub fn history_dir() -> Option<PathBuf> {
    Some(crate::dirs::data_dir()?.join("volume-history"))
}

fn history_series(dir: &Path, volume: &crate::volume_history::VolumeId) -> PathBuf {
    // The ID is built only from a platform prefix and validated hex/UUID
    // bytes. Replace the separator for a Windows-compatible component.
    dir.join(volume.as_str().replace(':', "-"))
}

/// A tracking generation. A scan started before tracking was switched off or
/// history was cleared must not repopulate the series after that operation.
#[derive(Debug, PartialEq, Eq)]
pub struct HistoryTracking(String);

pub fn history_tracking_in(
    dir: &Path,
    volume: &crate::volume_history::VolumeId,
) -> io::Result<Option<HistoryTracking>> {
    read_history_tracking(&history_series(dir, volume))
}

fn read_history_tracking(series: &Path) -> io::Result<Option<HistoryTracking>> {
    match fs::read_to_string(series.join("tracking")) {
        Ok(generation) => Ok(Some(HistoryTracking(generation))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_history_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn lock_history_series(series: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(series)?;
    let lock = fs::File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(series.join(".write.lock"))?;
    lock.lock()?;
    Ok(lock)
}

/// One scan at a time across the CLI and desktop processes. Keep this
/// handle alive from before scanning through the final observation write.
pub struct MonitoringLease {
    dir: PathBuf,
    _lock: fs::File,
}

/// A committed observation ID and its snapshot, in commit order even if the
/// system clock changed between observations.
pub struct MonitoringRecord {
    pub id: u64,
    pub snapshot: Kept,
}

pub fn monitoring_dir() -> Option<PathBuf> {
    Some(crate::dirs::data_dir()?.join("monitoring"))
}

impl MonitoringLease {
    pub(crate) fn directory(&self) -> &Path {
        &self.dir
    }
    pub fn try_acquire(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let lock = fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".scan.lock"))?;
        lock.try_lock().map_err(io::Error::from)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            _lock: lock,
        })
    }

    pub(crate) fn records(&self, profile: u64) -> io::Result<Vec<MonitoringRecord>> {
        list_monitoring_in(&self.dir, profile)
    }

    pub(crate) fn last_attempt(
        &self,
        profile: u64,
    ) -> io::Result<Option<crate::monitoring::LastAttempt>> {
        last_monitoring_attempt_in(&self.dir, profile)
    }

    pub(crate) fn write_attempt(
        &self,
        profile: u64,
        attempt: crate::monitoring::LastAttempt,
    ) -> io::Result<()> {
        use crate::monitoring::Attempt;
        use std::io::Write as _;

        let dir = monitoring_series(&self.dir, profile);
        fs::create_dir_all(&dir)?;
        let timestamp = attempt
            .time
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        let status = match attempt.status {
            Attempt::Scanning => "scanning",
            Attempt::Unavailable => "unavailable",
            Attempt::Incomplete => "incomplete",
            Attempt::Cancelled => "cancelled",
            Attempt::Completed => "completed",
        };
        crate::export::atomic_write(&dir.join("attempt"), |out| {
            writeln!(out, "v1 {timestamp} {status}")
        })
    }

    /// Save a verified sample to this profile's history. The newest two
    /// points always fit together or the new point is declined. Keep any
    /// referenced alert pair within the same budget.
    pub fn record(
        &mut self,
        profile: u64,
        sample: &crate::monitoring::Sample<'_>,
        taken: SystemTime,
        protected: &[u64],
    ) -> io::Result<MonitoringRecord> {
        self.record_tree(profile, sample.tree, taken, protected)
    }

    fn record_tree(
        &mut self,
        profile: u64,
        tree: &SizeTree,
        taken: SystemTime,
        protected: &[u64],
    ) -> io::Result<MonitoringRecord> {
        use std::io::Write as _;

        let series = monitoring_series(&self.dir, profile);
        let entries = list_monitoring_in(&self.dir, profile)?;
        let latest = entries.last();
        if latest.is_some_and(|entry| !same_root(&entry.snapshot.meta.root, tree.root_path())) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "monitoring root changed",
            ));
        }
        let id = latest
            .map_or(Some(1), |entry| entry.id.checked_add(1))
            .ok_or_else(|| io::Error::other("monitoring observation IDs exhausted"))?;
        let pinned = |entry: &MonitoringRecord| {
            latest.is_some_and(|latest| latest.id == entry.id) || protected.contains(&entry.id)
        };
        let mut count = 1 + entries.iter().filter(|entry| pinned(entry)).count();
        let mut bytes = entries
            .iter()
            .filter(|entry| pinned(entry))
            .fold(0u64, |total, entry| {
                total.saturating_add(entry.snapshot.bytes)
            });
        fs::create_dir_all(&series)?;
        let meta = Meta::of(tree, taken, "Storage monitoring");
        let staged = series.join(".pending");
        let destination = series.join(format!("{id:020}.kosha"));
        match fs::remove_file(&staged) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let write = (|| {
            let mut out = BufWriter::new(fs::File::create_new(&staged)?);
            super::write(tree, &meta, &mut out)?;
            out.flush()?;
            let file = out.into_inner()?;
            file.sync_all()?;
            let size = file.metadata()?.len();
            bytes = bytes.saturating_add(size);
            if count > HISTORY_POINTS || bytes > HISTORY_BYTES {
                return Err(io::Error::other(
                    "monitoring comparison exceeds the local history budget",
                ));
            }
            drop(file);
            for entry in entries.iter().rev().filter(|entry| !pinned(entry)) {
                if count < HISTORY_POINTS && entry.snapshot.bytes <= HISTORY_BYTES - bytes {
                    count += 1;
                    bytes += entry.snapshot.bytes;
                } else {
                    match fs::remove_file(&entry.snapshot.path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            fs::rename(&staged, &destination)?;
            Ok(size)
        })();
        if write.is_err() {
            let _ = fs::remove_file(&staged);
        }
        let size = write?;

        Ok(MonitoringRecord {
            id,
            snapshot: Kept {
                path: destination,
                meta,
                bytes: size,
            },
        })
    }
}

fn monitoring_series(dir: &Path, profile: u64) -> PathBuf {
    dir.join(format!("profile-{profile}"))
}

pub fn last_monitoring_attempt_in(
    dir: &Path,
    profile: u64,
) -> io::Result<Option<crate::monitoring::LastAttempt>> {
    use crate::monitoring::{Attempt, LastAttempt};
    use std::io::Read as _;
    use std::time::{Duration, UNIX_EPOCH};
    let file = match fs::File::open(monitoring_series(dir, profile).join("attempt")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut text = String::new();
    file.take(128).read_to_string(&mut text)?;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid monitoring attempt");
    let mut fields = text.split_ascii_whitespace();
    if fields.next() != Some("v1") {
        return Err(invalid());
    }
    let seconds = fields
        .next()
        .and_then(|field| field.parse().ok())
        .ok_or_else(invalid)?;
    let time = UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .ok_or_else(invalid)?;
    let status = match fields.next() {
        Some("scanning") => Attempt::Scanning,
        Some("unavailable") => Attempt::Unavailable,
        Some("incomplete") => Attempt::Incomplete,
        Some("cancelled") => Attempt::Cancelled,
        Some("completed") => Attempt::Completed,
        _ => return Err(invalid()),
    };
    if fields.next().is_some() || !text.ends_with('\n') {
        return Err(invalid());
    }
    Ok(Some(LastAttempt { time, status }))
}

pub fn list_monitoring_in(dir: &Path, profile: u64) -> io::Result<Vec<MonitoringRecord>> {
    let listing = match fs::read_dir(monitoring_series(dir, profile)) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut records = Vec::new();
    for entry in listing {
        let entry = entry?;
        let filename = entry.file_name();
        let Some(stem) = filename
            .to_str()
            .and_then(|name| name.strip_suffix(".kosha"))
        else {
            continue;
        };
        let Ok(id) = stem.parse::<u64>() else {
            continue;
        };
        if id == 0 || stem != format!("{id:020}") || !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let bytes = entry.metadata()?.len();
        let meta = read_meta_at(&path).map_err(io::Error::other)?;
        records.push(MonitoringRecord {
            id,
            snapshot: Kept { path, meta, bytes },
        });
    }
    records.sort_unstable_by_key(|record| record.id);
    Ok(records)
}

#[cfg(test)]
mod monitoring_tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn one_monitoring_lease_covers_all_profiles_until_drop() {
        let dir = tempfile::tempdir().unwrap();
        let lease = MonitoringLease::try_acquire(dir.path()).unwrap();
        assert!(MonitoringLease::try_acquire(dir.path()).is_err());
        drop(lease);
        assert!(MonitoringLease::try_acquire(dir.path()).is_ok());
    }

    #[test]
    fn monitoring_retention_preserves_alert_pair_and_ignores_clock_rollback() {
        let storage = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let tree = crate::scan(source.path()).unwrap();
        let series = monitoring_series(storage.path(), 1);
        let mut lease = MonitoringLease::try_acquire(storage.path()).unwrap();
        for id in 1..=12 {
            let taken = UNIX_EPOCH + Duration::from_secs(100 - id);
            let result = lease.record_tree(1, &tree, taken, &[1, 2]).unwrap();
            assert_eq!(result.id, id);
        }
        fs::write(series.join("unrelated.kosha"), b"leave this alone").unwrap();
        fs::write(series.join(".pending"), b"interrupted publication").unwrap();
        drop(lease);
        let records = list_monitoring_in(storage.path(), 1).unwrap();
        assert_eq!(
            records.iter().map(|record| record.id).collect::<Vec<_>>(),
            vec![1, 2, 7, 8, 9, 10, 11, 12]
        );
        assert_eq!(records.last().unwrap().snapshot.meta.taken, 88);
        let (_, restored) = open(&records[0].snapshot.path).unwrap();
        assert!(same_root(restored.root_path(), tree.root_path()));
        assert!(list_monitoring_in(storage.path(), 2).unwrap().is_empty());

        let mut lease = MonitoringLease::try_acquire(storage.path()).unwrap();
        let all_retained: Vec<_> = records.iter().map(|record| record.id).collect();
        assert!(
            lease
                .record_tree(1, &tree, UNIX_EPOCH, &all_retained)
                .is_err()
        );
        assert_eq!(list_monitoring_in(storage.path(), 1).unwrap().len(), 8);
        assert!(!series.join(".pending").exists());
        assert!(series.join("unrelated.kosha").is_file());

        let other = tempfile::tempdir().unwrap();
        let replacement = crate::scan(other.path()).unwrap();
        assert!(lease.record_tree(1, &replacement, UNIX_EPOCH, &[]).is_err());
    }
}

fn write_tracking_generation(series: &Path) -> io::Result<()> {
    use std::io::Write as _;
    let tick = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    crate::export::atomic_write(&series.join("tracking"), |out| {
        write!(out, "{}-{tick}", std::process::id())
    })
}

pub fn set_history_tracking_in(
    dir: &Path,
    volume: &crate::volume_history::VolumeId,
    enabled: bool,
) -> io::Result<()> {
    let series = history_series(dir, volume);
    let _lock = lock_history_series(&series)?;
    if enabled {
        if read_history_tracking(&series)?.is_none() {
            write_tracking_generation(&series)?;
        }
        Ok(())
    } else {
        remove_history_file(&series.join("tracking"))
    }
}

/// Remove only this volume's recorded observations and abandoned staging.
/// Keep its tracking preference, with a fresh generation for future scans.
pub fn clear_history_in(dir: &Path, volume: &crate::volume_history::VolumeId) -> io::Result<()> {
    let series = history_series(dir, volume);
    let _lock = lock_history_series(&series)?;
    if read_history_tracking(&series)?.is_some() {
        write_tracking_generation(&series)?;
    }
    for child in fs::read_dir(&series)? {
        let child = child?;
        if child.file_type()?.is_file() && history_filename(&child.file_name()).is_some() {
            remove_history_file(&child.path())?;
        }
    }
    remove_history_file(&series.join(".pending"))
}

pub fn list_history_in(
    dir: &Path,
    volume: &crate::volume_history::VolumeId,
) -> io::Result<Vec<HistoryEntry>> {
    let mut records = Vec::new();
    let series = history_series(dir, volume);
    let children = match fs::read_dir(series) {
        Ok(children) => children,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(records),
        Err(error) => return Err(error),
    };
    for child in children {
        let child = child?;
        if !child.file_type()?.is_file() {
            continue;
        }
        let Some((stamp, usage, partial)) = history_filename(&child.file_name()) else {
            continue;
        };
        let path = child.path();
        let Ok(meta) = read_meta_at(&path) else {
            continue;
        };
        if meta.at_ms() != stamp {
            continue;
        }
        let bytes = match child.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        records.push(HistoryEntry {
            record: Kept { bytes, meta, path },
            usage,
            partial,
        });
    }
    records.sort_unstable_by(|a, b| {
        (a.record.meta.at_ms(), &a.record.path).cmp(&(b.record.meta.at_ms(), &b.record.path))
    });
    Ok(records)
}

fn history_filename(name: &std::ffi::OsStr) -> Option<(u64, Option<crate::DiskUsage>, bool)> {
    let name = name.to_str()?.strip_prefix("v1-")?.strip_suffix(".kosha")?;
    let mut fields = name.split('-');
    let stamp = fields.next()?.parse().ok()?;
    let partial = match fields.next()? {
        "complete" => false,
        "partial" => true,
        _ => return None,
    };
    let usage = match fields.next()? {
        "unknown" => None,
        total => Some(crate::DiskUsage {
            total: total.parse().ok()?,
            free: fields.next()?.parse().ok()?,
            available: fields.next()?.parse().ok()?,
        }),
    };
    let _attempt: u32 = fields.next()?.parse().ok()?;
    fields.next().is_none().then_some((stamp, usage, partial))
}

/// Commit one observation and retain the newest points within the per-volume
/// limits. The newest point is kept even when its arena exceeds the byte cap.
pub fn save_history_in(
    dir: &Path,
    observation: &crate::volume_history::Observation<'_>,
) -> io::Result<()> {
    let series = history_series(dir, observation.volume());
    let _lock = lock_history_series(&series)?;
    save_history_locked(dir, &series, observation)
}

pub fn save_tracked_history_in(
    dir: &Path,
    observation: &crate::volume_history::Observation<'_>,
    generation: &HistoryTracking,
) -> io::Result<()> {
    let series = history_series(dir, observation.volume());
    let _lock = lock_history_series(&series)?;
    if read_history_tracking(&series)?.as_ref() == Some(generation) {
        save_history_locked(dir, &series, observation)?;
    }
    Ok(())
}

fn save_history_locked(
    dir: &Path,
    series: &Path,
    observation: &crate::volume_history::Observation<'_>,
) -> io::Result<()> {
    let tree = observation.tree();
    let meta = Meta::of(tree, observation.time(), "Drive history");
    let stamp = meta.at_ms();
    let coverage = if observation.partial() {
        "partial"
    } else {
        "complete"
    };
    let usage = observation.usage().map_or_else(
        || "unknown".to_owned(),
        |usage| format!("{}-{}-{}", usage.total, usage.free, usage.available),
    );
    let destination = (0..ATTEMPTS)
        .map(|index| series.join(format!("v1-{stamp}-{coverage}-{usage}-{index}.kosha")))
        .find(|path| !path.exists())
        .ok_or_else(|| io::Error::other("too many observations at the same time"))?;
    // The volume lock makes this a single staging name, including on retry
    // after an interrupted save. Readers ignore it until the final rename.
    let staged = series.join(".pending");
    match fs::remove_file(&staged) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let file = fs::File::create_new(&staged)?;
    commit_snapshot(file, tree, &meta, &staged, &destination)?;

    let mut bytes = 0u64;
    for (index, entry) in list_history_in(dir, observation.volume())?
        .into_iter()
        .rev()
        .enumerate()
    {
        bytes = bytes.saturating_add(entry.record.bytes);
        if index > 0 && (index >= HISTORY_POINTS || bytes > HISTORY_BYTES) {
            match fs::remove_file(entry.record.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

/// How many names to try before giving up.
///
/// Only reached by something taking snapshots in a loop inside one
/// millisecond. A bound rather than a `loop`, because a filesystem that
/// answers "this exists" to every name it is offered — a permission problem
/// misreported, a full directory — must end in an error and not in a spin.
const ATTEMPTS: u32 = 64;

/// Write `tree` as a new snapshot and return where it went.
///
/// Written to a temporary name and renamed into place, so a snapshot that fails
/// halfway — a full disk, most likely, which is the exact condition somebody
/// running this tool is in — never appears in the listing as a file that cannot
/// be opened.
///
/// **A snapshot never replaces one.** Taking two of the same folder in the same
/// second used to produce one file, because the name was built from whole
/// seconds: the second write landed on the first's name and the first was gone.
/// Milliseconds make that take a script rather than a double click, and
/// `create_new` below makes even a script fall to the next name instead of
/// overwriting — the name is chosen by asking the filesystem for it, which is
/// the only check that cannot lose a race with another kosha doing the same
/// thing.
pub fn save(tree: &SizeTree, label: &str, taken: SystemTime) -> io::Result<PathBuf> {
    let dir = dir().ok_or_else(|| {
        io::Error::other("this platform does not say where application data belongs")
    })?;
    save_in(&dir, tree, label, taken)
}

/// The same, into a directory the caller names.
///
/// Exists so the rule above can be tested. Where snapshots go is read from the
/// environment, and a test that moved the environment to check the rule would
/// be a test every other test in the process runs beside.
pub fn save_in(dir: &Path, tree: &SizeTree, label: &str, taken: SystemTime) -> io::Result<PathBuf> {
    let meta = Meta::of(tree, taken, label);
    save_with_meta_in(dir, tree, &meta, taken)
}

/// Keep a historical record as an explicit snapshot without dating it anew.
pub fn save_record(tree: &SizeTree, meta: &Meta) -> io::Result<PathBuf> {
    let dir = dir().ok_or_else(|| io::Error::other("application data is unavailable"))?;
    save_record_in(&dir, tree, meta)
}

/// Preserves the original header, including milliseconds, measurement options,
/// and tool provenance. Also works while the automatic file is still being saved.
pub fn save_record_in(dir: &Path, tree: &SizeTree, meta: &Meta) -> io::Result<PathBuf> {
    let taken = std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(meta.taken))
        .and_then(|time| {
            time.checked_add(std::time::Duration::from_millis(u64::from(meta.taken_ms)))
        })
        .ok_or_else(|| io::Error::other("the original snapshot date is out of range"))?;
    save_with_meta_in(dir, tree, meta, taken)
}

fn save_with_meta_in(
    dir: &Path,
    tree: &SizeTree,
    meta: &Meta,
    taken: SystemTime,
) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    for attempt in 0..ATTEMPTS {
        let final_path = dir.join(super::file_name_nth(tree.root_path(), taken, attempt));
        let temp_path = final_path.with_extension("writing");

        // `create_new` rather than `create`: it fails when the file is already
        // there, atomically, so two processes cannot both decide a name is
        // free. Whoever loses takes the next one.
        let file = match fs::File::create_new(&temp_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        // And the final name has to be free too: the temporary one being
        // available says nothing about a completed snapshot sitting there.
        if final_path.exists() {
            drop(file);
            let _ = fs::remove_file(&temp_path);
            continue;
        }

        commit_snapshot(file, tree, meta, &temp_path, &final_path)?;
        return Ok(final_path);
    }

    Err(io::Error::other(
        "could not find an unused name for this snapshot",
    ))
}

fn commit_snapshot(
    file: fs::File,
    tree: &SizeTree,
    meta: &Meta,
    staged: &Path,
    destination: &Path,
) -> io::Result<()> {
    let write = (|| {
        let mut out = BufWriter::new(file);
        super::write(tree, meta, &mut out)?;
        // Explicitly flush and sync before publishing the snapshot.
        out.into_inner()?.sync_all()
    })();
    let result = write.and_then(|()| fs::rename(staged, destination));
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}

/// A file in the store that is not a snapshot anybody can open.
///
/// Kept beside the good ones rather than dropped, because a snapshot that
/// vanishes from the listing is indistinguishable from one that was never
/// taken — and the difference matters to whoever went looking for it.
#[derive(Debug)]
pub struct Unreadable {
    pub path: PathBuf,
    pub why: ReadError,
}

/// What is in the store: the snapshots, and the files that are not.
#[derive(Debug, Default)]
pub struct Listing {
    /// Every snapshot kept, oldest first.
    pub kept: Vec<Kept>,
    /// Every file with the extension that could not be read, and why.
    pub unreadable: Vec<Unreadable>,
}

/// Every snapshot kept, oldest first.
///
/// Unreadable files are skipped rather than failing the listing: one damaged
/// snapshot must not be able to hide the nine good ones beside it. What they
/// are is [`list_all`]'s to report — this is for callers that only want the
/// list.
pub fn list() -> Vec<Kept> {
    list_all().kept
}

/// The same listing, with what it could not read.
///
/// **A damaged snapshot used to disappear.** The listing skipped it, both front
/// ends showed what was left, and the file was still sitting in the store
/// occupying its megabytes: `kosha --snapshots` said "1 snapshot" over a
/// directory holding four, and the window's rail said the same. The skipping is
/// right — one damaged file must not hide the nine good ones — and the note on
/// `list` said as much, adding that "the window is where somebody would be told
/// about it". Nothing carried the news that far, so nobody was told anywhere.
pub fn list_all() -> Listing {
    let Some(dir) = dir() else {
        return Listing::default();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Listing::default();
    };

    let mut listing = Listing::default();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == EXTENSION) {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match read_meta_at(&path) {
            Ok(meta) => listing.kept.push(Kept { path, meta, bytes }),
            Err(why) => listing.unreadable.push(Unreadable { path, why }),
        }
    }
    let kept = &mut listing.kept;

    // By the stamp inside rather than by the filename, which is only *usually*
    // the same: a snapshot copied in from another machine keeps its header and
    // may have been renamed by anything.
    //
    // To the millisecond, and then by name. Two snapshots the same age is not
    // a hypothetical — a folder that scans in under a second is a folder
    // somebody snapshots twice in a row — and a list whose order changes
    // between two readings of the same directory is one where the row a person
    // is about to click moves out from under them.
    kept.sort_by(|a, b| {
        a.meta
            .at_ms()
            .cmp(&b.meta.at_ms())
            .then_with(|| a.path.cmp(&b.path))
    });
    // The unreadable ones have no stamp to sort by — that is what is wrong with
    // them — so they go in name order, which is at least the same order twice.
    listing.unreadable.sort_by(|a, b| a.path.cmp(&b.path));
    listing
}

/// Read one snapshot's header without inflating its tree.
pub fn read_meta_at(path: &Path) -> Result<Meta, ReadError> {
    let file = fs::File::open(path)?;
    super::read_meta(BufReader::new(file))
}

/// Open a snapshot as a browsable tree.
pub fn open(path: &Path) -> Result<(Meta, SizeTree), ReadError> {
    let file = fs::File::open(path)?;
    // A large buffer because this is one long sequential read of a compressed
    // stream, and the default 8 KiB would be a syscall every few thousand
    // entries.
    super::read(BufReader::with_capacity(256 * 1024, file))
}

/// Delete one.
///
/// The one destructive thing in this module, and it is deliberately not
/// routed through `kosha-fsops`: that crate's grant exists to guard *the
/// user's* files, and a snapshot is kosha's own bookkeeping. Making it need
/// read-write mode would mean arming the window that can delete a photo
/// library in order to tidy up a list.
pub fn remove(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

/// What every snapshot occupies together.
pub fn total_bytes(kept: &[Kept]) -> u64 {
    kept.iter().map(|k| k.bytes).fold(0, u64::saturating_add)
}
