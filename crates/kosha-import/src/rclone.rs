// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kosha_core::format::parse_rfc3339;
use kosha_core::snapshot::{Meta, Provenance};
use kosha_core::{Entry, Flags, ScanStats, Strategy, TreeBuilder};
use serde::Deserialize;
use serde::de::{self, SeqAccess, Visitor};

use crate::{ImportError, Imported, Limits, listing_json_error, open_listing};

/// Metadata supplied separately from the listing.
pub struct RcloneSource<'a> {
    pub source_name: &'a str,
    pub label: &'a str,
    pub root: &'a str,
    pub tool: &'a str,
    pub observed: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    path: String,
    name: String,
    size: i64,
    is_dir: bool,
    #[serde(default)]
    mod_time: String,
}

#[derive(Clone, Copy)]
struct Directory {
    handle: u32,
    listed: bool,
}

struct Listing<'a> {
    limits: &'a Limits,
    builder: TreeBuilder,
    directories: HashMap<u32, HashMap<String, Directory>>,
    row: u64,
    synthesized: u64,
    unknown: u64,
    mtime_seen: bool,
    failure: Option<ImportError>,
}

fn directory_entry(mtime: u32) -> Entry {
    Entry {
        logical: 0,
        allocated: 0,
        mtime,
        flags: Flags::DIR,
    }
}

impl Listing<'_> {
    fn invalid(&self, reason: impl Into<String>) -> ImportError {
        ImportError::Entry {
            entry: self.row,
            reason: reason.into(),
        }
    }

    fn push(&mut self, parent: u32, name: &str, entry: Entry) -> Result<u32, ImportError> {
        if self.builder.len() as u64 >= self.limits.max_entries {
            return Err(ImportError::TooManyEntries(self.limits.max_entries));
        }
        self.builder
            .push(parent, name, entry)
            .map_err(|error| self.invalid(error.to_string()))
    }

    fn add(&mut self, item: Item) -> Result<(), ImportError> {
        let mut components = item.path.split('/').peekable();
        let depth = item.path.bytes().filter(|&byte| byte == b'/').count();
        let limit = self.limits.max_depth.min(crate::MAX_DEPTH);
        if depth + usize::from(item.is_dir) > limit as usize {
            return Err(ImportError::TooDeep {
                entry: self.row,
                limit,
            });
        }
        if item.path.rsplit('/').next() != Some(item.name.as_str()) {
            return Err(self.invalid("Path and Name disagree"));
        }
        let size = match item.size {
            -1 => {
                self.unknown += 1;
                0
            }
            n => u64::try_from(n).map_err(|_| self.invalid("Size is less than -1"))?,
        };
        let mtime = match item.mod_time.as_str() {
            "" | "2000-01-01T00:00:00Z" => 0,
            value => {
                if value.len() > self.limits.max_meta_bytes {
                    return Err(self.invalid("ModTime exceeds the metadata limit"));
                }
                let time = parse_rfc3339(value)
                    .ok_or_else(|| self.invalid("ModTime is not an RFC 3339 timestamp"))?;
                u32::try_from(time).unwrap_or_default()
            }
        };
        self.mtime_seen |= mtime != 0;
        let mut parent = 0;
        while let Some(name) = components.next() {
            if let Some(reason) = crate::ncdu::name_problem(name) {
                return Err(self.invalid(reason));
            }
            if name.len() > self.limits.max_name_bytes {
                return Err(self.invalid("a Path component exceeds the name limit"));
            }
            let ancestor = components.peek().is_some();
            if !ancestor && !item.is_dir {
                self.push(
                    parent,
                    name,
                    Entry {
                        logical: size,
                        allocated: size,
                        mtime,
                        flags: Flags::default(),
                    },
                )?;
                break;
            }
            let existing = self
                .directories
                .get(&parent)
                .and_then(|children| children.get(name))
                .copied();
            if let Some(directory) = existing {
                if ancestor {
                    parent = directory.handle;
                } else if directory.listed {
                    self.push(parent, name, directory_entry(mtime))?;
                } else {
                    if !self.builder.set_mtime(directory.handle, mtime) {
                        return Err(ImportError::Internal);
                    }
                    self.directories
                        .get_mut(&parent)
                        .unwrap()
                        .get_mut(name)
                        .unwrap()
                        .listed = true;
                    self.synthesized -= 1;
                }
            } else {
                let handle = self.push(
                    parent,
                    name,
                    directory_entry(if ancestor { 0 } else { mtime }),
                )?;
                self.directories.entry(parent).or_default().insert(
                    name.into(),
                    Directory {
                        handle,
                        listed: !ancestor,
                    },
                );
                self.synthesized += u64::from(ancestor);
                parent = handle;
            }
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for &mut Listing<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of rclone lsjson entries")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(item) = seq.next_element::<Item>()? {
            self.row += 1;
            if let Err(error) = self.add(item) {
                self.failure = Some(error);
                return Err(de::Error::custom("import stopped"));
            }
        }
        Ok(())
    }
}

/// Read the JSON array produced by `rclone lsjson -R`.
pub fn import_rclone<R: Read>(
    input: R,
    source: &RcloneSource<'_>,
    now: SystemTime,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    import_opened(open_listing(input, limits)?, source, now, limits)
}

pub(crate) fn import_opened(
    mut opened: crate::decompress::Opened<'_>,
    source: &RcloneSource<'_>,
    now: SystemTime,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    if limits.max_entries == 0 {
        return Err(ImportError::TooManyEntries(0));
    }
    for metadata in [source.root, source.tool, source.label, source.source_name] {
        if metadata.len() > limits.max_meta_bytes || metadata.contains('\0') {
            return Err(ImportError::Malformed("invalid listing metadata".into()));
        }
    }
    let root = if source.root.is_empty() {
        "rclone listing"
    } else {
        source.root
    };
    let mut listing = Listing {
        limits,
        builder: TreeBuilder::new(root, directory_entry(0))
            .map_err(|error| ImportError::Malformed(error.to_string()))?,
        directories: HashMap::new(),
        row: 0,
        synthesized: 0,
        unknown: 0,
        mtime_seen: false,
        failure: None,
    };
    let mut deserializer = serde_json::Deserializer::from_reader(&mut opened.reader);
    if let Err(error) = de::Deserializer::deserialize_seq(&mut deserializer, &mut listing)
        .and_then(|()| deserializer.end())
    {
        return Err(listing
            .failure
            .unwrap_or_else(|| listing_json_error(error, opened.compression)));
    }
    drop(listing.directories);
    let (tree, layout) = listing
        .builder
        .finish(
            PathBuf::from(root),
            ScanStats {
                strategy: Strategy::Imported,
                ..ScanStats::default()
            },
        )
        .map_err(|error| ImportError::Malformed(error.to_string()))?;
    let taken = source
        .observed
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .unwrap_or(now);
    let mut meta = Meta::of(&tree, taken, source.label);
    meta.exact_allocated = None;
    meta.duration_ms = 0;
    meta.import = Some(Provenance {
        format: "rclone-lsjson".into(),
        tool: source.tool.into(),
        observed: source.observed,
        observed_basis: if source.observed.is_some() {
            "user-supplied"
        } else {
            "unknown"
        }
        .into(),
        imported_at: now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        source_name: source.source_name.into(),
        mtime_known: listing.mtime_seen,
        unknown_size: listing.unknown,
        synthesized_dirs: listing.synthesized,
        duplicate_paths: layout.duplicate_names,
        ..Provenance::default()
    });
    Ok(Imported {
        tree,
        meta,
        compression: opened.compression,
        bytes_read: opened.input_count.get(),
        bytes_decoded: opened.output_count.get(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str, limits: Limits) -> Result<Imported, ImportError> {
        import_rclone(
            json.as_bytes(),
            &RcloneSource {
                source_name: "fixture.json",
                label: "Fixture",
                root: "drive:",
                tool: "",
                observed: None,
            },
            UNIX_EPOCH,
            &limits,
        )
    }

    #[test]
    fn imports_a_real_local_rclone_listing_with_scan_totals() {
        let json = include_str!("../tests/fixtures/rclone-local.json");
        let imported = read(json, Limits::default()).unwrap();
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("sub")).unwrap();
        std::fs::create_dir(fixture.path().join("empty")).unwrap();
        std::fs::write(fixture.path().join("one.txt"), b"hello").unwrap();
        std::fs::write(fixture.path().join("sub/two.bin"), b"12345678").unwrap();
        let walked = kosha_core::Scanner::new(kosha_core::ScanOptions::default())
            .scan(fixture.path())
            .unwrap();
        assert_eq!(imported.tree.total_logical(), walked.total_logical());
        assert_eq!(imported.tree.stats().entries, walked.stats().entries);
        assert_eq!(imported.tree.stats().dirs, walked.stats().dirs);
        assert!(imported.meta.import.unwrap().mtime_known);
    }

    #[test]
    fn small_mutations_of_real_output_do_not_panic() {
        let fixture = include_bytes!("../tests/fixtures/rclone-local.json");
        let source = RcloneSource {
            source_name: "fixture.json",
            label: "",
            root: "",
            tool: "",
            observed: None,
        };
        let mut bytes = fixture.to_vec();
        for index in (0..fixture.len()).step_by(7) {
            bytes[index] ^= 0x80;
            let _ = import_rclone(bytes.as_slice(), &source, UNIX_EPOCH, &Limits::default());
            bytes[index] = fixture[index];
        }
    }

    #[test]
    fn retains_duplicates_and_reconciles_late_parents() {
        let imported = read(
            r#"[
                {"Path":"a/one","Name":"one","Size":12,"IsDir":false},
                {"Path":"a/one","Name":"one","Size":-1,"IsDir":false},
                {"Path":"a","Name":"a","Size":-1,"IsDir":true,"ModTime":"2026-01-01T00:00:00.1Z"},
                {"Path":"b/two","Name":"two","Size":8,"IsDir":false,"ModTime":"2000-01-01T00:00:00Z"}
            ]"#,
            Limits::default(),
        ).unwrap();
        let provenance = imported.meta.import.unwrap();
        assert_eq!(provenance.format, "rclone-lsjson");
        assert_eq!(provenance.synthesized_dirs, 1);
        assert_eq!(provenance.duplicate_paths, 1);
        assert_eq!(provenance.unknown_size, 2);
        assert!(!provenance.allocated_known);
        assert!(provenance.mtime_known);
        assert_eq!(imported.tree.total_logical(), 20);
        assert_eq!(imported.tree.total_allocated(), 20);
    }

    #[test]
    fn enforces_path_and_entry_limits() {
        let traversal = r#"[{"Path":"../one","Name":"one","Size":12,"IsDir":false}]"#;
        assert!(matches!(
            read(traversal, Limits::default()),
            Err(ImportError::Entry { .. })
        ));
        let nested = r#"[{"Path":"a/one","Name":"one","Size":12,"IsDir":false}]"#;
        assert!(matches!(
            read(
                nested,
                Limits {
                    max_entries: 2,
                    ..Limits::default()
                }
            ),
            Err(ImportError::TooManyEntries(2))
        ));
        assert!(matches!(
            read("[", Limits::default()),
            Err(ImportError::Truncated(_))
        ));
    }
}
