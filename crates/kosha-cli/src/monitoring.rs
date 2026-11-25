// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::num::NonZeroU64;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::SystemTime;

use kosha_core::monitoring::{self, Cadence, CheckResult, Location, Profile, Thresholds};
use kosha_core::snapshot::store::{self, MonitoringLease};
use kosha_core::{format, sayln};
use serde_json::{Value, json};

const HELP: &str = "\
kosha monitor list [--jsonl]
kosha monitor show ID [--jsonl]
kosha monitor add ROOT daily|weekly [--growth-bytes N] [--low-space-bytes N]
kosha monitor update ID daily|weekly [--growth-bytes N] [--low-space-bytes N]
kosha monitor pause ID
kosha monitor resume ID
kosha monitor remove ID
kosha monitor check [ID] [--jsonl]

Without ID, check runs one pass over profiles that are due, then exits.
With ID, it checks that profile immediately. Both respect Pause.
Add and update require at least one positive threshold in bytes.";

pub fn run(args: &[String]) -> ExitCode {
    if args.is_empty() || matches!(args, [arg] if arg == "--help" || arg == "-h") {
        sayln!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let Some(dir) = store::monitoring_dir() else {
        eprintln!("error: monitoring data directory is unavailable");
        return ExitCode::from(2);
    };
    match execute(&dir, args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn execute(dir: &Path, args: &[String]) -> Result<bool, Box<dyn std::error::Error>> {
    let (args, jsonl) = match args.split_last() {
        Some((last, rest)) if last == "--jsonl" => (rest, true),
        _ => (args, false),
    };
    match args {
        [command] if command == "list" => {
            for profile in monitoring::list_profiles(dir)? {
                print_profile(&profile, jsonl);
            }
        }
        [command, id] if command == "show" => {
            let id: u64 = id.parse()?;
            let profile = monitoring::list_profiles(dir)?
                .into_iter()
                .find(|profile| profile.id == id);
            let observations = store::list_monitoring_in(dir, id)?;
            if let Some(profile) = profile {
                print_profile(&profile, jsonl);
            } else if observations.is_empty() {
                return Err("monitoring profile not found".into());
            } else if jsonl {
                sayln!(
                    "{}",
                    json!({ "kind": "profile", "id": id, "status": "removed" })
                );
            } else {
                sayln!("{id} · removed · saved observations");
            }
            let alerts = monitoring::alerts(dir, id)?;
            for entry in observations {
                if jsonl {
                    sayln!(
                        "{}",
                        json!({ "kind": "observation", "id": entry.id, "path": entry.snapshot.path.to_string_lossy(),
                        "allocated_bytes": entry.snapshot.meta.bytes_allocated })
                    );
                } else {
                    sayln!(
                        "  observation {} · {} · {}",
                        entry.id,
                        format::bytes(entry.snapshot.meta.bytes_allocated),
                        format::printable(&entry.snapshot.path.to_string_lossy())
                    );
                }
            }
            if jsonl {
                sayln!(
                    "{}",
                    json!({ "kind": "alerts", "profile_id": id, "alerts": alerts })
                );
            } else {
                if let Some(growth) = alerts.growth {
                    sayln!(
                        "  growth {} · compare observations {} and {}",
                        format::bytes(growth.bytes),
                        growth.before,
                        growth.after
                    );
                }
                if let Some(observation) = alerts.low_space_observation {
                    sayln!("  low space · observation {observation}");
                }
            }
        }
        [command, target, cadence, flags @ ..]
            if matches!(command.as_str(), "add" | "update") && !jsonl =>
        {
            let (cadence, thresholds) = parse_rules(cadence, flags)?;
            let mut lease = MonitoringLease::try_acquire(dir)?;
            let profile = if command == "add" {
                let root = std::path::absolute(Path::new(target))?;
                let location =
                    Location::pin(&root).ok_or("expected an accessible local drive or folder")?;
                lease.add_profile(location, cadence, thresholds)?
            } else {
                let existing = find_profile(monitoring::list_profiles(dir)?, target)?;
                lease.update_profile(existing.id, cadence, thresholds, existing.paused)?
            };
            print_profile(&profile, false);
        }
        [command, id] if matches!(command.as_str(), "pause" | "resume") && !jsonl => {
            let mut lease = MonitoringLease::try_acquire(dir)?;
            let profile = find_profile(monitoring::list_profiles(dir)?, id)?;
            let updated = lease.update_profile(
                profile.id,
                profile.cadence,
                profile.thresholds,
                command == "pause",
            )?;
            print_profile(&updated, false);
        }
        [command, id] if command == "remove" && !jsonl => {
            let mut lease = MonitoringLease::try_acquire(dir)?;
            let profile = lease.remove_profile(id.parse()?)?;
            sayln!(
                "Removed monitoring profile {}. Saved observations were kept.",
                profile.id
            );
        }
        [command, target @ ..] if command == "check" && target.len() <= 1 => {
            let mut lease = MonitoringLease::try_acquire(dir)?;
            let profiles = monitoring::list_profiles(dir)?;
            let explicit = !target.is_empty();
            let profiles = if let Some(id) = target.first() {
                vec![find_profile(profiles, id)?]
            } else {
                profiles
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&cancelled);
            ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))?;
            let mut completed = true;
            for profile in profiles {
                let result = monitoring::check(
                    &mut lease,
                    &profile,
                    SystemTime::now(),
                    explicit,
                    Arc::clone(&cancelled),
                );
                let (status, details) = match result {
                    Ok(CheckResult::Paused) => ("paused", Value::Null),
                    Ok(CheckResult::NotDue) => ("not_due", Value::Null),
                    Ok(CheckResult::Skipped(attempt)) => {
                        completed = false;
                        (
                            "skipped",
                            json!({ "reason": format!("{attempt:?}").to_ascii_lowercase() }),
                        )
                    }
                    Ok(CheckResult::Observed {
                        previous_id,
                        record,
                        decision,
                    }) => (
                        "observed",
                        json!({
                            "observation_id": record.id, "previous_id": previous_id, "growth_bytes": decision.growth_alert_bytes,
                            "low_space_alert": decision.low_space_alert
                        }),
                    ),
                    Err(error) => {
                        completed = false;
                        ("error", json!({ "reason": error.to_string() }))
                    }
                };
                if jsonl {
                    sayln!(
                        "{}",
                        json!({ "kind": "check", "profile_id": profile.id, "status": status, "details": details })
                    );
                } else if details.is_null() {
                    sayln!("{} · {status}", profile.id);
                } else {
                    sayln!("{} · {status} · {details}", profile.id);
                }
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(false);
                }
            }
            return Ok(completed);
        }
        _ => return Err("see kosha monitor --help for supported commands".into()),
    }
    Ok(true)
}

fn find_profile(profiles: Vec<Profile>, id: &str) -> Result<Profile, Box<dyn std::error::Error>> {
    let id: u64 = id.parse()?;
    profiles
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| "monitoring profile not found".into())
}

fn parse_rules(
    cadence: &str,
    flags: &[String],
) -> Result<(Cadence, Thresholds), Box<dyn std::error::Error>> {
    let cadence = match cadence {
        "daily" => Cadence::Daily,
        "weekly" => Cadence::Weekly,
        _ => return Err("cadence must be daily or weekly".into()),
    };
    let (mut growth, mut low_space) = (None, None);
    for flag in flags.chunks(2) {
        let [name, value] = flag else {
            return Err("threshold requires a byte count".into());
        };
        let slot = match name.as_str() {
            "--growth-bytes" if growth.is_none() => &mut growth,
            "--low-space-bytes" if low_space.is_none() => &mut low_space,
            _ => return Err("unknown or repeated monitoring threshold".into()),
        };
        *slot = Some(value.parse::<NonZeroU64>()?);
    }
    let rules = match (growth, low_space) {
        (Some(growth), Some(low_space)) => Thresholds::Both { growth, low_space },
        (Some(growth), None) => Thresholds::Growth(growth),
        (None, Some(low)) => Thresholds::LowSpace(low),
        (None, None) => return Err("set at least one monitoring threshold".into()),
    };
    Ok((cadence, rules))
}

fn print_profile(profile: &Profile, jsonl: bool) {
    if jsonl {
        sayln!(
            "{}",
            json!({ "kind": "profile", "id": profile.id, "root": profile.location.root(),
            "cadence": profile.cadence, "thresholds": profile.thresholds, "paused": profile.paused })
        );
    } else {
        let cadence = match profile.cadence {
            Cadence::Daily => "daily",
            Cadence::Weekly => "weekly",
        };
        let status = if profile.paused { "paused" } else { "enabled" };
        sayln!(
            "{} · {} · {cadence} · {status}",
            profile.id,
            format::printable(&profile.location.root().to_string_lossy())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_profile_commands_preserve_the_saved_id_and_root() {
        let storage = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().to_str().unwrap();
        let run = |args: &[&str]| {
            execute(
                storage.path(),
                &args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>(),
            )
        };
        assert!(run(&["add", root, "daily", "--growth-bytes", "12"]).unwrap());
        assert!(run(&["pause", "1"]).unwrap());
        assert!(run(&["update", "1", "weekly", "--low-space-bytes", "20"]).unwrap());
        let profiles = monitoring::list_profiles(storage.path()).unwrap();
        let [profile] = profiles.as_slice() else {
            panic!("expected one profile")
        };
        assert_eq!(profile.id, 1);
        assert!(profile.location.is_current());
        assert_eq!(profile.cadence, Cadence::Weekly);
        assert!(profile.paused);
        assert!(run(&["resume", "1"]).unwrap());
        assert!(run(&["show", "1", "--jsonl"]).unwrap());
        assert!(run(&["update", "1", "weekly", "--low-space-bytes", "0"]).is_err());
        let mut lease = MonitoringLease::try_acquire(storage.path()).unwrap();
        let enabled = monitoring::list_profiles(storage.path()).unwrap().remove(0);
        assert!(matches!(
            monitoring::check(
                &mut lease,
                &enabled,
                SystemTime::now(),
                true,
                Arc::new(AtomicBool::new(false))
            )
            .unwrap(),
            CheckResult::Observed { .. }
        ));
        drop(lease);
        assert!(run(&["remove", "1"]).unwrap());
        assert!(run(&["remove", "1"]).is_err());
        assert!(
            monitoring::list_profiles(storage.path())
                .unwrap()
                .is_empty()
        );
        assert!(run(&["show", "1", "--jsonl"]).unwrap());
        assert!(run(&["add", root, "daily", "--growth-bytes", "12"]).unwrap());
        assert_eq!(monitoring::list_profiles(storage.path()).unwrap()[0].id, 2);
    }
}
