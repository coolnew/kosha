// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Turning numbers into the strings a person reads.
//!
//! Shared by the CLI and the GUI so that the same byte count never renders two
//! different ways in two places.

use std::borrow::Cow;
use std::fmt::Write as _;

/// A name or path with its control characters replaced, for showing a person.
///
/// On Unix a filename may contain a newline, a tab, an escape — every byte
/// except `/` and NUL is legal, and people do it both by accident and on
/// purpose. Printed raw into an aligned table, a newline breaks the row in
/// half and every column after it lands in the wrong place; in a GUI label it
/// either disappears or shifts the layout. Windows forbids these characters, so
/// nothing here noticed until the scanner was pointed at a Linux directory.
///
/// Replaced with `?`, which is what `ls` does writing to a terminal. Not
/// U+FFFD: that already means "these bytes were not valid text" and conflating
/// the two would hide which problem a name actually has.
///
/// **For display only.** Matching must see the real name, or a query would fail
/// to find the file it names; the CSV and JSON writers have their own escaping
/// and round-trip these characters exactly.
///
/// Borrows when there is nothing to replace, which is almost always — this runs
/// per visible row.
pub fn printable(text: &str) -> Cow<'_, str> {
    // DEL is left alone: it is invisible rather than disruptive, and `ls` keeps
    // it too. What matters here is anything that moves the cursor.
    if !text.chars().any(|c| c.is_control()) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect(),
    )
}

/// Which power the units step by.
///
/// Disk tools disagree about this and users get burned by it, so we are
/// explicit: `Binary` says `GiB` and means 1024³, `Decimal` says `GB` and means
/// 10⁹. Never a `GB` that secretly means 1024³.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Units {
    #[default]
    Binary,
    Decimal,
}

const BINARY: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
const DECIMAL: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];

/// Split a byte count into a scaled value and its unit.
pub fn scale(bytes: u64, units: Units) -> (f64, &'static str) {
    let (step, labels) = match units {
        Units::Binary => (1024.0_f64, &BINARY),
        Units::Decimal => (1000.0_f64, &DECIMAL),
    };
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= step && idx < labels.len() - 1 {
        value /= step;
        idx += 1;
    }
    // **A value that will *print* as the step belongs in the next unit.**
    // 1,048,575 bytes is 1023.999 KiB, and three significant figures made that
    // `1024 KiB` — a spelling of one mebibyte that nothing else in the world
    // uses, and one character wider than every other row in the column. The
    // same at every step: `1024 MiB`, `1024 GiB`. Both readings are a rounding
    // of the same number; only one of them is in the unit it belongs to.
    //
    // Half a unit, because that is what `{:.0}` rounds up from, and `{:.0}` is
    // what [`bytes_in`] uses for everything at or above 100 — which anything
    // near the step always is.
    if idx < labels.len() - 1 && value >= step - 0.5 {
        value /= step;
        idx += 1;
    }
    (value, labels[idx])
}

/// `4.2 GiB`. Three significant figures, which is as much precision as anyone
/// can act on and enough that column widths stay stable.
pub fn bytes(n: u64) -> String {
    bytes_in(n, Units::default())
}

pub fn bytes_in(n: u64, units: Units) -> String {
    let (value, label) = scale(n, units);
    if label == "B" {
        format!("{n} B")
    } else if value >= 100.0 {
        format!("{value:.0} {label}")
    } else if value >= 10.0 {
        format!("{value:.1} {label}")
    } else {
        format!("{value:.2} {label}")
    }
}

/// `91,204`. Grouped so a six-figure file count is readable at a glance.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let first = digits.len() % 3;
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && i >= first && (i - first).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `1.4%`, or `<0.1%` rather than a misleading `0.0%`.
pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "—".into();
    }
    let pct = part as f64 / whole as f64 * 100.0;
    if pct > 0.0 && pct < 0.1 {
        "<0.1%".into()
    } else if (99.5..100.0).contains(&pct) {
        // The same argument as `<0.1%`, at the other end and found the same
        // way: 999,999 bytes of a million printed `100%`, beside a row that
        // was still there and still had a size. A share that is not all of it
        // must not be spelled as all of it — a reader who sees `100%` on one
        // row and anything at all on another has been told two things that
        // cannot both be true.
        ">99%".into()
    } else if pct >= 10.0 {
        format!("{pct:.0}%")
    } else {
        format!("{pct:.1}%")
    }
}

/// `2.4 s`, `840 ms`, `1m 12s`.
pub fn duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    // The thresholds are where the *rounded* number would reach the next unit,
    // not where the number does: at a flat `1.0` and `60.0` a scan that took
    // 999.6 ms printed `1000 ms` and one that took 59.97 s printed `60.0 s`.
    // The same slip as `1024 KiB`, in the other column of the same line.
    if secs < 0.9995 {
        format!("{:.0} ms", secs * 1000.0)
    } else if secs < 59.95 {
        format!("{secs:.1} s")
    } else {
        // Rounded to whole seconds *first*, then split. Taking the minutes
        // from the unrounded value and rounding the remainder afterwards gave
        // `0m 60s` for 59.97 seconds — the carry has to happen before the
        // division, not after it.
        let whole = secs.round() as u64;
        format!("{}m {}s", whole / 60, whole % 60)
    }
}

/// `3 months ago`. Deliberately coarse: the exact timestamp is noise when the
/// question is "is this stale?".
pub fn age(mtime: u32, now: u32) -> String {
    if mtime == 0 {
        return "unknown".into();
    }
    let secs = now.saturating_sub(mtime) as u64;
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;

    let (n, unit) = match secs {
        s if s < MINUTE => return "just now".into(),
        s if s < HOUR => (s / MINUTE, "minute"),
        s if s < DAY => (s / HOUR, "hour"),
        s if s < MONTH => (s / DAY, "day"),
        s if s < YEAR => (s / MONTH, "month"),
        s => (s / YEAR, "year"),
    };
    let mut out = String::with_capacity(16);
    let _ = write!(out, "{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    out
}

/// `1 project`, `4 projects`, `0 projects`, `9 copies`, `2 matches`.
///
/// Two regular spelling rules beyond appending `s`, because both have already
/// been hit in practice: a consonant before a final `y` becomes `ies`, and a
/// word ending in a sibilant takes `es`. Nothing further — a caller wanting an
/// irregular plural should say so in its own words rather than teaching this
/// function English.
pub fn plural(n: usize, singular: &str) -> String {
    if n == 1 {
        return format!("{n} {singular}");
    }
    format!("{} {}", count(n as u64), pluralise(singular))
}

/// The plural spelling of one noun.
fn pluralise(singular: &str) -> String {
    let mut chars = singular.chars().rev();
    let last = chars.next();
    let before = chars.next();

    match last {
        // "copy" -> "copies", but "day" -> "days": the rule turns on whether a
        // vowel precedes the y.
        Some('y') if before.is_some_and(|c| !"aeiou".contains(c)) => {
            format!("{}ies", &singular[..singular.len() - 1])
        }
        // A bare "s" after a sibilant is unpronounceable: "matchs", "boxs".
        Some('s' | 'x' | 'z') => format!("{singular}es"),
        Some('h') if matches!(before, Some('c' | 's')) => format!("{singular}es"),
        _ => format!("{singular}s"),
    }
}

/// `2024-02-29T13:05:00Z` — minimal RFC 3339, UTC.
///
/// Written by hand rather than pulling in a date crate for one format string,
/// and living here rather than in each caller: the audit log, the exporter and
/// the query language all convert between a day count and a calendar date, and
/// three copies of Hinnant's algorithm is two too many.
///
/// Takes a *signed* count, and divides Euclidean, so a moment before 1970 is a
/// date in 1969 rather than a wrong one in 1970. `civil_from_days` below has
/// always handled negative day counts — the limitation was entirely here, in
/// the unsigned parameter and in truncating division: `-1 / 86_400` truncates
/// toward zero and gives day 0 with a remainder of -1, putting one second
/// before the epoch on 1970-01-01, while `div_euclid` floors to day -1 and
/// `rem_euclid` gives 86,399. Both spellings print a *plausible* date, which is
/// how the difference goes unnoticed.
///
/// No caller can reach a negative value today: every one of them passes a
/// `Node::mtime`, which is a `u32` saturating at the epoch, so a pre-1970
/// timestamp is already clamped before it arrives. That clamp is a deliberate
/// trade for the 48-byte node and is not what this fixes. This makes the
/// function correct for whoever calls it next — several archivers do write
/// pre-epoch stamps, and a scan of an extracted tree is where it would surface.
/// Reported by `marg`, which widened its own row to `i64` and hit it.
pub fn rfc3339(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Parse an RFC 3339 timestamp to Unix seconds, discarding fractional precision.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let decimal = |part: &[u8]| {
        part.iter().try_fold(0u32, |value, &digit| {
            digit
                .is_ascii_digit()
                .then(|| value * 10 + u32::from(digit - b'0'))
        })
    };
    let (date_time, zone) = bytes.split_at_checked(19)?;
    if date_time[4] != b'-'
        || date_time[7] != b'-'
        || !matches!(date_time[10], b'T' | b't')
        || date_time[13] != b':'
        || date_time[16] != b':'
    {
        return None;
    }
    let year = i64::from(decimal(&date_time[..4])?);
    let month = decimal(&date_time[5..7])?;
    let day = decimal(&date_time[8..10])?;
    let hour = decimal(&date_time[11..13])?;
    let minute = decimal(&date_time[14..16])?;
    let second = decimal(&date_time[17..19])?;
    if !is_a_date(year, month, day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let zone = if let [b'.', fraction @ ..] = zone {
        let length = fraction
            .iter()
            .take_while(|digit| digit.is_ascii_digit())
            .count();
        if length == 0 {
            return None;
        }
        &fraction[length..]
    } else {
        zone
    };
    let offset = match zone {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let hours = decimal(&[*h1, *h2])?;
            let minutes = decimal(&[*m1, *m2])?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = i64::from(hours * 3600 + minutes * 60);
            if *sign == b'-' { -seconds } else { seconds }
        }
        _ => return None,
    };
    Some(
        days_from_civil(year, month, day) * 86_400 + i64::from(hour * 3600 + minute * 60 + second)
            - offset,
    )
}

/// Now, as RFC 3339 UTC.
pub fn now_rfc3339() -> String {
    rfc3339(i64::from(now_unix()))
}

/// Month names, three letters, for stamps a person reads rather than parses.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// When something happened, on the clock, as short as it can be and still be
/// unambiguous.
///
/// `14:32:06` today, `12 Sep 14:32` earlier this year, `12 Sep 2025 14:32`
/// before that. The parts that are the same as now are the parts nobody needs
/// told: somebody reading a row from today is not deciding anything by its
/// date.
///
/// **Beside a relative age, never instead of one.** "3 days ago" is what a
/// person is deciding on and this is how they tell two of them apart — which
/// is the entire reason it exists. Snapshots taken a minute apart both read
/// "just now", and a list of rows that all say the same thing is a list you
/// cannot act on.
///
/// **Seconds, but only for today.** Two snapshots a few seconds apart is the
/// case this has to separate — it is what a double click makes, and what a
/// person makes when they take one before a cleanup and one after — and to the
/// minute they read identically. On an older day the date is the identity and
/// six more characters of precision are noise in a rail 200 pixels wide.
///
/// Local time where the platform will say, and UTC with a `Z` where it will
/// not — the `Z` being the difference between a time that is right and a time
/// that is merely plausible.
pub fn stamp(secs: u64, now: u64) -> String {
    let Some(then) = crate::platform::local_clock(secs as i64) else {
        // No local clock: say so in the only way that cannot mislead.
        let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
        let (year, month, day) = civil_from_days(days);
        return format!(
            "{day} {} {year} {:02}:{:02}:{:02}Z",
            MONTHS[(month as usize - 1).min(11)],
            rest / 3600,
            (rest % 3600) / 60,
            rest % 60
        );
    };
    let today = crate::platform::local_clock(now as i64);

    let clock = format!("{:02}:{:02}", then.hour, then.minute);
    let month = MONTHS[(then.month as usize - 1).min(11)];

    match today {
        Some(today)
            if (today.year, today.month, today.day) == (then.year, then.month, then.day) =>
        {
            format!("{clock}:{:02}", then.second)
        }
        Some(today) if today.year == then.year => format!("{} {month} {clock}", then.day),
        _ => format!("{} {month} {} {clock}", then.day, then.year),
    }
}

/// An exact, unambiguous timestamp for search boundaries. Unlike `stamp`, this
/// always retains the date, year and seconds, plus the per-instant UTC offset.
/// If local time cannot be represented, UTC is explicitly marked with `Z`.
pub fn local_timestamp(secs: u64) -> String {
    let local = i64::try_from(secs).ok().and_then(|instant| {
        let clock = crate::platform::local_clock(instant)?;
        let wall = days_from_civil(clock.year, clock.month, clock.day)
            .checked_mul(86_400)?
            .checked_add(
                i64::from(clock.hour) * 3600
                    + i64::from(clock.minute) * 60
                    + i64::from(clock.second),
            )?;
        let offset = wall.checked_sub(instant)?;
        // Historical second-level offsets cannot be represented in RFC 3339.
        if offset % 60 != 0 || offset.unsigned_abs() >= 86_400 {
            return None;
        }
        let zone = if offset == 0 {
            "Z".to_string()
        } else {
            format!(
                "{}{:02}:{:02}",
                if offset < 0 { '-' } else { '+' },
                offset.unsigned_abs() / 3600,
                offset.unsigned_abs() % 3600 / 60
            )
        };
        Some(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{zone}",
            clock.year, clock.month, clock.day, clock.hour, clock.minute, clock.second
        ))
    });
    local.unwrap_or_else(|| utc_timestamp(secs))
}

fn utc_timestamp(secs: u64) -> String {
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rest = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Whether a year is a leap year in the Gregorian calendar.
///
/// The hundred-year and four-hundred-year rules included, which matters for
/// exactly one year in living memory and is exactly the sort of thing nobody
/// notices is missing: 2000 is a leap year, 1900 and 2100 are not, and a date
/// library that gets it wrong is right for ninety-six years out of a hundred.
pub fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// How many days that month has, or zero if there is no such month.
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Whether those three numbers name a day that exists.
///
/// **Here, beside the conversion, because the conversion answers anyway.**
/// `days_from_civil` is arithmetic and will happily turn the 30th of February
/// into the 2nd of March — a date in the wrong month, which looks like a fact.
/// `calendar` says that out loud for the dates it reads off a disk ("either
/// damage or a formatter that did not care") and validated them; the query
/// language, which gets its dates from a person, checked only `1..=31` and
/// accepted `modified < 2026-02-30`. That filter meant the 2nd of March: two
/// days of files on the wrong side of a boundary somebody typed.
pub fn is_a_date(year: i64, month: u32, day: u32) -> bool {
    (1..=days_in_month(year, month)).contains(&day)
}

/// The inverse: a calendar date to a count of days since 1970-01-01.
///
/// Here rather than beside its caller because [`civil_from_days`] is here, and a
/// conversion whose two halves live in different crates is a conversion that can
/// drift in one of them. `kosha-query` carried its own copy of this.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The Unix second at which a calendar date begins **where the user is**.
///
/// A date typed into a filter is a date on the wall, not a date in Greenwich.
/// [`stamp`] prints local time, so a boundary set at midnight UTC puts the
/// user's own offset of every day on the wrong side of it: on the machine this
/// was written on, the second before `2026-03-01` displayed as `1 Mar 05:29`.
///
/// `None` where the platform will not say what local time is — the same
/// condition under which [`stamp`] falls back to printing UTC with a `Z`, and the
/// caller should fall back the same way rather than pretend.
///
/// **Two passes, because an offset is a property of an instant and not of a
/// zone.** The first finds the offset in force at midnight UTC and the second at
/// the answer that offset produces, which is what makes a date the far side of a
/// daylight-saving change come out right. Where local midnight does not exist at
/// all — a zone that springs forward at midnight — this lands on the first
/// instant of that local day, which is what a boundary means anyway; where it
/// happens twice, it lands on one of the two, and an hour of ambiguity on a
/// filter boundary is not worth a table of transitions to resolve.
pub fn local_midnight(year: i64, month: u32, day: u32) -> Option<u64> {
    let utc_midnight = days_from_civil(year, month, day).checked_mul(86_400)?;

    let offset_at = |instant: i64| -> Option<i64> {
        let clock = crate::platform::local_clock(instant)?;
        let wall = days_from_civil(clock.year, clock.month, clock.day).checked_mul(86_400)?
            + i64::from(clock.hour) * 3600
            + i64::from(clock.minute) * 60
            + i64::from(clock.second);
        Some(wall - instant)
    };

    let first = offset_at(utc_midnight)?;
    let refined = offset_at(utc_midnight - first)?;
    u64::try_from(utc_midnight - refined).ok()
}

/// Local calendar date at an explicit instant; no hidden clock read.
pub fn local_date(now: u32) -> Option<[i32; 3]> {
    let clock = crate::platform::local_clock(i64::from(now))?;
    Some([
        clock.year.try_into().ok()?,
        clock.month as i32,
        clock.day as i32,
    ])
}

/// Howard Hinnant's `civil_from_days`, the standard branch-free conversion from
/// a day count to a `(year, month, day)` calendar date.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Seconds since the Unix epoch, saturating — the same clock `Node::mtime` uses.
pub fn now_unix() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(u64::from(u32::MAX)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_search_timestamp_retains_date_seconds_and_offset() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(1_709_164_800), "2024-02-29T00:00:00Z");
        for seconds in [0, 1, 1_709_164_800, 1_795_003_201, u64::from(u32::MAX)] {
            let text = local_timestamp(seconds);
            // Independently decode the displayed fields and offset: the
            // label must identify the original instant to the second.
            let date: Vec<i64> = text[..10]
                .split('-')
                .map(|part| part.parse().unwrap())
                .collect();
            let time: Vec<i64> = text[11..19]
                .split(':')
                .map(|part| part.parse().unwrap())
                .collect();
            let offset = if text.ends_with('Z') {
                0
            } else {
                let hours: i64 = text[20..22].parse().unwrap();
                let minutes: i64 = text[23..25].parse().unwrap();
                let value = hours * 3600 + minutes * 60;
                if text.as_bytes()[19] == b'-' {
                    -value
                } else {
                    value
                }
            };
            let decoded = days_from_civil(date[0], date[1] as u32, date[2] as u32) * 86_400
                + time[0] * 3600
                + time[1] * 60
                + time[2]
                - offset;
            assert_eq!(decoded as u64, seconds, "{text}");
        }
        // A query can contain a distant unsigned timestamp. Display must not
        // wrap it through an i64 clock or panic when falling back to UTC.
        assert_eq!(local_timestamp(u64::MAX), utc_timestamp(u64::MAX));
    }

    /// The stamp exists to tell two snapshots apart, so the one thing it must
    /// never do is print the same string for two different moments in a day.
    #[test]
    fn two_moments_a_second_apart_read_differently() {
        let now = 1_757_680_000;
        assert_ne!(stamp(now, now), stamp(now - 1, now));
        assert_ne!(stamp(now, now), stamp(now - 60, now));
    }

    /// Today is a time, this year is a date and a time, and anything older
    /// carries its year. Checked by shape rather than by exact text, because
    /// the machine running the test has its own time zone and that is the
    /// point of the function.
    #[test]
    fn a_stamp_says_no_more_than_it_has_to() {
        let now = 1_757_680_000;
        let today = stamp(now, now);
        assert_eq!(today.len(), 8, "{today}");
        assert_eq!(today.matches(':').count(), 2, "{today}");

        // Two hundred days back is another date in the same year, unless the
        // two straddle a new year, in which case it carries one — both are
        // correct, and the year is what tells them apart.
        let older = stamp(now - 86_400 * 200, now);
        assert!(older.contains(':'), "{older}");
        assert_eq!(
            older.matches(':').count(),
            1,
            "no seconds on an old row: {older}"
        );

        let ancient = stamp(now - 86_400 * 900, now);
        assert!(
            ancient.split_whitespace().count() == 4,
            "an old stamp carries its year: {ancient}"
        );
    }

    /// A stamp with no clock behind it still has to be a stamp, and has to say
    /// which zone it is in rather than letting a reader assume theirs.
    #[test]
    fn a_stamp_far_outside_a_clock_falls_back_to_utc() {
        // Year 5000-odd: past what a Windows SYSTEMTIME converts and past what
        // any sensible clock reports, so this exercises the fallback on the
        // platform where there is one.
        let far = 95_617_584_000u64;
        let text = stamp(far, 1_757_680_000);
        assert!(text.contains(':'), "{text}");
    }

    #[test]
    fn control_characters_never_reach_a_terminal() {
        // A newline in a filename is legal on Unix and used to split a table
        // row in half, putting every column after it in the wrong place.
        assert_eq!(printable("two\nlines.txt"), "two?lines.txt");
        assert_eq!(printable("tab\there"), "tab?here");
        assert_eq!(printable("esc\u{1b}[31m"), "esc?[31m");
    }

    #[test]
    fn ordinary_names_are_borrowed_untouched() {
        // This runs once per visible row, so the common case must not allocate.
        assert!(matches!(printable("node_modules"), Cow::Borrowed(_)));
        assert!(matches!(printable("Vidéos-café.txt"), Cow::Borrowed(_)));
        // The replacement character stands for invalid bytes and is not a
        // control; it means something different and must survive.
        assert_eq!(printable("caf\u{fffd}.txt"), "caf\u{fffd}.txt");
        assert_eq!(printable(""), "");
    }

    #[test]
    fn converts_days_to_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // A leap day, which is where an off-by-one in this algorithm shows up.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn parses_rfc_3339_offsets_and_validates_dates() {
        assert_eq!(
            parse_rfc3339("1970-01-01T05:30:00.123456789+05:30"),
            Some(0)
        );
        assert_eq!(parse_rfc3339("1969-12-31T19:00:01-05:00"), Some(1));
        assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), Some(-1));
        assert_eq!(
            parse_rfc3339("2024-02-29T23:59:59Z")
                .map(rfc3339)
                .as_deref(),
            Some("2024-02-29T23:59:59Z")
        );
        for invalid in [
            "2023-02-29T00:00:00Z",
            "2024-01-01T00:00:00.Z",
            "2024-01-01T00:00:00+24:00",
            "2024-01-01T00:00:00",
        ] {
            assert_eq!(parse_rfc3339(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn writes_rfc_3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // Midnight and the second before it, where the time carry happens.
        assert_eq!(rfc3339(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(rfc3339(86_400), "1970-01-02T00:00:00Z");
    }

    /// The epoch boundary from the other side. Truncating division gets every
    /// one of these wrong and prints a plausible date while doing it: one
    /// second before the epoch came out as `1970-01-01T00:00:00Z`, which is a
    /// real timestamp that happens to be a whole day and a second out.
    ///
    /// `civil_from_days` was never the problem — the test above already pins
    /// `civil_from_days(-1)` at 1969-12-31. The signature was.
    #[test]
    fn writes_dates_before_the_epoch() {
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59Z");
        assert_eq!(rfc3339(-86_400), "1969-12-31T00:00:00Z");
        assert_eq!(rfc3339(-86_401), "1969-12-30T23:59:59Z");
        // A stamp an archiver might plausibly carry: 1960-01-01T00:00:00Z.
        assert_eq!(rfc3339(-315_619_200), "1960-01-01T00:00:00Z");
    }

    /// Every second of a day around the boundary, checked against an
    /// independent reconstruction — the failure mode here is an off-by-one-day
    /// that looks entirely reasonable in isolation, so spot checks are weak.
    #[test]
    fn the_day_and_the_time_never_disagree_across_the_epoch() {
        for secs in -90_000i64..90_000 {
            let text = rfc3339(secs);
            let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
            let (y, m, d) = civil_from_days(days);
            let expected = format!(
                "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
                rem / 3600,
                (rem % 3600) / 60,
                rem % 60
            );
            assert_eq!(text, expected, "at {secs}");
            // The time of day is always in range, which truncation breaks by
            // producing a negative remainder.
            let hour: i64 = text[11..13].parse().unwrap();
            assert!((0..24).contains(&hour), "{text} at {secs}");
        }
    }

    #[test]
    fn pluralises_regular_nouns() {
        assert_eq!(plural(1, "copy"), "1 copy");
        assert_eq!(plural(0, "copy"), "0 copies");
        assert_eq!(plural(9, "copy"), "9 copies");
        assert_eq!(plural(2, "directory"), "2 directories");
        // A vowel before the y keeps it.
        assert_eq!(plural(3, "day"), "3 days");
        // Sibilants take -es.
        assert_eq!(plural(2, "match"), "2 matches");
        assert_eq!(plural(2, "box"), "2 boxes");
        assert_eq!(plural(2, "class"), "2 classes");
        assert_eq!(plural(4, "project"), "4 projects");
        assert_eq!(plural(2, "duplicate set"), "2 duplicate sets");
        // Large counts are grouped, and the noun still agrees.
        assert_eq!(plural(1234, "entry"), "1,234 entries");
    }

    /// A number that would print as its own unit is carried to the next.
    ///
    /// Measured across every step: one byte short of a mebibyte printed
    /// `1024 KiB`, and so did one byte short of a gibibyte in mebibytes, and a
    /// tebibyte in gibibytes. Nothing spells a mebibyte that way, and the row
    /// was a character wider than the ones around it.
    #[test]
    fn a_value_one_byte_short_of_a_unit_is_not_spelled_in_the_old_one() {
        assert_eq!(bytes(1024 * 1024 - 1), "1.00 MiB");
        assert_eq!(bytes(1024 * 1024 * 1024 - 1), "1.00 GiB");
        assert_eq!(bytes(1024u64.pow(4) - 1), "1.00 TiB");
        assert_eq!(bytes_in(1_000_000 - 1, Units::Decimal), "1.00 MB");

        // And nothing below the rounding point moves: 1023.4 KiB is still
        // said in kibibytes, because that is what it is.
        assert_eq!(bytes(1_048_000), "1023 KiB");
        assert_eq!(bytes(1023), "1023 B");

        // The top unit has nowhere to carry to and must not try.
        assert_eq!(bytes(u64::MAX), "16.0 EiB");
    }

    /// A duration is rounded too, and had the same two boundaries wrong.
    ///
    /// It had no test at all, which is how both survived: a scan that took
    /// 999.6 ms said `1000 ms`, and one that took 59.97 s said `60.0 s`.
    /// Neither is a unit anybody would choose for that number, and both sit in
    /// the same line of output as the size that said `1024 KiB`.
    #[test]
    fn a_duration_rounds_into_the_unit_it_belongs_to() {
        use std::time::Duration;

        assert_eq!(duration(Duration::from_millis(1)), "1 ms");
        assert_eq!(duration(Duration::from_micros(999_600)), "1.0 s");
        assert_eq!(duration(Duration::from_micros(999_400)), "999 ms");
        assert_eq!(duration(Duration::from_secs(1)), "1.0 s");
        assert_eq!(duration(Duration::from_millis(59_970)), "1m 0s");
        assert_eq!(duration(Duration::from_millis(59_940)), "59.9 s");
        assert_eq!(duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(duration(Duration::from_secs(3661)), "61m 1s");
    }

    /// And the same at the top of a percentage.
    #[test]
    fn a_share_that_is_not_all_of_it_is_not_spelled_as_all_of_it() {
        assert_eq!(percent(999_999, 1_000_000), ">99%");
        assert_eq!(percent(999, 1000), ">99%");
        // All of it really is all of it.
        assert_eq!(percent(1000, 1000), "100%");
        // And the ordinary readings are unchanged.
        assert_eq!(percent(99, 100), "99%");
        assert_eq!(percent(1, 3), "33%");
    }

    #[test]
    fn scales_bytes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.00 KiB");
        assert_eq!(bytes(1536), "1.50 KiB");
        assert_eq!(bytes(4 * 1024 * 1024 * 1024), "4.00 GiB");
        assert_eq!(bytes_in(1_000_000, Units::Decimal), "1.00 MB");
    }

    #[test]
    fn drops_precision_as_magnitude_grows() {
        assert_eq!(bytes(12 * 1024), "12.0 KiB");
        assert_eq!(bytes(512 * 1024), "512 KiB");
    }

    #[test]
    fn groups_counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(91_204), "91,204");
        assert_eq!(count(1_204_331), "1,204,331");
    }

    #[test]
    fn percent_never_lies_about_small_shares() {
        assert_eq!(percent(1, 100_000), "<0.1%");
        assert_eq!(percent(0, 100), "0.0%");
        assert_eq!(percent(50, 100), "50%");
        assert_eq!(percent(0, 0), "—");
    }

    #[test]
    fn pluralises_and_groups() {
        assert_eq!(plural(1, "project"), "1 project");
        assert_eq!(plural(0, "project"), "0 projects");
        assert_eq!(plural(4, "project"), "4 projects");
        assert_eq!(plural(91_204, "file"), "91,204 files");
    }

    #[test]
    fn ages_read_naturally() {
        let now = 1_000_000_000;
        assert_eq!(age(now, now), "just now");
        assert_eq!(age(now - 3600, now), "1 hour ago");
        assert_eq!(age(now - 2 * 86400, now), "2 days ago");
        assert_eq!(age(0, now), "unknown");
    }

    /// The rules the calendar turns on, in the one place both callers read them.
    ///
    /// `calendar` had these privately, for dates found on a FAT or ISO volume,
    /// and `kosha-query` had `1..=31` for dates typed by a person — which accepts
    /// the 30th of February and converts it into the 2nd of March.
    #[test]
    fn a_month_is_as_long_as_it_is() {
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29, "a leap year");
        assert_eq!(days_in_month(2000, 2), 29, "divisible by 400");
        assert_eq!(days_in_month(1900, 2), 28, "divisible by 100 and not 400");
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(days_in_month(2026, 13), 0, "there is no such month");
        assert_eq!(days_in_month(2026, 0), 0);

        assert!(is_a_date(2024, 2, 29));
        assert!(!is_a_date(2023, 2, 29));
        assert!(!is_a_date(2026, 2, 30));
        assert!(!is_a_date(2026, 4, 31));
        assert!(!is_a_date(2026, 1, 0));
        assert!(!is_a_date(2026, 13, 1));
    }

    /// The two halves of the calendar conversion are each other's inverse.
    ///
    /// `days_from_civil` moved here from `kosha-query`, which had its own copy;
    /// this is the assertion that the copy that remains is the right one. The
    /// dates are the ones that break a naive implementation: the epoch itself, a
    /// leap day, the century that is not a leap year, and the one that is.
    #[test]
    fn the_calendar_converts_both_ways() {
        for (year, month, day) in [
            (1970, 1, 1),
            (1969, 12, 31),
            (2000, 2, 29),
            (1900, 3, 1),
            (2024, 2, 29),
            (2100, 3, 1),
            (2026, 9, 26),
        ] {
            let days = days_from_civil(year, month, day);
            assert_eq!(
                civil_from_days(days),
                (year, month, day),
                "{year}-{month:02}-{day:02} did not come back"
            );
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0, "the epoch is day zero");
    }

    /// Local midnight is the first instant of a local day, wherever this runs.
    ///
    /// Stated as a boundary rather than as a clock reading, because that is what
    /// it is for and because it stays true in a zone that has no midnight on the
    /// day it springs forward: the instant returned is on the day asked for, and
    /// the instant before it is not.
    ///
    /// The dates are the daylight-saving transitions of the zones most people are
    /// in, plus ordinary days. On a machine that keeps UTC — a build server, this
    /// project's WSL distribution — every one of them is an ordinary day, and the
    /// assertion is still the right one.
    #[test]
    fn a_local_day_begins_once_and_the_second_before_it_is_yesterday() {
        for (year, month, day) in [
            (2026, 1, 1),
            (2026, 3, 8),
            (2026, 3, 29),
            (2026, 6, 15),
            (2026, 9, 6),
            (2026, 10, 25),
            (2026, 11, 1),
            (2024, 2, 29),
        ] {
            let Some(at) = local_midnight(year, month, day) else {
                // A platform with no local clock says so, and the query language
                // falls back to UTC for the same reason. Nothing to assert.
                return;
            };
            let here = crate::platform::local_clock(at as i64).expect("a clock that just answered");
            assert_eq!(
                (here.year, here.month, here.day),
                (year, month, day),
                "{year}-{month:02}-{day:02} began on some other day: {here:?}"
            );
            let before = crate::platform::local_clock(at as i64 - 1).expect("one second earlier");
            assert_ne!(
                (before.year, before.month, before.day),
                (year, month, day),
                "the second before {year}-{month:02}-{day:02} began was already on it"
            );
        }
    }

    /// And it agrees with what the table prints, which is the whole point.
    #[test]
    fn the_boundary_and_the_stamp_agree_about_the_day() {
        let Some(at) = local_midnight(2026, 3, 1) else {
            return;
        };
        let now = at + 40 * 86_400;
        assert!(stamp(at, now).starts_with("1 Mar"), "{}", stamp(at, now));
        assert!(
            stamp(at - 1, now).starts_with("28 Feb"),
            "{}",
            stamp(at - 1, now)
        );
    }
}
