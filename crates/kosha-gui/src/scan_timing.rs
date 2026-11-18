// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Throughput over thirty seconds. A prior completed scan supplies an estimated
//! total; a queue of differently sized directories cannot supply one honestly.

use kosha_core::Progress;
use std::collections::VecDeque;
use std::time::Duration;

#[derive(Default)]
pub struct ScanTiming {
    samples: VecDeque<(Duration, Progress)>,
    reference_entries: Option<u64>,
    last_advance: Duration,
}

pub struct Estimate {
    pub entries_per_second: f64,
    pub remaining: Option<Duration>,
}

impl ScanTiming {
    pub fn with_total(reference_entries: Option<u64>) -> Self {
        let mut timing = Self {
            reference_entries,
            ..Default::default()
        };
        timing.observe(
            Duration::ZERO,
            Progress {
                entries: 0,
                dirs_scanned: Some(0),
                dirs_pending: 0,
                depth: 0,
            },
        );
        timing
    }

    pub fn reference_total(&self) -> Option<u64> {
        self.reference_entries
    }

    pub fn set_reference_total(&mut self, entries: u64) {
        self.reference_entries.get_or_insert(entries);
    }

    pub fn observe(&mut self, elapsed: Duration, progress: Progress) {
        if self
            .samples
            .back()
            .is_none_or(|(_, last)| progress.entries > last.entries)
        {
            self.last_advance = elapsed;
        }
        self.samples.push_back((elapsed, progress));
        while self.samples.len() > 2
            && (self.samples.len() > 512
                || elapsed.saturating_sub(self.samples[1].0) >= Duration::from_secs(30))
        {
            self.samples.pop_front();
        }
    }

    pub fn estimate(&self) -> Option<Estimate> {
        let (start, first) = self.samples.front()?;
        let (end, last) = self.samples.back()?;
        let seconds = end.saturating_sub(*start).as_secs_f64();
        if seconds < 1.0 || last.entries < first.entries {
            return None;
        }
        let entries_per_second = (last.entries - first.entries) as f64 / seconds;
        let remaining = self
            .reference_entries
            .filter(|total| *total > last.entries)
            .filter(|_| {
                seconds >= 5.0
                    && entries_per_second > 0.0
                    && last.dirs_scanned.is_some()
                    && last.dirs_pending > 0
                    && end.saturating_sub(self.last_advance) < Duration::from_secs(5)
            })
            .map(|total| {
                Duration::from_secs_f64(
                    ((total - last.entries) as f64 / entries_per_second).min(365.0 * 86400.0),
                )
            });
        Some(Estimate {
            entries_per_second,
            remaining,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(entries: u64) -> Progress {
        Progress {
            entries,
            dirs_scanned: Some(entries / 10),
            dirs_pending: 20,
            depth: 1,
        }
    }

    #[test]
    fn estimate_uses_remaining_entries_and_a_longer_rate_window() {
        let mut timing = ScanTiming::with_total(Some(4000));
        timing.observe(Duration::from_secs(10), p(1000));
        assert_eq!(
            timing.estimate().unwrap().remaining,
            Some(Duration::from_secs(30))
        );
        timing.observe(Duration::from_secs(20), p(3000));
        let estimate = timing.estimate().unwrap();
        assert_eq!(
            estimate.entries_per_second, 150.0,
            "a burst does not erase earlier measurements"
        );
        assert!((estimate.remaining.unwrap().as_secs_f64() - 1000.0 / 150.0).abs() < 0.001);
    }

    #[test]
    fn unknown_totals_overruns_stalls_and_warmup_do_not_show_a_countdown() {
        let mut timing = ScanTiming::with_total(None);
        timing.observe(Duration::from_secs(10), p(1000));
        assert!(timing.estimate().unwrap().remaining.is_none());
        timing.set_reference_total(2000);
        assert!(timing.estimate().unwrap().remaining.is_some());
        timing.observe(Duration::from_secs(16), p(1000));
        assert!(
            timing.estimate().unwrap().remaining.is_none(),
            "stalled scan"
        );
        timing.observe(Duration::from_secs(20), p(2001));
        assert!(
            timing.estimate().unwrap().remaining.is_none(),
            "old total exceeded"
        );
        let mut warming = ScanTiming::with_total(Some(4000));
        warming.observe(Duration::from_secs(2), p(100));
        assert!(warming.estimate().unwrap().remaining.is_none());
    }

    #[test]
    fn old_samples_expire_and_record_strategies_do_not_use_tree_entry_totals() {
        let mut timing = ScanTiming::with_total(Some(10000));
        timing.observe(Duration::from_secs(30), p(3000));
        timing.observe(Duration::from_secs(60), p(9000));
        assert_eq!(timing.estimate().unwrap().entries_per_second, 200.0);
        timing.observe(
            Duration::from_secs(61),
            Progress {
                dirs_scanned: None,
                ..p(9100)
            },
        );
        assert!(timing.estimate().unwrap().remaining.is_none());
    }
}
