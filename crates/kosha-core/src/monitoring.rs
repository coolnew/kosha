// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Shared scheduling and alert decisions for verified local observations.

use std::io::{self, Read as _, Write as _};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime};

use crate::snapshot::store::{MonitoringLease, MonitoringRecord};
use crate::volume_history::VolumeId;
use crate::{SizeTree, Strategy, disk_usage, path, snapshot};

/// Identity pinned at setup, or at an explicit baseline reset. A folder
/// requires an identity within the volume. A drive root uses the volume ID.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Location {
    root: PathBuf,
    volume: VolumeId,
    directory: Option<u128>,
}

/// A complete scan of the pinned location and its optional free-space sample.
pub struct Sample<'a> {
    pub(crate) tree: &'a SizeTree,
    pub available: Option<u64>,
}

impl Location {
    pub fn pin(root: &Path) -> Option<Self> {
        if !root.is_absolute() {
            return None;
        }
        let root = path::simplify(root.canonicalize().ok()?);
        if !root.is_dir() {
            return None;
        }
        let (volume, mount) = VolumeId::containing(&root)?;
        let directory = if snapshot::store::same_root(&root, &path::simplify(mount)) {
            None
        } else {
            Some(directory_identity(&root)?)
        };
        Some(Self {
            root,
            volume,
            directory,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Check immediately before starting a scheduled scan.
    pub fn is_current(&self) -> bool {
        Self::pin(&self.root).as_ref() == Some(self)
    }

    /// Refuse results from changed locations, cancellation and incomplete
    /// coverage. Recheck the identity on both sides of the free-space query.
    pub fn sample<'a>(&self, tree: &'a SizeTree) -> Option<Sample<'a>> {
        if tree.stats().strategy != Strategy::DirectoryWalk
            || tree.stats().cross_filesystems
            || tree.is_incomplete()
            || tree.is_partial()
            || tree.root_unreadable()
            || tree.names_replaced() != 0
            || !snapshot::store::same_root(self.root(), tree.root_path())
            || !self.is_current()
        {
            return None;
        }
        let available = disk_usage(self.root())
            .filter(|usage| usage.total > 0 && usage.available <= usage.total)
            .map(|usage| usage.available);
        self.is_current().then_some(Sample { tree, available })
    }
}

#[cfg(unix)]
fn directory_identity(root: &Path) -> Option<u128> {
    use std::os::unix::fs::MetadataExt;
    let metadata = root.metadata().ok()?;
    metadata.is_dir().then(|| u128::from(metadata.ino()))
}

#[cfg(windows)]
fn directory_identity(root: &Path) -> Option<u128> {
    let directory = crate::platform::open_for_metadata(root)?;
    if !directory.metadata().ok()?.is_dir() {
        return None;
    }
    crate::platform::file_identity(&directory).map(|(_, id)| id)
}

#[cfg(not(any(unix, windows)))]
fn directory_identity(_: &Path) -> Option<u128> {
    None
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    #[default]
    Daily,
    Weekly,
}

impl Cadence {
    pub fn interval(self) -> Duration {
        const DAY: Duration = Duration::from_secs(86_400);
        match self {
            Self::Daily => DAY,
            Self::Weekly => DAY * 7,
        }
    }

    /// A missed interval makes one attempt due. A clock set back by a
    /// whole interval also makes one attempt due, so the old timestamp
    /// cannot postpone monitoring indefinitely.
    pub fn is_due(self, last_attempt: Option<SystemTime>, now: SystemTime) -> bool {
        last_attempt.is_none_or(|last| {
            let elapsed = now
                .duration_since(last)
                .unwrap_or_else(|error| error.duration());
            elapsed >= self.interval()
        })
    }
}

/// At least one alert rule and a nonzero byte threshold for each enabled rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Thresholds {
    Growth(NonZeroU64),
    LowSpace(NonZeroU64),
    Both {
        growth: NonZeroU64,
        low_space: NonZeroU64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// Newly observed allocated growth, if it meets the configured threshold.
    pub growth_alert_bytes: Option<u64>,
    /// True if a known free-space sample enters the low-space state.
    pub low_space_alert: bool,
    /// Preserve this state for the next successful check.
    pub low_space_active: bool,
}

/// Inputs shared by scheduled desktop and CLI scans. Configuration owners
/// retain the profile ID and pinned location across runs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub id: u64,
    pub location: Location,
    pub cadence: Cadence,
    pub thresholds: Thresholds,
    pub paused: bool,
}

const MAX_PROFILES: usize = 64;
const MAX_PROFILE_BYTES: usize = 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct Profiles {
    version: u8,
    next_id: u64,
    profiles: Vec<Profile>,
}

impl Profiles {
    fn load(dir: &Path) -> io::Result<Self> {
        let file = match std::fs::File::open(dir.join("profiles.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    version: 1,
                    next_id: 1,
                    profiles: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_PROFILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "monitoring profiles exceed the storage limit",
            ));
        }
        let saved: Self = serde_json::from_slice(&bytes)?;
        let mut previous = 0;
        if saved.version != 1
            || saved.next_id == 0
            || saved.profiles.len() > MAX_PROFILES
            || saved.profiles.iter().any(|profile| {
                let valid = profile.id > previous
                    && profile.id < saved.next_id
                    && profile.location.root().is_absolute();
                previous = profile.id;
                !valid
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid monitoring profiles",
            ));
        }
        Ok(saved)
    }

    fn save(&self, dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "monitoring profiles exceed the storage limit",
            ));
        }
        crate::export::atomic_write(&dir.join("profiles.json"), |out| out.write_all(&bytes))
    }
}

pub fn list_profiles(dir: &Path) -> io::Result<Vec<Profile>> {
    Ok(Profiles::load(dir)?.profiles)
}

impl MonitoringLease {
    pub fn add_profile(
        &mut self,
        location: Location,
        cadence: Cadence,
        thresholds: Thresholds,
    ) -> io::Result<Profile> {
        let dir = self.directory();
        let mut saved = Profiles::load(dir)?;
        if saved.profiles.len() >= MAX_PROFILES {
            return Err(io::Error::other("monitoring profile limit reached"));
        }
        if saved
            .profiles
            .iter()
            .any(|profile| snapshot::store::same_root(profile.location.root(), location.root()))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "location already has a monitoring profile",
            ));
        }
        let id = saved.next_id;
        saved.next_id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("monitoring profile IDs exhausted"))?;
        let profile = Profile {
            id,
            location,
            cadence,
            thresholds,
            paused: false,
        };
        saved.profiles.push(profile.clone());
        saved.save(dir)?;
        Ok(profile)
    }

    pub fn update_profile(
        &mut self,
        id: u64,
        cadence: Cadence,
        thresholds: Thresholds,
        paused: bool,
    ) -> io::Result<Profile> {
        let dir = self.directory();
        let mut saved = Profiles::load(dir)?;
        let profile = saved
            .profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "monitoring profile not found")
            })?;
        profile.cadence = cadence;
        profile.thresholds = thresholds;
        profile.paused = paused;
        let updated = profile.clone();
        saved.save(dir)?;
        Ok(updated)
    }

    /// Stop future checks while preserving the saved observations.
    pub fn remove_profile(&mut self, id: u64) -> io::Result<Profile> {
        let dir = self.directory();
        let mut saved = Profiles::load(dir)?;
        let index = saved
            .profiles
            .iter()
            .position(|profile| profile.id == id)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "monitoring profile not found")
            })?;
        let removed = saved.profiles.remove(index);
        saved.save(dir)?;
        Ok(removed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    Scanning,
    Unavailable,
    Incomplete,
    Cancelled,
    Completed,
}

pub struct LastAttempt {
    pub time: SystemTime,
    pub status: Attempt,
}

pub enum CheckResult {
    Paused,
    NotDue,
    Skipped(Attempt),
    Observed {
        previous_id: Option<u64>,
        record: Box<MonitoringRecord>,
        decision: Decision,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alerts {
    pub growth: Option<GrowthAlert>,
    pub low_space_observation: Option<u64>,
    pub low_space_active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GrowthAlert {
    pub before: u64,
    pub after: u64,
    pub bytes: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AlertUpdate {
    revision: u64,
    previous: Alerts,
    next: Alerts,
}

fn alert_path(dir: &Path, profile: u64) -> PathBuf {
    dir.join(format!("profile-{profile}")).join("alerts.json")
}

fn load_alerts(dir: &Path, profile: u64, latest: Option<u64>) -> io::Result<Alerts> {
    let file = match std::fs::File::open(alert_path(dir, profile)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Alerts::default()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "monitoring alert is too large",
        ));
    }
    let update: AlertUpdate = serde_json::from_slice(&bytes)?;
    Ok(if latest == Some(update.revision) {
        update.next
    } else {
        update.previous
    })
}

pub fn alerts(dir: &Path, profile: u64) -> io::Result<Alerts> {
    let latest = snapshot::store::list_monitoring_in(dir, profile)?
        .last()
        .map(|entry| entry.id);
    load_alerts(dir, profile, latest)
}

/// Check one profile under the shared lease. Scheduled attempts use the
/// stored timestamp. Explicit checks can skip cadence but still honor Pause.
pub fn check(
    lease: &mut MonitoringLease,
    profile: &Profile,
    now: SystemTime,
    explicit: bool,
    cancel: Arc<AtomicBool>,
) -> std::io::Result<CheckResult> {
    if profile.paused {
        return Ok(CheckResult::Paused);
    }
    let record_attempt = |lease: &MonitoringLease, status| {
        lease.write_attempt(profile.id, LastAttempt { time: now, status })
    };
    if !explicit
        && !profile.cadence.is_due(
            lease.last_attempt(profile.id)?.map(|attempt| attempt.time),
            now,
        )
    {
        return Ok(CheckResult::NotDue);
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(CheckResult::Skipped(Attempt::Cancelled));
    }
    if !profile.location.is_current() {
        record_attempt(lease, Attempt::Unavailable)?;
        return Ok(CheckResult::Skipped(Attempt::Unavailable));
    }
    record_attempt(lease, Attempt::Scanning)?;
    let worker = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .map_err(std::io::Error::other)?;
    let scanner =
        crate::Scanner::new(crate::ScanOptions::default()).with_cancel(Arc::clone(&cancel));
    let scanned = worker.install(|| scanner.scan(profile.location.root()));
    let sample = scanned
        .as_ref()
        .ok()
        .and_then(|tree| profile.location.sample(tree));
    let failure = if cancel.load(Ordering::Relaxed) {
        Some(Attempt::Cancelled)
    } else if sample.is_none() {
        Some(if profile.location.is_current() {
            Attempt::Incomplete
        } else {
            Attempt::Unavailable
        })
    } else {
        None
    };
    if let Some(status) = failure {
        record_attempt(lease, status)?;
        return Ok(CheckResult::Skipped(status));
    }
    let sample = sample.expect("successful monitoring scan has a verified sample");
    let previous = lease.records(profile.id)?.pop();
    let previous_id = previous.as_ref().map(|entry| entry.id);
    let revision = previous_id
        .map_or(Some(1), |id| id.checked_add(1))
        .ok_or_else(|| io::Error::other("monitoring observation IDs exhausted"))?;
    let saved = load_alerts(lease.directory(), profile.id, previous_id)?;
    let decision = profile.thresholds.evaluate(
        previous
            .as_ref()
            .map(|entry| entry.snapshot.meta.bytes_allocated),
        sample.tree.total_allocated(),
        sample.available,
        saved.low_space_active,
    );
    let mut next = saved.clone();
    if let (Some(before), Some(bytes)) = (previous_id, decision.growth_alert_bytes) {
        next.growth = Some(GrowthAlert {
            before,
            after: revision,
            bytes,
        });
    }
    next.low_space_active = decision.low_space_active;
    if decision.low_space_alert {
        next.low_space_observation = Some(revision);
    } else if !decision.low_space_active {
        next.low_space_observation = None;
    }
    let protected: Vec<u64> = [&saved, &next]
        .into_iter()
        .flat_map(|state| {
            state
                .growth
                .iter()
                .flat_map(|alert| [alert.before, alert.after])
                .chain(state.low_space_observation)
        })
        .collect();
    // The next alert state becomes visible only with the committed snapshot.
    // A failed snapshot write leaves the preceding alert state effective.
    let update = AlertUpdate {
        revision,
        previous: saved,
        next,
    };
    let path = alert_path(lease.directory(), profile.id);
    crate::export::atomic_write(&path, |out| {
        serde_json::to_writer(out, &update).map_err(io::Error::from)
    })?;
    let record = lease.record(profile.id, &sample, now, &protected)?;
    record_attempt(lease, Attempt::Completed)?;
    Ok(CheckResult::Observed {
        previous_id: previous.map(|entry| entry.id),
        record: Box::new(record),
        decision,
    })
}

impl Thresholds {
    /// Compare with the previous retained observation of the same location.
    /// The caller verifies scan coverage and identity before supplying either
    /// observation. A missing free-space counter leaves the low-space latch
    /// alone, so a failed measurement cannot rearm the alert.
    pub fn evaluate(
        self,
        previous_allocated: Option<u64>,
        allocated: u64,
        available: Option<u64>,
        was_low: bool,
    ) -> Decision {
        let (growth, low_space) = match self {
            Self::Growth(growth) => (Some(growth), None),
            Self::LowSpace(low_space) => (None, Some(low_space)),
            Self::Both { growth, low_space } => (Some(growth), Some(low_space)),
        };
        let growth_alert_bytes = previous_allocated
            .and_then(|previous| allocated.checked_sub(previous))
            .filter(|delta| growth.is_some_and(|limit| *delta >= limit.get()));
        let low_space_active =
            low_space.is_some_and(|limit| available.map_or(was_low, |free| free < limit.get()));
        Decision {
            growth_alert_bytes,
            low_space_alert: !was_low && low_space_active,
            low_space_active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: NonZeroU64 = NonZeroU64::new(10).unwrap();

    #[test]
    fn profile_reopens_with_same_identity_and_updates_in_place() {
        let storage = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let location = Location::pin(root.path()).unwrap();
        let mut lease = MonitoringLease::try_acquire(storage.path()).unwrap();
        let profile = lease
            .add_profile(location.clone(), Cadence::Daily, Thresholds::Growth(LIMIT))
            .unwrap();
        assert_eq!(list_profiles(storage.path()).unwrap(), [profile.clone()]);
        assert_eq!(
            lease
                .add_profile(location, Cadence::Daily, Thresholds::Growth(LIMIT))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(lease);
        let mut lease = MonitoringLease::try_acquire(storage.path()).unwrap();
        let changed = lease
            .update_profile(
                profile.id,
                Cadence::Weekly,
                Thresholds::LowSpace(LIMIT),
                true,
            )
            .unwrap();
        assert_eq!(changed.location, profile.location);
        assert_eq!(list_profiles(storage.path()).unwrap(), [changed]);
    }

    #[test]
    fn cadence_handles_first_run_missed_intervals_and_clock_rollback() {
        let reference = SystemTime::UNIX_EPOCH + Duration::from_secs(900_000);
        let one_day = Cadence::Daily.interval();
        assert!(Cadence::Daily.is_due(None, reference));
        assert!(!Cadence::Daily.is_due(Some(reference), reference));
        assert!(!Cadence::Daily.is_due(Some(reference), reference - Duration::from_secs(5)));
        assert!(Cadence::Daily.is_due(Some(reference), reference - one_day));
        assert!(Cadence::Daily.is_due(Some(reference), reference + one_day));
        assert!(!Cadence::Weekly.is_due(Some(reference), reference + one_day));
        assert!(Cadence::Weekly.is_due(Some(reference), reference + one_day * 20));
    }

    #[test]
    fn growth_requires_a_baseline_and_includes_the_boundary() {
        let rules = Thresholds::Growth(LIMIT);
        for (previous, current, expected) in [
            (None, 120, None),
            (Some(120), 100, None),
            (Some(120), 129, None),
            (Some(120), 130, Some(10)),
            (Some(120), u64::MAX, Some(u64::MAX - 120)),
        ] {
            assert_eq!(
                rules
                    .evaluate(previous, current, None, false)
                    .growth_alert_bytes,
                expected
            );
        }
    }

    #[test]
    fn low_space_alert_rearms_only_after_observed_recovery() {
        let rules = Thresholds::LowSpace(LIMIT);
        let first = rules.evaluate(None, 120, Some(9), false);
        assert!(first.low_space_alert && first.low_space_active);

        let missing = rules.evaluate(Some(120), 120, None, first.low_space_active);
        assert!(!missing.low_space_alert && missing.low_space_active);
        let still_low = rules.evaluate(Some(120), 120, Some(0), missing.low_space_active);
        assert!(!still_low.low_space_alert && still_low.low_space_active);

        let recovered = rules.evaluate(Some(120), 120, Some(10), still_low.low_space_active);
        assert!(!recovered.low_space_alert && !recovered.low_space_active);
        assert!(
            rules
                .evaluate(Some(120), 120, Some(9), recovered.low_space_active)
                .low_space_alert
        );
    }

    #[test]
    fn both_rules_can_alert_in_the_same_observation() {
        let rules = Thresholds::Both {
            growth: LIMIT,
            low_space: LIMIT,
        };
        assert_eq!(
            rules.evaluate(Some(120), 132, Some(8), false),
            Decision {
                growth_alert_bytes: Some(12),
                low_space_alert: true,
                low_space_active: true
            }
        );
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn observations_reject_replaced_directories_and_cancelled_scans() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("monitored");
        std::fs::create_dir(&folder).unwrap();
        let location = Location::pin(&folder).expect("local fixture on an identified filesystem");
        let mut tree = crate::scan(location.root()).unwrap();
        assert!(location.sample(&tree).is_some());
        tree.mark_cancelled();
        assert!(location.sample(&tree).is_none());

        // Keep the original alive so the filesystem cannot reuse its ID.
        std::fs::rename(&folder, fixture.path().join("previous")).unwrap();
        std::fs::create_dir(&folder).unwrap();
        assert!(!location.is_current());
        let replacement = crate::scan(location.root()).unwrap();
        assert!(location.sample(&replacement).is_none());
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn runner_records_growth_and_preserves_baseline_when_unavailable() {
        let fixture = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("monitored");
        std::fs::create_dir(&folder).unwrap();
        let profile = Profile {
            id: 7,
            location: Location::pin(&folder).unwrap(),
            cadence: Cadence::Daily,
            thresholds: Thresholds::Growth(LIMIT),
            paused: false,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let mut lease = MonitoringLease::try_acquire(storage.path()).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let mut run = |time, explicit| {
            check(&mut lease, &profile, time, explicit, Arc::clone(&cancel)).unwrap()
        };

        assert!(matches!(
            run(now, false),
            CheckResult::Observed {
                previous_id: None,
                ..
            }
        ));
        assert!(matches!(run(now, false), CheckResult::NotDue));
        std::fs::write(folder.join("growth.bin"), vec![42; 64 * 1024]).unwrap();
        let CheckResult::Observed {
            previous_id,
            record,
            decision,
        } = run(now + Duration::from_secs(1), true)
        else {
            panic!("expected a second observation")
        };
        assert_eq!(previous_id, Some(1));
        assert_eq!(record.id, 2);
        assert!(
            decision
                .growth_alert_bytes
                .is_some_and(|delta| delta >= 64 * 1024)
        );
        let growth = alerts(storage.path(), 7).unwrap().growth.unwrap();
        assert_eq!((growth.before, growth.after), (1, 2));
        assert_eq!(Some(growth.bytes), decision.growth_alert_bytes);

        std::fs::rename(&folder, fixture.path().join("unmounted")).unwrap();
        let later = now + Cadence::Daily.interval() + Duration::from_secs(1);
        assert!(matches!(
            run(later, false),
            CheckResult::Skipped(Attempt::Unavailable)
        ));
        assert!(matches!(run(later, false), CheckResult::NotDue));
        assert_eq!(
            crate::snapshot::store::last_monitoring_attempt_in(storage.path(), 7)
                .unwrap()
                .unwrap()
                .status,
            Attempt::Unavailable
        );
        assert_eq!(lease.records(7).unwrap().last().unwrap().id, 2);
    }

    #[test]
    fn alert_update_is_visible_only_with_its_observation() {
        let storage = tempfile::tempdir().unwrap();
        let path = alert_path(storage.path(), 1);
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        let previous = Alerts::default();
        let next = Alerts {
            low_space_observation: Some(2),
            low_space_active: true,
            ..Alerts::default()
        };
        let staged = AlertUpdate {
            revision: 2,
            previous: previous.clone(),
            next: next.clone(),
        };
        std::fs::write(path, serde_json::to_vec(&staged).unwrap()).unwrap();
        assert_eq!(load_alerts(storage.path(), 1, Some(1)).unwrap(), previous);
        assert_eq!(load_alerts(storage.path(), 1, Some(2)).unwrap(), next);
    }
}
