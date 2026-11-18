// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reading a file's extent map.
//!
//! `FSCTL_GET_RETRIEVAL_POINTERS` answers with a `RETRIEVAL_POINTERS_BUFFER`: a
//! count, a starting virtual cluster, and then a run of
//! (next-virtual-cluster, logical-cluster) pairs saying where each piece of the
//! file physically sits. Counting the placed pairs is how fragmented it is.
//!
//! The parsing lives here, away from the ioctl, so it is under test on whichever
//! platform it is being edited on — the same arrangement as [`super::reparse`]
//! and for the same reason. Only the call that produces these bytes is
//! Windows-only.

/// A logical cluster number that means "nothing is allocated here": the run is
/// a hole in a sparse file.
pub const UNPLACED: i64 = -1;

/// One `RETRIEVAL_POINTERS_BUFFER`, read as bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RetrievalPointers {
    /// The virtual cluster this answer starts at, which is where the previous
    /// round trip stopped rather than necessarily zero.
    pub starting_vcn: i64,
    /// `(next_vcn, lcn)` per run, in order. A run covers the clusters from the
    /// previous `next_vcn` — or `starting_vcn` for the first — up to its own.
    pub extents: Vec<(i64, i64)>,
}

/// Parse the variable-length extent map.
///
/// ```text
///  0  ExtentCount    u32
///  8  StartingVcn    i64   (8-aligned, so four bytes of padding precede it)
/// 16  Extents[]            NextVcn i64, Lcn i64
/// ```
///
/// The count is the driver's word and the length is the one we asked for, so
/// the two are reconciled here rather than either being trusted: a count larger
/// than the bytes that actually arrived is read as however many runs arrived.
/// This matters in the ordinary case rather than only a corrupt one — a file
/// with more runs than the buffer holds comes back with `ERROR_MORE_DATA`, a
/// full buffer, and a count describing what *would* have fitted.
pub fn parse(bytes: &[u8]) -> Option<RetrievalPointers> {
    const HEADER: usize = 16;
    const RUN: usize = 16;
    if bytes.len() < HEADER {
        return None;
    }
    let i64_at = |at: usize| {
        let mut value = [0u8; 8];
        value.copy_from_slice(&bytes[at..at + 8]);
        i64::from_le_bytes(value)
    };
    let declared = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let arrived = (bytes.len() - HEADER) / RUN;

    Some(RetrievalPointers {
        starting_vcn: i64_at(8),
        extents: (0..declared.min(arrived))
            .map(|i| {
                let at = HEADER + i * RUN;
                (i64_at(at), i64_at(at + 8))
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `RETRIEVAL_POINTERS_BUFFER`.
    ///
    /// `declared` is written into the count field and `runs` is what is actually
    /// appended, so the two can be made to disagree — which is what the driver
    /// does on every truncated answer.
    fn buffer(declared: u32, starting_vcn: i64, runs: &[(i64, i64)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&declared.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]); // padding before the 8-aligned i64
        out.extend_from_slice(&starting_vcn.to_le_bytes());
        for (next_vcn, lcn) in runs {
            out.extend_from_slice(&next_vcn.to_le_bytes());
            out.extend_from_slice(&lcn.to_le_bytes());
        }
        out
    }

    /// A file in one piece: one run, starting at virtual cluster zero.
    #[test]
    fn an_unfragmented_file_is_one_run() {
        let parsed = parse(&buffer(1, 0, &[(40, 1_234_567)])).unwrap();
        assert_eq!(parsed.starting_vcn, 0);
        assert_eq!(parsed.extents, vec![(40, 1_234_567)]);
    }

    #[test]
    fn several_runs_come_back_in_order() {
        let runs = [(10, 500), (25, 90_000), (40, 700)];
        let parsed = parse(&buffer(3, 0, &runs)).unwrap();
        assert_eq!(parsed.extents, runs.to_vec());
    }

    /// A file with no extents at all — empty, or resident in its own MFT
    /// record. The header arrives on its own and says zero.
    #[test]
    fn a_file_with_no_extents_parses_as_none_of_them() {
        let parsed = parse(&buffer(0, 0, &[])).unwrap();
        assert!(parsed.extents.is_empty());
    }

    /// The ordinary truncation case, not a corrupt one: the buffer filled up,
    /// the driver said `ERROR_MORE_DATA`, and the count describes what would
    /// have fitted. Reading `declared` runs out of a shorter buffer would walk
    /// off the end of it.
    #[test]
    fn a_count_larger_than_the_buffer_reads_only_what_arrived() {
        let parsed = parse(&buffer(9_999, 0, &[(10, 500), (25, 600)])).unwrap();
        assert_eq!(parsed.extents.len(), 2);
        assert_eq!(parsed.extents, vec![(10, 500), (25, 600)]);
    }

    /// And the other direction, which is how a continuation round trip looks:
    /// fewer runs claimed than the buffer could hold.
    #[test]
    fn a_continuation_starts_where_the_last_answer_stopped() {
        let parsed = parse(&buffer(1, 4_096, &[(8_192, 77), (0, 0)])).unwrap();
        assert_eq!(parsed.starting_vcn, 4_096);
        assert_eq!(parsed.extents, vec![(8_192, 77)]);
    }

    /// A hole occupies nothing and costs no seek. Counting it as a piece of the
    /// file would report every sparse file as badly fragmented.
    #[test]
    fn a_hole_is_marked_by_an_unplaced_cluster() {
        let parsed = parse(&buffer(3, 0, &[(10, 500), (200, UNPLACED), (210, 512)])).unwrap();
        let placed = parsed.extents.iter().filter(|(_, lcn)| *lcn != UNPLACED);
        assert_eq!(placed.count(), 2);
    }

    /// The bytes come from a driver, and a short one must not be indexed into.
    #[test]
    fn a_truncated_buffer_is_refused_rather_than_read() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 15]).is_none());
        // Exactly a header and no runs is valid, and says so.
        assert_eq!(parse(&[0u8; 16]).unwrap().extents.len(), 0);
        // A header plus half a run reads the header and none of the run.
        assert_eq!(parse(&[0u8; 24]).unwrap().extents.len(), 0);
    }
}
