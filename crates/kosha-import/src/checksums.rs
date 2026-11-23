// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Read checksum claims from rclone listings without opening any listed path.

use std::fmt;
use std::io::Read;
use std::str::FromStr;

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::{ImportError, Limits, listing_json_error, open_listing};

const MAX_ROWS: usize = 1_000_000;
const MAX_PATH_BYTES: usize = 64 << 20;
const MAX_INPUT_BYTES: u64 = 256 << 20;

/// The rclone checksum algorithm selected for the entire report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(field_identifier, rename_all = "lowercase")]
pub enum Algorithm {
    Md5,
    Sha1,
    Sha256,
    Quickxor,
    Dropbox,
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Quickxor => "quickxor",
            Self::Dropbox => "dropbox",
            Self::Unknown => "unknown",
        })
    }
}

impl FromStr for Algorithm {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "md5" => Ok(Self::Md5),
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            "quickxor" => Ok(Self::Quickxor),
            "dropbox" => Ok(Self::Dropbox),
            _ => Err("choose md5, sha1, sha256, quickxor or dropbox"),
        }
    }
}

impl Algorithm {
    fn width(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 | Self::Quickxor => 20,
            Self::Sha256 | Self::Dropbox => 32,
            Self::Unknown => 0,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Default)]
enum Checksum {
    #[default]
    Missing,
    Invalid,
    Hex([u8; 32], usize),
}

impl<'de> Deserialize<'de> for Checksum {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Value;

        impl<'de> Visitor<'de> for Value {
            type Value = Checksum;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a hexadecimal checksum")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Checksum, E> {
                if text.is_empty() {
                    return Ok(Checksum::Invalid);
                }
                let mut result = [0; 32];
                let (pairs, remainder) = text.as_bytes().as_chunks::<2>();
                if pairs.len() > result.len() || !remainder.is_empty() {
                    return Ok(Checksum::Invalid);
                }
                let width = pairs.len();
                for (pair, target) in pairs.iter().zip(&mut result) {
                    let digit = |byte: u8| char::from(byte).to_digit(16).map(|n| n as u8);
                    let (Some(high), Some(low)) = (digit(pair[0]), digit(pair[1])) else {
                        return Ok(Checksum::Invalid);
                    };
                    *target = (high << 4) | low;
                }
                Ok(Checksum::Hex(result, width))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Checksum, E> {
                Ok(Checksum::Invalid)
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Checksum, E> {
                Ok(Checksum::Invalid)
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Checksum, E> {
                Ok(Checksum::Invalid)
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Checksum, E> {
                Ok(Checksum::Invalid)
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Checksum, E> {
                Ok(Checksum::Invalid)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Checksum, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Checksum::Invalid)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Checksum, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Checksum::Invalid)
            }
        }

        de.deserialize_any(Value)
    }
}

#[derive(Default)]
struct Checksums([Checksum; Algorithm::Unknown as usize]);

impl<'de> Deserialize<'de> for Checksums {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Fields;

        impl<'de> Visitor<'de> for Fields {
            type Value = Checksums;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an rclone Hashes object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Checksums, A::Error> {
                let mut fields = Checksums::default();
                while let Some(key) = map.next_key::<Algorithm>()? {
                    if key == Algorithm::Unknown {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                    let slot = &mut fields.0[key.index()];
                    if matches!(slot, Checksum::Missing) {
                        *slot = map.next_value()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                        *slot = Checksum::Invalid;
                    }
                }
                Ok(fields)
            }
        }

        de.deserialize_map(Fields)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    path: String,
    size: i64,
    is_dir: bool,
    #[serde(default)]
    hashes: Checksums,
}

/// Input totals. Each row belongs to exactly one of the last five categories.
#[derive(Clone, Debug, Default)]
pub struct Coverage {
    pub rows: u64,
    pub directories: u64,
    pub unknown_size: u64,
    pub missing: u64,
    pub invalid: u64,
    pub comparable: u64,
}

struct Candidate {
    size: u64,
    digest: [u8; 32],
    path_start: u32,
    path_len: u32,
    ordinal: u32,
    source: u8,
}

/// Bounded collection of rows for a single selected algorithm.
pub struct ChecksumRows {
    algorithm: Algorithm,
    candidates: Vec<Candidate>,
    paths: String,
    coverage: Vec<Coverage>,
}

/// A member of a candidate group, located in a supplied listing.
pub struct Member<'a> {
    pub source: usize,
    pub ordinal: u32,
    pub path: &'a str,
}

/// A group with equal recorded size and checksum.
pub struct Group<'a> {
    candidates: &'a [Candidate],
    paths: &'a str,
}

impl<'a> Group<'a> {
    pub fn size(&self) -> u64 {
        self.candidates[0].size
    }

    /// The members on one side, in the order they appeared in that input.
    pub fn members(&self, source: usize) -> impl Iterator<Item = Member<'a>> {
        let rows = self.candidates;
        let start = rows.partition_point(|row| usize::from(row.source) < source);
        let end = rows.partition_point(|row| usize::from(row.source) <= source);
        let paths = self.paths;
        rows[start..end].iter().map(move |row| {
            let start = row.path_start as usize;
            Member {
                source,
                ordinal: row.ordinal,
                path: &paths[start..start + row.path_len as usize],
            }
        })
    }
}

impl ChecksumRows {
    pub fn new(algorithm: Algorithm) -> Self {
        Self {
            algorithm,
            candidates: Vec::new(),
            paths: String::new(),
            coverage: Vec::new(),
        }
    }

    /// Read one input. A refused input contributes no rows.
    pub fn read(&mut self, input: impl Read) -> Result<(), ImportError> {
        if self.coverage.len() == 2 || self.algorithm == Algorithm::Unknown {
            return Err(ImportError::Malformed(
                "expected one or two listings and a supported checksum algorithm".into(),
            ));
        }
        let limits = Limits {
            max_input_bytes: MAX_INPUT_BYTES,
            ..Limits::default()
        };
        let mut opened = open_listing(input, &limits).map_err(input_limit)?;
        let old_rows = self.candidates.len();
        let old_paths = self.paths.len();
        let mut state = Listing {
            report: self,
            coverage: Coverage::default(),
            limits,
            failure: None,
        };
        let mut parser = serde_json::Deserializer::from_reader(&mut opened.reader);
        if let Err(error) = parser
            .deserialize_seq(&mut state)
            .and_then(|()| parser.end())
        {
            state.report.candidates.truncate(old_rows);
            state.report.paths.truncate(old_paths);
            return Err(state
                .failure
                .unwrap_or_else(|| input_limit(listing_json_error(error, opened.compression))));
        }
        state.report.coverage.push(state.coverage);
        Ok(())
    }

    /// Sort once before enumerating groups. Largest apparent size first.
    pub fn finish(&mut self) {
        self.candidates.sort_unstable_by(|a, b| {
            b.size
                .cmp(&a.size)
                .then_with(|| a.digest.cmp(&b.digest))
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.ordinal.cmp(&b.ordinal))
        });
    }

    pub fn coverage(&self) -> &[Coverage] {
        &self.coverage
    }

    /// Enumerate matching groups after sorting with `finish`.
    pub fn groups(&self) -> impl Iterator<Item = Group<'_>> {
        let cross_listing = self.coverage.len() == 2;
        self.candidates
            .chunk_by(|a, b| a.size == b.size && a.digest == b.digest)
            .filter(move |rows| {
                rows.len() > 1 && (!cross_listing || rows[0].source != rows[rows.len() - 1].source)
            })
            .map(|candidates| Group {
                candidates,
                paths: &self.paths,
            })
    }
}

fn input_limit(error: ImportError) -> ImportError {
    match error {
        ImportError::InputTooLarge(_) | ImportError::DecompressedTooLarge(_) => {
            ImportError::Malformed(
                "checksum input exceeds the 256 MiB raw or decoded limit per listing".into(),
            )
        }
        other => other,
    }
}

struct Listing<'a> {
    report: &'a mut ChecksumRows,
    coverage: Coverage,
    limits: Limits,
    failure: Option<ImportError>,
}

impl Listing<'_> {
    fn add(&mut self, item: Item) -> Result<(), ImportError> {
        self.coverage.rows += 1;
        let row = self.coverage.rows;
        if row > self.limits.max_entries {
            return Err(ImportError::TooManyEntries(self.limits.max_entries));
        }
        let algorithm = self.report.algorithm;
        let digest = match (item.is_dir, item.size, item.hashes.0[algorithm.index()]) {
            (true, _, _) => {
                self.coverage.directories += 1;
                return Ok(());
            }
            (false, -1, _) => {
                self.coverage.unknown_size += 1;
                return Ok(());
            }
            (false, ..) if item.size < -1 => {
                return Err(ImportError::Entry {
                    entry: row,
                    reason: "Size is less than -1".into(),
                });
            }
            (false, _, Checksum::Missing) => {
                self.coverage.missing += 1;
                return Ok(());
            }
            (false, _, Checksum::Hex(digest, width)) if width == algorithm.width() => digest,
            _ => {
                self.coverage.invalid += 1;
                return Ok(());
            }
        };
        if item.path.len() > self.limits.max_meta_bytes || item.path.contains('\0') {
            return Err(ImportError::Entry {
                entry: row,
                reason: "invalid listing path or path exceeds 4096 bytes".into(),
            });
        }
        let report = &mut self.report;
        if report.candidates.len() >= MAX_ROWS {
            return Err(ImportError::Malformed(
                "more than 1,000,000 eligible checksum rows across the inputs".into(),
            ));
        }
        if report.paths.len() + item.path.len() > MAX_PATH_BYTES {
            return Err(ImportError::Malformed(
                "checksum paths exceed the 64 MiB limit across the inputs".into(),
            ));
        }
        report.candidates.push(Candidate {
            size: item.size as u64,
            digest,
            path_start: report.paths.len() as u32,
            path_len: item.path.len() as u32,
            ordinal: row as u32,
            source: report.coverage.len() as u8,
        });
        report.paths.push_str(&item.path);
        self.coverage.comparable += 1;
        Ok(())
    }
}

impl<'de> Visitor<'de> for &mut Listing<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of rclone lsjson entries")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(item) = seq.next_element()? {
            if let Err(error) = self.add(item) {
                self.failure = Some(error);
                return Err(de::Error::custom("checksum input stopped"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_listing_requires_both_sides_and_equal_sizes() {
        let lower = "ab".repeat(16);
        let upper = lower.to_ascii_uppercase();
        let item = |path, size, digest: &str| {
            format!(
                r#"{{"Path":"{path}","Size":{size},"IsDir":false,"Hashes":{{"md5":"{digest}"}}}}"#
            )
        };
        let left = [
            item("same", 12, &upper),
            item("same", 12, &lower),
            item("size", 99, &lower),
            item("exclusive", 99, &lower),
        ];
        let right = [item("match", 12, &lower), item("different", 13, &lower)];
        let mut report = ChecksumRows::new(Algorithm::Md5);
        for rows in [&left[..], &right] {
            report
                .read(format!("[{}]", rows.join(",")).as_bytes())
                .unwrap();
        }
        report.finish();
        let groups: Vec<_> = report.groups().collect();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size(), 12);
        assert_eq!(
            groups[0]
                .members(0)
                .map(|row| row.ordinal)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(groups[0].members(1).next().unwrap().path, "match");
    }

    #[test]
    fn distinguishes_no_coverage_from_malformed_selected_fields() {
        let good = "12".repeat(16);
        let input = format!(
            r#"[
            {{"Path":"folder","Size":-1,"IsDir":true}},
            {{"Path":"unknown","Size":-1,"IsDir":false,"Hashes":{{"md5":"{good}"}}}},
            {{"Path":"missing","Size":5,"IsDir":false,"Hashes":{{"custom":[5]}}}},
            {{"Path":"duplicate","Size":5,"IsDir":false,"Hashes":{{"md5":"{good}","md5":"{good}"}}}},
            {{"Path":"malformed","Size":5,"IsDir":false,"Hashes":{{"md5":{{"bad":5}}}}}},
            {{"Path":"short","Size":5,"IsDir":false,"Hashes":{{"md5":"1234"}}}}
        ]"#
        );
        let mut report = ChecksumRows::new(Algorithm::Md5);
        report.read(input.as_bytes()).unwrap();
        report.finish();
        let coverage = &report.coverage()[0];
        assert_eq!(
            (coverage.rows, coverage.directories, coverage.unknown_size),
            (6, 1, 1)
        );
        assert_eq!(
            (coverage.missing, coverage.invalid, coverage.comparable),
            (1, 3, 0)
        );
        assert!(report.groups().next().is_none());
    }

    #[test]
    fn supported_widths_and_failed_read_rollback() {
        for (name, width) in [
            ("md5", 32),
            ("sha1", 40),
            ("quickxor", 40),
            ("sha256", 64),
            ("dropbox", 64),
        ] {
            let digest = "a".repeat(width);
            let row = format!(
                r#"{{"Path":"x","Size":1,"IsDir":false,"Hashes":{{"{name}":"{digest}"}}}}"#
            );
            let mut report = ChecksumRows::new(name.parse().unwrap());
            assert!(report.read(format!("[{row},").as_bytes()).is_err());
            assert!(report.coverage().is_empty());
            report.read(format!("[{row},{row}]").as_bytes()).unwrap();
            report.finish();
            let group = report.groups().next().unwrap();
            assert_eq!(group.members(0).count(), 2);
        }
    }
}
