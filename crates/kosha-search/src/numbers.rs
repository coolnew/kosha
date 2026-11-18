// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Small exact English quantities. This module never rewrites source tokens:
//! the caller retains their byte spans and receives the consumed token count.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Quantity {
    pub numerator: u128,
    pub denominator: u128,
    pub used: usize,
}
impl Quantity {
    pub fn integer(&self) -> Option<u64> {
        self.numerator
            .is_multiple_of(self.denominator)
            .then(|| u64::try_from(self.numerator / self.denominator).ok())
            .flatten()
    }
    pub fn scaled_integer(&self, scale: u64) -> Option<u64> {
        let scaled = self.numerator.checked_mul(u128::from(scale))?;
        if !scaled.is_multiple_of(self.denominator) {
            return None;
        }
        u64::try_from(scaled / self.denominator).ok()
    }
    pub fn decimal(&self) -> String {
        let whole = self.numerator / self.denominator;
        let mut remainder = self.numerator % self.denominator;
        if remainder == 0 {
            return whole.to_string();
        }
        let mut out = format!("{whole}.");
        while remainder != 0 {
            remainder *= 10;
            out.push(char::from(b'0' + (remainder / self.denominator) as u8));
            remainder %= self.denominator;
        }
        out
    }
}

pub(crate) fn parse<'a>(words: impl Iterator<Item = &'a str>) -> Option<Quantity> {
    let words: Vec<_> = words
        .take(24)
        .take_while(|word| is_number_word(word))
        .collect();
    let mut best = None;
    for used in 1..=words.len() {
        if let Some((numerator, denominator)) = exact(&words[..used]) {
            best = Some(Quantity {
                numerator,
                denominator,
                used,
            });
        }
    }
    best
}

fn is_number_word(word: &str) -> bool {
    small(word).is_some()
        || tens(word).is_some()
        || word.split_once('-').is_some_and(|(a, b)| {
            tens(a).is_some() && small(b).is_some_and(|n| (1..=9).contains(&n))
        })
        || matches!(
            word,
            "hundred"
                | "thousand"
                | "million"
                | "billion"
                | "and"
                | "point"
                | "half"
                | "quarter"
                | "a"
        )
        || decimal(word).is_some()
}

fn exact(words: &[&str]) -> Option<(u128, u128)> {
    if words.len() == 1
        && let Some(result) = decimal(words[0])
    {
        return Some(result);
    }
    for (tail, denominator) in [
        (&["half"][..], 2),
        (&["half", "a"][..], 2),
        (&["a", "half"][..], 2),
        (&["quarter"][..], 4),
        (&["quarter", "a"][..], 4),
        (&["a", "quarter"][..], 4),
    ] {
        if words == tail {
            return Some((1, denominator));
        }
    }
    if words.len() >= 4
        && words[words.len() - 3] == "and"
        && words[words.len() - 2] == "a"
        && matches!(words[words.len() - 1], "half" | "quarter")
    {
        let whole = cardinal(&words[..words.len() - 3])?;
        let denominator = if words[words.len() - 1] == "half" {
            2
        } else {
            4
        };
        return Some((u128::from(whole) * denominator + 1, denominator));
    }
    if let Some(point) = words.iter().position(|word| *word == "point") {
        let whole = cardinal(&words[..point])?;
        let fractional = &words[point + 1..];
        if fractional.is_empty() || fractional.len() > 9 {
            return None;
        }
        let mut numerator = u128::from(whole);
        let mut denominator = 1u128;
        for word in fractional {
            let digit = small(word).filter(|n| *n < 10)?;
            numerator = numerator.checked_mul(10)?.checked_add(u128::from(digit))?;
            denominator *= 10;
        }
        return Some((numerator, denominator));
    }
    Some((u128::from(cardinal(words)?), 1))
}

fn decimal(word: &str) -> Option<(u128, u128)> {
    if word.is_empty() || !word.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let mut parts = word.split('.');
    let whole = parts.next()?;
    let fractional = parts.next();
    if parts.next().is_some() || whole.is_empty() {
        return None;
    }
    let whole = whole.parse::<u64>().ok()?;
    match fractional {
        None => Some((u128::from(whole), 1)),
        Some(raw) if !raw.is_empty() && raw.len() <= 9 => {
            let denominator = 10u128.pow(raw.len() as u32);
            Some((
                u128::from(whole) * denominator + raw.parse::<u128>().ok()?,
                denominator,
            ))
        }
        _ => None,
    }
}

fn cardinal(words: &[&str]) -> Option<u64> {
    if words == ["a"] {
        return Some(1);
    }
    if words.len() == 1 && words[0].bytes().all(|b| b.is_ascii_digit()) {
        return words[0].parse().ok();
    }
    let mut total = 0u64;
    let mut start = 0;
    let mut previous = u64::MAX;
    for (i, word) in words.iter().enumerate() {
        let scale = match *word {
            "billion" => 1_000_000_000,
            "million" => 1_000_000,
            "thousand" => 1000,
            _ => continue,
        };
        if scale >= previous {
            return None;
        }
        let group = group(&words[start..i])?;
        if group == 0 {
            return None;
        }
        total = total.checked_add(group.checked_mul(scale)?)?;
        previous = scale;
        start = i + 1;
    }
    let mut tail = &words[start..];
    if total > 0 && tail.first() == Some(&"and") {
        tail = &tail[1..];
        if tail.is_empty() {
            return None;
        }
    }
    if tail.is_empty() {
        return (total > 0).then_some(total);
    }
    if total > 0 && tail == ["a"] {
        return None;
    }
    let tail = group(tail)?;
    if total > 0 && tail == 0 {
        return None;
    }
    total.checked_add(tail)
}

fn group(words: &[&str]) -> Option<u64> {
    if words == ["a"] {
        return Some(1);
    }
    if words.len() >= 2 && words[1] == "hundred" {
        let first = small(words[0])
            .or_else(|| (words[0] == "a").then_some(1))
            .filter(|n| (1..=9).contains(n))?;
        let mut tail = &words[2..];
        if tail.is_empty() {
            return Some(first * 100);
        }
        if tail[0] == "and" {
            tail = &tail[1..];
        }
        let rest = under_hundred(tail)?;
        if rest == 0 {
            return None;
        }
        Some(first * 100 + rest)
    } else {
        under_hundred(words)
    }
}

fn under_hundred(words: &[&str]) -> Option<u64> {
    match words {
        [one] => small(one).or_else(|| tens(one)).or_else(|| {
            let (a, b) = one.split_once('-')?;
            Some(tens(a)? + small(b).filter(|n| (1..=9).contains(n))?)
        }),
        [a, b] => Some(tens(a)? + small(b).filter(|n| (1..=9).contains(n))?),
        _ => None,
    }
}
fn small(word: &str) -> Option<u64> {
    [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ]
    .iter()
    .position(|w| *w == word)
    .map(|i| i as u64)
}
fn tens(word: &str) -> Option<u64> {
    [
        "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ]
    .iter()
    .position(|w| *w == word)
    .map(|i| (i as u64 + 2) * 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_quantities_and_consumed_tokens() {
        assert!(exact(&["one", "hundred", "and", "a"]).is_none());
        assert!(exact(&["one", "thousand", "and", "a"]).is_none());
        for (words, value) in [
            (&["a"][..], (1, 1)),
            (&["a", "hundred"][..], (100, 1)),
            (&["a", "million"][..], (1_000_000, 1)),
            (&["one", "hundred", "and", "a", "half"][..], (201, 2)),
        ] {
            assert_eq!(exact(words), Some(value));
        }
        for (text, value, used) in [
            ("one hundred and five files", "105", 4),
            ("one point five six GB", "1.56", 4),
            ("half a GB", "0.5", 2),
            ("twenty-four hours", "24", 1),
            ("one and a half GB", "1.5", 4),
            ("one thousand and five files", "1005", 4),
        ] {
            let quantity = parse(text.split_whitespace()).unwrap();
            assert_eq!(quantity.decimal(), value);
            assert_eq!(quantity.used, used);
        }
        assert!(exact(&["one", "two"]).is_none());
        assert!(exact(&["one", "hundred", "hundred"]).is_none());
        assert!(exact(&["one", "thousand", "million"]).is_none());
        assert_eq!(
            parse("half files".split_whitespace()).unwrap().integer(),
            None
        );
    }
}
