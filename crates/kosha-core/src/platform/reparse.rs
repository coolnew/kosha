// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Telling a mounted volume from a junction.
//!
//! Both are reparse points and both carry the *same* tag,
//! `IO_REPARSE_TAG_MOUNT_POINT`. They differ only in what they point at:
//! `\??\Volume{GUID}\` is a mounted volume, `\??\C:\some\path` is a junction.
//! There is no flag for it and no cheaper question to ask.
//!
//! That distinction is worth a query twice over. Both must be refused by
//! default, because following either can loop and both cross into storage the
//! scan was not pointed at — but only one of them is a *filesystem boundary*,
//! and telling somebody their mounted 2 TB drive is "a link" is a different and
//! worse answer than telling them it is another filesystem.
//!
//! Two callers, reaching the same bytes by very different routes: the ordinary
//! walk asks `FSCTL_GET_REPARSE_POINT` for them, and the MFT reader finds the
//! identical structure sitting in the record's `$REPARSE_POINT` attribute with
//! nothing to ask at all. Compiled on every platform so the parsing stays under
//! test wherever it is being edited.

/// The tag a junction and a mounted volume share.
pub const MOUNT_POINT_TAG: u32 = 0xA000_0003;

/// Does this reparse buffer point at a volume rather than a directory?
///
/// Fed bytes so the parsing can be tested without a mounted volume, which needs
/// an elevated `mountvol` and a spare partition. The layout is
/// `REPARSE_DATA_BUFFER`: a `u32` tag, two `u16` lengths, then for the mount
/// point flavour four `u16` offsets and lengths into a UTF-16 buffer.
///
/// ```text
///  0  ReparseTag              u32
///  4  ReparseDataLength       u16
///  6  Reserved                u16
///  8  SubstituteNameOffset    u16   \
/// 10  SubstituteNameLength    u16    |  bytes, into PathBuffer
/// 12  PrintNameOffset         u16    |
/// 14  PrintNameLength         u16   /
/// 16  PathBuffer              u16[]
/// ```
pub fn targets_a_volume(buffer: &[u8], want_tag: u32) -> bool {
    const HEADER: usize = 16;
    if buffer.len() < HEADER {
        return false;
    }
    let u32_at =
        |i: usize| u32::from_le_bytes([buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]]);
    let u16_at = |i: usize| u16::from_le_bytes([buffer[i], buffer[i + 1]]) as usize;

    if u32_at(0) != want_tag {
        return false;
    }
    let offset = u16_at(8);
    let length = u16_at(10);
    let (Some(start), Some(end)) = (
        HEADER.checked_add(offset),
        HEADER
            .checked_add(offset)
            .and_then(|s| s.checked_add(length)),
    ) else {
        return false;
    };
    if end > buffer.len() || length % 2 != 0 {
        return false;
    }

    let units: Vec<u16> = buffer[start..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let name = String::from_utf16_lossy(&units);

    // `\??\Volume{...}` is a volume; `\??\C:\path` is a junction. Compared
    // case-insensitively because the prefix is written by whichever tool
    // created the mount point, and `mountvol` and the Disk Management UI do not
    // agree on the case of the GUID.
    name.to_ascii_lowercase().starts_with("\\??\\volume{")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a `REPARSE_DATA_BUFFER` for the mount-point tag around a
    /// substitute name.
    ///
    /// The parsing is fed bytes rather than a real reparse point because
    /// creating one of *either* kind is awkward and creating the interesting
    /// kind — a mounted volume — needs an elevated `mountvol` and a spare
    /// partition. The layout is fixed and documented, so a synthetic buffer
    /// exercises exactly what the live one would.
    pub(crate) fn buffer(tag: u32, substitute: &str) -> Vec<u8> {
        let units: Vec<u16> = substitute.encode_utf16().collect();
        let name: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();

        let mut out = Vec::new();
        out.extend_from_slice(&tag.to_le_bytes());
        // ReparseDataLength: the eight bytes of offsets plus the name.
        out.extend_from_slice(&((8 + name.len()) as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        out.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        out.extend_from_slice(&(name.len() as u16).to_le_bytes()); // SubstituteNameLength
        out.extend_from_slice(&(name.len() as u16).to_le_bytes()); // PrintNameOffset
        out.extend_from_slice(&0u16.to_le_bytes()); // PrintNameLength
        out.extend_from_slice(&name);
        out
    }

    /// The distinction the whole feature rests on. Both are the same tag, and
    /// only the target says which is which.
    #[test]
    fn a_mounted_volume_is_told_apart_from_a_junction() {
        let volume = buffer(
            MOUNT_POINT_TAG,
            r"\??\Volume{7f2d3c14-0000-0000-0000-100000000000}\",
        );
        assert!(targets_a_volume(&volume, MOUNT_POINT_TAG));

        for junction in [r"\??\C:\some\directory", r"\??\D:\", r"\??\J:\git"] {
            assert!(
                !targets_a_volume(&buffer(MOUNT_POINT_TAG, junction), MOUNT_POINT_TAG),
                "`{junction}` is a junction and was read as a mounted volume"
            );
        }
    }

    /// `mountvol` and the Disk Management UI do not agree on the case of the
    /// GUID, and neither is wrong.
    #[test]
    fn the_volume_prefix_is_matched_without_regard_to_case() {
        for spelling in [
            r"\??\Volume{ABCDEF01-0000-0000-0000-000000000000}\",
            r"\??\VOLUME{abcdef01-0000-0000-0000-000000000000}\",
            r"\??\volume{AbCdEf01-0000-0000-0000-000000000000}\",
        ] {
            assert!(
                targets_a_volume(&buffer(MOUNT_POINT_TAG, spelling), MOUNT_POINT_TAG),
                "`{spelling}` was not recognised"
            );
        }
    }

    /// A symlink is a different tag entirely and must not be read with the
    /// mount-point layout, whose offsets land in different places.
    #[test]
    fn another_tag_is_refused_whatever_it_points_at() {
        const SYMLINK_TAG: u32 = 0xA000_000C;
        let looks_right = buffer(SYMLINK_TAG, r"\??\Volume{7f2d3c14-0000-0000-0000-1}\");
        assert!(!targets_a_volume(&looks_right, MOUNT_POINT_TAG));
    }

    /// The buffer comes from a driver — or, for the MFT reader, straight off
    /// the disk — and the lengths in it are its word rather than ours. Every
    /// one of these would be an out-of-bounds slice if the arithmetic were done
    /// without checking.
    #[test]
    fn a_malformed_buffer_is_refused_rather_than_indexed_into() {
        let good = buffer(MOUNT_POINT_TAG, r"\??\Volume{1}\");

        assert!(!targets_a_volume(&[], MOUNT_POINT_TAG), "empty");
        assert!(!targets_a_volume(&good[..8], MOUNT_POINT_TAG), "truncated");

        // A length running past the end of the buffer.
        let mut overrun = good.clone();
        overrun[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(!targets_a_volume(&overrun, MOUNT_POINT_TAG), "long length");

        // An offset running past the end.
        let mut far = good.clone();
        far[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(!targets_a_volume(&far, MOUNT_POINT_TAG), "far offset");

        // An odd length cannot be UTF-16 and is not half a character.
        let mut odd = good.clone();
        odd[10..12].copy_from_slice(&7u16.to_le_bytes());
        assert!(!targets_a_volume(&odd, MOUNT_POINT_TAG), "odd length");
    }

    #[test]
    fn an_empty_target_is_not_a_volume() {
        assert!(!targets_a_volume(
            &buffer(MOUNT_POINT_TAG, ""),
            MOUNT_POINT_TAG
        ));
    }
}
