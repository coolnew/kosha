// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The first 512 bytes of an NTFS volume.
//!
//! Everything the MFT reader needs to find anything is here: how big a cluster
//! is, where `$MFT` starts, and how big one of its records is. The structure is
//! the BIOS parameter block plus NTFS' own extensions, and it has not changed
//! since NT 3.1.
//!
//! Parsed from a byte slice rather than from a handle so it is testable on any
//! platform, which is the same reason `platform::wsl` and
//! `platform::reparse` are compiled everywhere.

/// What the volume's geometry says about where things are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    /// Also the alignment every unbuffered read has to honour.
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u64,
    /// Cluster number where `$MFT`'s first record sits.
    pub mft_cluster: u64,
    /// Size of one MFT record. 1024 on every volume anybody has measured, and
    /// read rather than assumed because the field exists.
    pub bytes_per_record: u32,
    /// Total clusters in the volume, for bounds-checking a decoded run list.
    pub total_clusters: u64,
}

impl Geometry {
    /// Byte offset of a cluster from the start of the volume.
    #[inline]
    pub fn offset_of(&self, cluster: u64) -> Option<u64> {
        cluster.checked_mul(self.bytes_per_cluster)
    }
}

/// Fields we read, at the offsets the format fixes them at.
///
/// ```text
///  0x03  8   OEM name, "NTFS    "
///  0x0B  2   bytes per sector
///  0x0D  1   sectors per cluster
///  0x28  8   total sectors
///  0x30  8   $MFT start, in clusters
///  0x40  1   clusters per MFT record
/// ```
const OEM: usize = 0x03;
const BYTES_PER_SECTOR: usize = 0x0B;
const SECTORS_PER_CLUSTER: usize = 0x0D;
const TOTAL_SECTORS: usize = 0x28;
const MFT_CLUSTER: usize = 0x30;
const CLUSTERS_PER_RECORD: usize = 0x40;

/// Read the geometry, or refuse.
///
/// `None` for anything that is not NTFS and for anything that claims to be and
/// then contradicts itself. Every number here is multiplied by something later,
/// so a nonsense value is not a cosmetic problem: it is an offset into a raw
/// volume. The rule throughout is that a field we do not believe means we do not
/// read the volume at all, and the ordinary directory walk runs instead.
pub fn parse(sector: &[u8]) -> Option<Geometry> {
    if sector.len() < 512 || &sector[OEM..OEM + 8] != b"NTFS    " {
        return None;
    }

    let bytes_per_sector = u32::from(u16::from_le_bytes([
        sector[BYTES_PER_SECTOR],
        sector[BYTES_PER_SECTOR + 1],
    ]));
    if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        return None;
    }

    let bytes_per_cluster = cluster_size(sector[SECTORS_PER_CLUSTER], bytes_per_sector)?;

    let mft_cluster = u64_at(sector, MFT_CLUSTER);
    let total_sectors = u64_at(sector, TOTAL_SECTORS);
    let total_clusters = total_sectors / (bytes_per_cluster / u64::from(bytes_per_sector));
    if mft_cluster == 0 || mft_cluster >= total_clusters {
        return None;
    }

    let bytes_per_record = record_size(sector[CLUSTERS_PER_RECORD], bytes_per_cluster)?;

    Some(Geometry {
        bytes_per_sector,
        bytes_per_cluster,
        mft_cluster,
        bytes_per_record,
        total_clusters,
    })
}

/// Bytes per cluster, from a field that is a power of two written two ways.
///
/// Ordinarily the byte is a count of *sectors*: 1, 2, 4, … 128, multiplied by
/// the sector size. Above 128 it is read as a negative `i8` and is a power-of-
/// two count of *bytes* outright, with the sector size playing no part — which
/// is how a volume formatted with clusters larger than 64 KiB describes itself.
/// Multiplying the second spelling by the sector size as well is the mistake
/// the shape of the field invites, and it overstates every cluster on such a
/// volume by a factor of 512.
///
/// The 2 MiB ceiling is twice the largest cluster Windows will format and
/// exists so the multiplications this feeds cannot run away.
fn cluster_size(raw: u8, bytes_per_sector: u32) -> Option<u64> {
    let bytes = if raw == 0 {
        return None;
    } else if raw <= 0x80 {
        if !raw.is_power_of_two() {
            return None;
        }
        u64::from(bytes_per_sector) * u64::from(raw)
    } else {
        let shift = u32::from(raw.wrapping_neg());
        if shift >= 32 {
            return None;
        }
        1u64 << shift
    };
    (bytes >= u64::from(bytes_per_sector) && bytes <= 2 * 1024 * 1024).then_some(bytes)
}

/// Size of one MFT record, from the same two-spellings field.
///
/// Positive is a count of clusters; negative is a power-of-two byte count,
/// which is what every volume with clusters bigger than 1 KiB actually uses —
/// a record is 1024 bytes almost everywhere regardless of the cluster size.
fn record_size(raw: u8, bytes_per_cluster: u64) -> Option<u32> {
    let bytes = if raw == 0 {
        return None;
    } else if raw <= 0x80 {
        u64::from(raw) * bytes_per_cluster
    } else {
        let shift = u32::from(raw.wrapping_neg());
        if shift >= 32 {
            return None;
        }
        1u64 << shift
    };

    // A record holds a header, a fixup array and at least one attribute, and
    // the fixup arithmetic assumes it covers whole sectors. Anything outside
    // this range is a field we do not believe.
    if !(256..=64 * 1024).contains(&bytes) || !bytes.is_power_of_two() {
        return None;
    }
    Some(bytes as u32)
}

#[inline]
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An otherwise-blank boot sector with the fields this module reads.
    fn sector(
        bytes_per_sector: u16,
        sectors_per_cluster: u8,
        mft_cluster: u64,
        clusters_per_record: u8,
    ) -> Vec<u8> {
        let mut s = vec![0u8; 512];
        s[OEM..OEM + 8].copy_from_slice(b"NTFS    ");
        s[BYTES_PER_SECTOR..BYTES_PER_SECTOR + 2].copy_from_slice(&bytes_per_sector.to_le_bytes());
        s[SECTORS_PER_CLUSTER] = sectors_per_cluster;
        s[TOTAL_SECTORS..TOTAL_SECTORS + 8].copy_from_slice(&1_000_000_000u64.to_le_bytes());
        s[MFT_CLUSTER..MFT_CLUSTER + 8].copy_from_slice(&mft_cluster.to_le_bytes());
        s[CLUSTERS_PER_RECORD] = clusters_per_record;
        s
    }

    /// The volume this was written on: 512-byte sectors, 4 KiB clusters,
    /// 1 KiB records written as the negative spelling.
    #[test]
    fn an_ordinary_volume_reads_as_itself() {
        let g = parse(&sector(512, 8, 786_432, 0xF6)).unwrap();
        assert_eq!(g.bytes_per_sector, 512);
        assert_eq!(g.bytes_per_cluster, 4096);
        assert_eq!(g.mft_cluster, 786_432);
        assert_eq!(g.bytes_per_record, 1024);
        assert_eq!(g.offset_of(786_432), Some(3_221_225_472));
    }

    /// A 4Kn drive: the sector *is* the cluster, and the record is still 1 KiB.
    #[test]
    fn a_four_kilobyte_sector_is_read_as_one() {
        let g = parse(&sector(4096, 1, 12, 0xF6)).unwrap();
        assert_eq!(g.bytes_per_sector, 4096);
        assert_eq!(g.bytes_per_cluster, 4096);
        assert_eq!(g.bytes_per_record, 1024);
    }

    /// The other spelling of the cluster size: a negative byte is a count of
    /// *bytes*, and the sector size plays no part. Multiplying by it as well
    /// would report a 128 KiB cluster as 64 MiB.
    #[test]
    fn a_large_cluster_is_given_in_bytes_rather_than_sectors() {
        // 0xEF as i8 is -17, so the cluster is 2^17 = 128 KiB.
        let g = parse(&sector(512, 0xEF, 64, 0xF6)).unwrap();
        assert_eq!(g.bytes_per_cluster, 131_072);
    }

    /// The positive spelling of the record size, which means clusters.
    #[test]
    fn a_record_can_be_given_in_clusters() {
        let g = parse(&sector(512, 2, 64, 1)).unwrap();
        assert_eq!(g.bytes_per_cluster, 1024);
        assert_eq!(g.bytes_per_record, 1024);
    }

    #[test]
    fn anything_that_is_not_ntfs_is_refused() {
        let mut fat = sector(512, 8, 100, 0xF6);
        fat[OEM..OEM + 8].copy_from_slice(b"MSDOS5.0");
        assert!(parse(&fat).is_none());
        assert!(parse(&[]).is_none(), "empty");
        assert!(parse(&vec![0u8; 511]).is_none(), "short");
    }

    /// Every one of these would become an offset into a raw volume.
    #[test]
    fn a_field_we_do_not_believe_refuses_the_whole_volume() {
        assert!(parse(&sector(777, 8, 100, 0xF6)).is_none(), "sector size");
        assert!(parse(&sector(512, 0, 100, 0xF6)).is_none(), "no clusters");
        assert!(parse(&sector(512, 3, 100, 0xF6)).is_none(), "odd cluster");
        assert!(parse(&sector(512, 8, 0, 0xF6)).is_none(), "mft at zero");
        assert!(parse(&sector(512, 8, 100, 0)).is_none(), "no record size");
        assert!(parse(&sector(512, 8, 100, 0xFF)).is_none(), "2-byte record");
        assert!(
            parse(&sector(4096, 0xE9, 100, 0xF6)).is_none(),
            "an eight-megabyte cluster is past the ceiling"
        );
    }

    /// The MFT cannot start past the end of the volume it is on.
    #[test]
    fn an_mft_outside_the_volume_is_refused() {
        assert!(parse(&sector(512, 8, 999_999_999, 0xF6)).is_none());
    }
}
