// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Turning a date somebody wrote on a disk into seconds since 1970.
//!
//! **Only the filesystems that store a broken-down date need this**, and there
//! are two of them: FAT packs a year, a month and a day into sixteen bits, and
//! ISO 9660 writes seven bytes that mean exactly what they look like. ext, XFS
//! and NTFS all keep a count from an epoch, which is arithmetic rather than a
//! calendar — NTFS' is 1601 and one subtraction away.
//!
//! It lives here rather than in either reader because the second one to need
//! it is the point at which a copy becomes a place for the two to disagree.
//!
//! By that same argument the calendar arithmetic itself is not here:
//! `days_from_civil` is [`crate::format`]'s, beside its inverse, where the
//! exporter and the query language also reach it. There were three copies of it
//! in this crate and its dependents, which is three places for one conversion to
//! be wrong in.
//!
//! The leap-year rules went the same way, and for a reason this module argues
//! itself: **a date is validated rather than trusted.** The query language reads
//! its dates from a person rather than off a disk and had its own, weaker check —
//! `1..=31`, which accepts the 30th of February — so the validation now lives
//! beside the conversion it exists to protect, and both callers use it.
//!
//! **A date is validated rather than trusted.** These bytes come off a disk
//! somebody else wrote, and a month of 13 or a 31st of February is either
//! damage or a formatter that did not care. Computing an instant from one
//! produces a date in the wrong month, which looks like a fact; refusing it
//! produces no date at all, which is what is actually known.

/// Seconds since 1970 for a date and time taken as UTC.
///
/// `None` where the date is not one — a month outside 1 to 12, a day past the
/// end of that month, an hour past 23 — or where it falls outside what a `u32`
/// of seconds can hold, which is anything before 1970 or after 2106.
pub(crate) fn unix_seconds(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<u32> {
    if !crate::format::is_a_date(year, month, day) {
        return None;
    }
    // A leap second is written as 60 by the clocks that know about them, and
    // neither of these formats does; 60 here is damage.
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = crate::format::days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour * 3600 + minute * 60 + second))?;
    u32::try_from(seconds).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants_are_the_seconds_they_are() {
        // The epoch itself, the instant every other answer is relative to.
        assert_eq!(unix_seconds(1970, 1, 1, 0, 0, 0), Some(0));
        // A date taken off a real FAT volume while this was written.
        assert_eq!(unix_seconds(2024, 5, 17, 14, 30, 20), Some(1_715_956_220));
        // The last second a 32-bit count can hold, and the first it cannot.
        assert_eq!(
            unix_seconds(2106, 2, 7, 6, 28, 15),
            Some(u32::MAX),
            "the end of what a u32 of seconds reaches"
        );
        assert_eq!(unix_seconds(2106, 2, 7, 6, 28, 16), None);
    }

    /// The century rule, which is right for ninety-six years in a hundred
    /// without being written at all.
    ///
    /// The rule itself is `format`'s now, with its own test: it is needed by the
    /// dates a person types as well as the ones a volume holds. What this asserts
    /// is that *this* function applies it — a date on a disk is validated rather
    /// than trusted, which is the whole argument of the module above.
    #[test]
    fn the_leap_year_rules_are_all_three_of_them() {
        // 2000-02-29 exists and 1900-02-29 does not, and the difference
        // between them is the whole of the rule.
        assert!(unix_seconds(2000, 2, 29, 0, 0, 0).is_some());
        assert!(unix_seconds(1900, 2, 29, 0, 0, 0).is_none());
        // A day either side of the leap day, in a year that has one.
        assert_eq!(
            unix_seconds(2024, 3, 1, 0, 0, 0).unwrap()
                - unix_seconds(2024, 2, 28, 0, 0, 0).unwrap(),
            2 * 86_400,
            "a leap year has a day between the 28th of February and the 1st of March"
        );
    }

    #[test]
    fn a_date_that_is_not_one_has_no_instant() {
        for (what, year, month, day, hour, minute, second) in [
            ("month zero", 2024, 0, 1, 0, 0, 0),
            ("month thirteen", 2024, 13, 1, 0, 0, 0),
            ("day zero", 2024, 1, 0, 0, 0, 0),
            ("the 31st of April", 2024, 4, 31, 0, 0, 0),
            (
                "the 29th of February in a common year",
                2023,
                2,
                29,
                0,
                0,
                0,
            ),
            ("the 24th hour", 2024, 1, 1, 24, 0, 0),
            ("the 60th minute", 2024, 1, 1, 0, 60, 0),
            ("a leap second", 2024, 1, 1, 0, 0, 60),
            ("before the epoch", 1969, 12, 31, 23, 59, 59),
        ] {
            assert_eq!(
                unix_seconds(year, month, day, hour, minute, second),
                None,
                "{what} was given an instant"
            );
        }
    }

    /// Every day of a leap year and of a common one, walked in order. Each has
    /// to be exactly a day after the one before it, which catches a month
    /// length that is wrong without knowing which one it is.
    #[test]
    fn every_day_of_a_year_is_a_day_after_the_last() {
        for year in [2023i64, 2024] {
            let mut previous = unix_seconds(year, 1, 1, 0, 0, 0).expect("the first of January");
            let mut days = 1;
            for month in 1..=12u32 {
                for day in 1..=31u32 {
                    let Some(seconds) = unix_seconds(year, month, day, 0, 0, 0) else {
                        continue;
                    };
                    if month == 1 && day == 1 {
                        continue;
                    }
                    assert_eq!(
                        seconds - previous,
                        86_400,
                        "{year}-{month}-{day} is not a day after the date before it"
                    );
                    previous = seconds;
                    days += 1;
                }
            }
            assert_eq!(days, if year == 2024 { 366 } else { 365 }, "{year}");
        }
    }
}
