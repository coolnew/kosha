// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Calendar-only grammar. Calendar periods use caller-resolved local boundaries;
//! rolling durations belong to the separate exact-quantity/instant grammar.

use crate::numbers;

pub(crate) struct Date {
    pub days: i64,
    pub used: usize,
}
pub(crate) struct Period {
    pub from: i64,
    pub to: i64,
    pub used: usize,
}

pub(crate) fn parse_date<'a>(words: impl Iterator<Item = &'a str>) -> Option<Date> {
    let words: Vec<_> = words.take(5).collect();
    let first = *words.first()?;
    if let Some(date) = iso(first) {
        return Some(Date {
            days: civil_days(date)?,
            used: 1,
        });
    }
    let (month, day, year_index) = if let Some(month) = month(first) {
        let day = day(words.get(1)?)?;
        let year_index = if words.get(2) == Some(&",") { 3 } else { 2 };
        (month, day, year_index)
    } else {
        let day = day(first)?;
        let month = month(words.get(1)?)?;
        (month, day, 2)
    };
    let year = year(words.get(year_index)?)?;
    Some(Date {
        days: civil_days([year, month, day])?,
        used: year_index + 1,
    })
}

pub(crate) fn parse_named_period<'a>(words: impl Iterator<Item = &'a str>) -> Option<Period> {
    let words: Vec<_> = words.take(3).collect();
    let first = *words.first()?;
    let (from, to, used) = if let Some(month) = month(first) {
        let year = year(words.get(1)?)?;
        let next = if month == 12 {
            [year + 1, 1, 1]
        } else {
            [year, month + 1, 1]
        };
        ([year, month, 1], next, 2)
    } else {
        let year = year(first)?;
        ([year, 1, 1], [year + 1, 1, 1], 1)
    };
    Some(Period {
        from: civil_days(from)?,
        to: civil_days(to)?,
        used,
    })
}

pub(crate) fn parse_relative<'a>(
    words: impl Iterator<Item = &'a str>,
    today: [i32; 3],
) -> Option<Period> {
    let words: Vec<_> = words.take(24).collect();
    let today = civil_days(today)?;
    if words.first() == Some(&"last") {
        let weekday = weekday(words.get(1)?)?;
        let current = (today + 3).rem_euclid(7);
        let difference = (current - weekday).rem_euclid(7);
        let difference = if difference == 0 { 7 } else { difference };
        let from = today.checked_sub(difference)?;
        return bounded_period(from, from + 1, 2);
    }
    let amount = numbers::parse(words.iter().copied())?;
    let count = amount.integer()?;
    let multiplier = match *words.get(amount.used)? {
        "day" | "days" => 1u64,
        "week" | "weeks" => 7,
        _ => return None,
    };
    if words.get(amount.used + 1) != Some(&"ago") {
        return None;
    }
    let offset = i64::try_from(count.checked_mul(multiplier)?).ok()?;
    let from = today.checked_sub(offset)?;
    bounded_period(from, from.checked_add(1)?, amount.used + 2)
}

fn bounded_period(from: i64, to: i64, used: usize) -> Option<Period> {
    if from < 0 || to > civil_days([9999, 12, 31])? {
        return None;
    }
    Some(Period { from, to, used })
}

fn iso(word: &str) -> Option<[i32; 3]> {
    let mut parts = word.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let date = [year, month, day];
    civil_days(date)?;
    Some(date)
}
fn year(word: &str) -> Option<i32> {
    if word.len() != 4 || !word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    word.parse::<i32>()
        .ok()
        .filter(|y| (1970..=9999).contains(y))
}
fn day(word: &str) -> Option<i32> {
    let split = word
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(word.len());
    if split == 0 {
        return None;
    }
    let day = word[..split].parse::<i32>().ok()?;
    let suffix = &word[split..];
    let expected = if (11..=13).contains(&(day % 100)) {
        "th"
    } else {
        match day % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    if !suffix.is_empty() && suffix != expected {
        return None;
    }
    (1..=31).contains(&day).then_some(day)
}
fn month(word: &str) -> Option<i32> {
    Some(match word {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}
fn weekday(word: &str) -> Option<i64> {
    Some(match word {
        "monday" | "mon" => 0,
        "tuesday" | "tue" | "tues" => 1,
        "wednesday" | "wed" => 2,
        "thursday" | "thu" | "thurs" => 3,
        "friday" | "fri" => 4,
        "saturday" | "sat" => 5,
        "sunday" | "sun" => 6,
        _ => return None,
    })
}

pub(crate) fn civil_days(date: [i32; 3]) -> Option<i64> {
    let [year, month, day] = date;
    if !(1970..=9999).contains(&year)
        || !kosha_core::format::is_a_date(i64::from(year), month as u32, day as u32)
    {
        return None;
    }
    Some(kosha_core::format::days_from_civil(
        i64::from(year),
        month as u32,
        day as u32,
    ))
}
pub(crate) fn date_for_days(days: i64) -> String {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_named_dates_and_calendar_relative_days() {
        for text in ["november 5 , 2026", "5 november 2026", "nov 5th 2026"] {
            let date = parse_date(text.split_whitespace()).unwrap();
            assert_eq!(date_for_days(date.days), "2026-11-05");
        }
        let tuesday = parse_relative("last tuesday".split_whitespace(), [2026, 11, 17]).unwrap();
        assert_eq!(date_for_days(tuesday.from), "2026-11-10");
        let ago = parse_relative("two days ago".split_whitespace(), [2026, 11, 18]).unwrap();
        assert_eq!(date_for_days(ago.from), "2026-11-16");
        for text in [
            "11/5/2026",
            "november 31 2026",
            "february 29 2025",
            "november 5",
            "november 11st 2026",
        ] {
            assert!(parse_date(text.split_whitespace()).is_none(), "{text}");
        }
        assert!(parse_named_period("december 9999".split_whitespace()).is_none());
    }
}
