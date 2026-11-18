// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Where a non-resident attribute's bytes actually are.
//!
//! NTFS stores the extent map of a non-resident attribute as a *run list*: a
//! sequence of variable-length entries, each a length in clusters and an offset
//! from the previous run's start. The encoding is deliberately tiny — a
//! contiguous 4 GiB file is four bytes of run list — and it is the only thing
//! standing between "the MFT says `$MFT` is 900 MB" and being able to read it.
//!
//! We decode exactly one run list in anger: `$MFT`'s own `$DATA`. Every other
//! file's size comes out of the attribute header without the extents mattering,
//! which is the whole reason this scan is cheap.

/// One contiguous extent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    /// First cluster of this run on the volume. `None` for a sparse run, which
    /// occupies no space and reads as zeros.
    pub start: Option<u64>,
    pub clusters: u64,
}

/// Decode a run list.
///
/// `None` when the bytes run out mid-entry or a field claims a width the
/// remaining buffer cannot hold — both of which mean the record is not what it
/// says it is, and the honest response is to stop reading the volume.
///
/// Each entry begins with a header byte: the low nibble is how many bytes the
/// *length* occupies, the high nibble how many the *offset* occupies. A zero
/// header terminates the list. A zero-width offset is a sparse run.
pub fn decode(bytes: &[u8]) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut at = 0usize;
    let mut lcn: i64 = 0;

    loop {
        let header = *bytes.get(at)?;
        if header == 0 {
            return Some(runs);
        }
        at += 1;

        let length_bytes = usize::from(header & 0x0F);
        let offset_bytes = usize::from(header >> 4);
        // A length of zero cannot encode a run, and eight is the widest either
        // field can be. Both spellings appear only in a corrupt list.
        if length_bytes == 0 || length_bytes > 8 || offset_bytes > 8 {
            return None;
        }

        let clusters = unsigned(bytes.get(at..at + length_bytes)?);
        at += length_bytes;

        let start = if offset_bytes == 0 {
            None
        } else {
            // The offset is signed and relative: a run list walks backwards
            // across the volume as often as forwards.
            lcn = lcn.checked_add(signed(bytes.get(at..at + offset_bytes)?))?;
            at += offset_bytes;
            if lcn < 0 {
                return None;
            }
            Some(lcn as u64)
        };

        // A run of no clusters carries no data and no position; keeping it
        // would only invite a division or an empty read later.
        if clusters > 0 {
            runs.push(Run { start, clusters });
        }
    }
}

/// Little-endian, zero-extended.
fn unsigned(bytes: &[u8]) -> u64 {
    let mut value = 0u64;
    for (i, byte) in bytes.iter().enumerate() {
        value |= u64::from(*byte) << (8 * i);
    }
    value
}

/// Little-endian, sign-extended from whatever width it was written at.
fn signed(bytes: &[u8]) -> i64 {
    let mut value = unsigned(bytes);
    let top = bytes.len() * 8;
    if top < 64 && value & (1 << (top - 1)) != 0 {
        value |= !0u64 << top;
    }
    value as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical example from every description of the format: three runs,
    /// the third of which is *behind* the second.
    #[test]
    fn a_run_list_is_relative_and_can_go_backwards() {
        // 0x21 0x18 0x34 0x56 — 1-byte length 0x18, 2-byte offset 0x5634
        // 0x31 0x42 0x00 0x14 0x00 — 1-byte length 0x42, 3-byte offset 0x1400
        // 0x11 0x30 0x60 — 1-byte length 0x30, 1-byte offset 0x60
        let runs = decode(&[
            0x21, 0x18, 0x34, 0x56, 0x31, 0x42, 0x00, 0x14, 0x00, 0x11, 0x30, 0x60, 0x00,
        ])
        .unwrap();

        assert_eq!(
            runs,
            vec![
                Run {
                    start: Some(0x5634),
                    clusters: 0x18
                },
                Run {
                    start: Some(0x5634 + 0x1400),
                    clusters: 0x42
                },
                Run {
                    start: Some(0x5634 + 0x1400 + 0x60),
                    clusters: 0x30
                },
            ]
        );
    }

    /// A negative offset is the common case in a fragmented file, and reading
    /// it unsigned would put the extent hundreds of gigabytes away.
    #[test]
    fn a_negative_offset_moves_back_down_the_volume() {
        // Length 8 at cluster 0x20, then length 8 at 0x20 - 0x10.
        let runs = decode(&[0x11, 0x08, 0x20, 0x11, 0x08, 0xF0, 0x00]).unwrap();
        assert_eq!(runs[0].start, Some(0x20));
        assert_eq!(runs[1].start, Some(0x10));
    }

    #[test]
    fn a_sparse_run_has_a_length_and_no_place() {
        let runs = decode(&[0x01, 0x40, 0x11, 0x08, 0x20, 0x00]).unwrap();
        assert_eq!(
            runs,
            vec![
                Run {
                    start: None,
                    clusters: 0x40
                },
                // The hole does not move the cursor: the next run is still
                // relative to the last *placed* one, which is cluster zero here.
                Run {
                    start: Some(0x20),
                    clusters: 8
                },
            ]
        );
    }

    #[test]
    fn an_empty_list_is_a_list_of_nothing() {
        assert_eq!(decode(&[0x00]), Some(Vec::new()));
    }

    /// The bytes come off a raw volume, so every width in them is the disk's
    /// word rather than ours.
    #[test]
    fn a_truncated_or_impossible_list_is_refused() {
        assert!(decode(&[]).is_none(), "no terminator");
        assert!(decode(&[0x21, 0x18]).is_none(), "offset cut short");
        assert!(decode(&[0x11, 0x08]).is_none(), "offset missing");
        assert!(decode(&[0x21]).is_none(), "length missing");
        assert!(decode(&[0x10, 0x00]).is_none(), "zero-width length");
        assert!(
            decode(&[0x11, 0x08, 0xF0, 0x00]).is_none(),
            "first run before the start of the volume"
        );
    }

    /// A run of zero clusters is a hole in the encoding rather than in the
    /// file, and carrying it forward would mean an extent with no bytes.
    #[test]
    fn a_zero_length_run_is_dropped() {
        assert_eq!(decode(&[0x11, 0x00, 0x20, 0x00]), Some(Vec::new()));
    }
}
