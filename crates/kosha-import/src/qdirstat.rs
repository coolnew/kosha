// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kosha_core::snapshot::{Meta, Provenance};
use kosha_core::{Entry, Flags, ScanStats, Strategy, TreeBuilder};

use crate::decompress::Opened;
use crate::{ImportError, Imported, Limits, io_error};

const MAX_LINE: usize = 1024;

fn invalid(line: u64, reason: impl Into<String>) -> ImportError {
    ImportError::Entry {
        entry: line,
        reason: reason.into(),
    }
}

fn line<'a>(
    opened: &mut Opened<'_>,
    bytes: &'a mut Vec<u8>,
    number: u64,
) -> Result<Option<&'a [u8]>, ImportError> {
    bytes.clear();
    (&mut opened.reader)
        .take((MAX_LINE + 2) as u64)
        .read_until(b'\n', bytes)
        .map_err(|error| io_error(error, opened.compression))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let text = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let text = text.strip_suffix(b"\r").unwrap_or(text);
    if text.len() > MAX_LINE {
        return Err(invalid(number, "QDirStat line exceeds 1024 bytes"));
    }
    Ok(Some(text))
}

fn number(bytes: &[u8], radix: u32) -> Option<u64> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.is_empty() || !text.chars().all(|ch| ch.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(text, radix).ok()
}

fn size(bytes: &[u8]) -> Option<u64> {
    let (digits, factor) = match bytes.split_last()? {
        (b'K', digits) => (digits, 1 << 10),
        (b'M', digits) => (digits, 1 << 20),
        (b'G', digits) => (digits, 1 << 30),
        _ => (bytes, 1),
    };
    number(digits, 10)?.checked_mul(factor)
}

fn decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&first, tail)) = rest.split_first() {
        rest = tail;
        if first == b'%' {
            let (hex, tail) = rest.split_at_checked(2)?;
            let byte = number(hex, 16)? as u8;
            if byte == b'/' || byte == 0 {
                return None;
            }
            out.push(byte);
            rest = tail;
        } else {
            out.push(first);
        }
    }
    Some(out)
}

fn native(bytes: &[u8], lossy: &mut u64) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let _ = lossy;
        OsStr::from_bytes(bytes).to_owned()
    }
    #[cfg(not(unix))]
    {
        let name = String::from_utf8_lossy(bytes);
        *lossy += u64::from(matches!(name, std::borrow::Cow::Owned(_)));
        OsStr::new(name.as_ref()).to_owned()
    }
}

struct Record {
    path: Vec<u8>,
    entry: Entry,
}

fn record(text: &[u8], v2: bool, row: u64) -> Result<Record, ImportError> {
    let bad = |reason| invalid(row, reason);
    let mut fields = text
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    let kind = fields.next().unwrap_or_default().to_ascii_lowercase();
    let mut flags = match kind.as_slice() {
        b"d" => Flags::DIR,
        b"l" => Flags::SYMLINK,
        b"f" | b"blockdev" | b"chardev" | b"fifo" | b"socket" => Flags::default(),
        _ => return Err(bad("unknown QDirStat entry type")),
    };
    let path = fields
        .next()
        .and_then(decode)
        .filter(|path| !path.is_empty() && !path.contains(&0))
        .ok_or_else(|| bad("invalid encoded path"))?;
    let logical = fields
        .next()
        .and_then(size)
        .ok_or_else(|| bad("invalid size"))?;
    if v2 {
        for radix in [10, 10, 8] {
            if fields
                .next()
                .and_then(|value| number(value, radix))
                .is_none()
            {
                return Err(bad("invalid QDirStat owner or mode field"));
            }
        }
    }
    let timestamp = fields.next().ok_or_else(|| bad("missing mtime"))?;
    let timestamp = if let Some(hex) = timestamp.strip_prefix(b"0x") {
        number(hex, 16)
    } else if let Some(digits) = timestamp.strip_prefix(b"-") {
        number(digits, 10).map(|_| 0)
    } else {
        number(timestamp, 10)
    };
    let mtime = timestamp
        .ok_or_else(|| bad("invalid mtime"))?
        .min(u32::MAX as u64) as u32;
    let mut blocks = None;
    let mut links = None;
    while let Some(key) = fields.next() {
        let slot = if key.eq_ignore_ascii_case(b"blocks:") {
            &mut blocks
        } else if key.eq_ignore_ascii_case(b"links:") {
            &mut links
        } else {
            return Err(bad("unknown QDirStat optional field"));
        };
        let value = fields.next().and_then(|value| number(value, 10));
        if slot.is_some() || value.is_none() {
            return Err(bad("invalid or repeated QDirStat optional field"));
        }
        *slot = value;
    }
    if let Some(blocks) = blocks {
        let allocated = blocks
            .checked_mul(512)
            .ok_or_else(|| bad("block count overflows"))?;
        if allocated < logical && kind == b"f" {
            flags.insert(Flags::SPARSE);
        }
    }
    // The tree uses apparent sizes throughout because most allocations are absent.
    let allocated = if flags.contains(Flags::DIR) {
        0
    } else {
        logical
    };
    Ok(Record {
        path,
        entry: Entry {
            logical,
            allocated,
            mtime,
            flags,
        },
    })
}

fn absolute(path: &[u8], limits: &Limits, row: u64) -> Result<(), ImportError> {
    let components = path
        .strip_prefix(b"/")
        .ok_or_else(|| invalid(row, "expected an absolute path"))?;
    if !components.is_empty() {
        for name in components.split(|&byte| byte == b'/') {
            if name.len() > limits.max_name_bytes
                || !kosha_core::builder::is_valid_component(&String::from_utf8_lossy(name))
            {
                return Err(invalid(row, "invalid absolute path component"));
            }
        }
    }
    Ok(())
}

pub(crate) fn import(
    mut opened: Opened<'_>,
    source_name: &str,
    label: &str,
    now: SystemTime,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    for metadata in [source_name, label] {
        if metadata.len() > limits.max_meta_bytes || metadata.contains('\0') {
            return Err(invalid(1, "invalid listing metadata"));
        }
    }
    let mut bytes = Vec::with_capacity(MAX_LINE + 2);
    let version = match line(&mut opened, &mut bytes, 1)? {
        Some(b"[qdirstat 1.0 cache file]") => "1.0",
        Some(b"[qdirstat 2.0 cache file]") => "2.0",
        None => return Err(ImportError::Empty),
        _ => return Err(invalid(1, "unrecognized QDirStat cache header")),
    };
    let mut next = |row: &mut u64| loop {
        *row += 1;
        let Some(text) = line(&mut opened, &mut bytes, *row)? else {
            return Ok(None);
        };
        let text = text.trim_ascii_start();
        if !text.is_empty() && !text.starts_with(b"#") {
            return record(text, version == "2.0", *row).map(Some);
        }
    };
    let mut row = 1;
    let root = next(&mut row)?.ok_or_else(|| invalid(row, "missing root directory"))?;
    if !root.entry.flags.contains(Flags::DIR) {
        return Err(invalid(row, "first entry must be a directory"));
    }
    absolute(&root.path, limits, row)?;
    if limits.max_entries == 0 {
        return Err(ImportError::TooManyEntries(0));
    }
    let mut lossy = 0;
    let root_path = PathBuf::from(native(&root.path, &mut lossy));
    let mut builder = TreeBuilder::new(&root_path.to_string_lossy(), root.entry)
        .map_err(|error| invalid(row, error.to_string()))?;
    let mut directories = HashMap::from([(root.path.clone(), (0, 0u32))]);
    let mut current = root.path;
    let depth_limit = limits.max_depth.min(crate::MAX_DEPTH);
    while let Some(item) = next(&mut row)? {
        if builder.len() as u64 >= limits.max_entries {
            return Err(ImportError::TooManyEntries(limits.max_entries));
        }
        let is_dir = item.entry.flags.contains(Flags::DIR);
        let (parent_path, name) = if item.path.starts_with(b"/") {
            absolute(&item.path, limits, row)?;
            if item.path == b"/" {
                return Err(invalid(row, "repeated root directory"));
            }
            let split = item.path.iter().rposition(|&byte| byte == b'/').unwrap();
            (&item.path[..split.max(1)], &item.path[split + 1..])
        } else {
            if is_dir {
                return Err(invalid(row, "directory path must be absolute"));
            }
            (current.as_slice(), item.path.as_slice())
        };
        let &(parent, depth) = directories.get(parent_path).ok_or_else(|| {
            invalid(
                row,
                "parent directory is outside the root or has not appeared",
            )
        })?;
        if depth + u32::from(is_dir) > depth_limit {
            return Err(ImportError::TooDeep {
                entry: row,
                limit: depth_limit,
            });
        }
        if name.len() > limits.max_name_bytes {
            return Err(invalid(row, "path component exceeds the name limit"));
        }
        let handle = builder
            .push_os(parent, &native(name, &mut lossy), item.entry)
            .map_err(|error| invalid(row, error.to_string()))?;
        if is_dir {
            current = item.path.clone();
            if directories.insert(item.path, (handle, depth + 1)).is_some() {
                return Err(invalid(row, "duplicate directory path"));
            }
        } else if item.path.starts_with(b"/") {
            current = parent_path.to_vec();
        }
    }
    let (tree, layout) = builder
        .finish(
            root_path,
            ScanStats {
                strategy: Strategy::Imported,
                ..ScanStats::default()
            },
        )
        .map_err(|error| invalid(row, error.to_string()))?;
    if layout.duplicate_names != 0 {
        return Err(invalid(row, "duplicate path in QDirStat cache"));
    }
    let mut meta = Meta::of(&tree, now, label);
    meta.exact_allocated = None;
    meta.import = Some(Provenance {
        format: "qdirstat-cache".into(),
        format_version: version.into(),
        observed_basis: "unknown".into(),
        imported_at: now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        source_name: source_name.into(),
        mtime_known: true,
        lossy_names: lossy,
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
    use std::io::Write;

    const CACHE: &[u8] = b"[qdirstat 2.0 cache file]\n\n# comment\nD /fixture 40 1000 1000 0755 0x6553f100\nF a%20b 2K 1000 1000 0644 1 Blocks: 1 Links: 2\nL /fixture/sym 3 1000 1000 0777 2\nF bad-%ff 11 1000 1000 0644 3\n";

    #[test]
    fn imports_gzip_through_auto_detection() {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(CACHE).unwrap();
        let gzip = gzip.finish().unwrap();
        let source = crate::RcloneSource {
            source_name: "fixture.cache.gz",
            label: "Fixture",
            root: "",
            tool: "",
            observed: None,
        };
        let imported =
            crate::import_auto(gzip.as_slice(), &source, UNIX_EPOCH, &Limits::default()).unwrap();
        assert_eq!(imported.compression, crate::Compression::Gzip);
        assert_eq!(imported.tree.total_logical(), 2062);
        let origin = imported.meta.import.unwrap();
        assert_eq!(origin.format, "qdirstat-cache");
        assert_eq!(origin.format_version, "2.0");
        assert!(!origin.allocated_known);
        assert_eq!(origin.lossy_names, u64::from(!cfg!(unix)));
    }

    #[test]
    fn refuses_paths_and_inputs_outside_the_limits() {
        for record in [
            "F /outside/file 1 1",
            "F %2e%2e 1 1",
            "F bad%2fpath 1 1",
            "F bad%00path 1 1",
            "F invalid 18446744073709551615G 1",
            "F file 1 1\nF file 2 2",
        ] {
            let cache = format!("[qdirstat 1.0 cache file]\nD /fixture 0 0\n{record}\n");
            assert!(
                crate::import_qdirstat(
                    cache.as_bytes(),
                    "fixture",
                    "",
                    UNIX_EPOCH,
                    &Limits::default()
                )
                .is_err(),
                "{record}"
            );
        }
        let mut long = CACHE.to_vec();
        long.extend(std::iter::repeat_n(b' ', MAX_LINE + 1));
        assert!(
            crate::import_qdirstat(
                long.as_slice(),
                "fixture",
                "",
                UNIX_EPOCH,
                &Limits::default()
            )
            .is_err()
        );
    }
}
