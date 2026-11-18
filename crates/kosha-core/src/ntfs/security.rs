// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `$Secure`, the volume's shared table of security descriptors.
//!
//! NTFS does not store a security descriptor in each file's record. It stores
//! every *distinct* descriptor once, in a file called `$Secure`, and each
//! record carries a four-byte `SecurityId` into it. A volume with a million
//! files has a few thousand descriptors at the outside and usually a few
//! hundred, because a descriptor is inherited unchanged down whole directory
//! trees.
//!
//! That is the whole reason per-user attribution is free in an MFT scan and
//! costs a call per file in the ordinary walk. The walk has to ask the
//! filesystem who owns each file one at a time; here the ids arrive with the
//! records, and one sequential read of `$SDS` turns all of them into owners.
//!
//! ## The stream
//!
//! `$Secure` keeps three streams. Two are B-trees for looking a descriptor up
//! by id or by hash, and neither is needed: this reads *all* of them, so the
//! indexes would be a slower way to arrive at the same place. The third,
//! `$SDS`, is the descriptors themselves, one after another, each behind a
//! twenty-byte header:
//!
//! ```text
//! 0x00  u32  hash of the descriptor
//! 0x04  u32  security id
//! 0x08  u64  this entry's own offset within $SDS
//! 0x10  u32  length of the entry, header included
//! 0x14   …   a self-relative SECURITY_DESCRIPTOR
//! ```
//!
//! Entries are sixteen-byte aligned, and **every descriptor appears twice**:
//! NTFS mirrors the stream so that a torn write cannot lose the only copy, with
//! the second copy 256 KiB after the first. An entry never straddles a 256 KiB
//! boundary, so the tail of each block is zero padding. Both facts fall out of
//! the same rule here — a length that cannot be an entry means the rest of this
//! block is padding, and an id already seen keeps the copy already read.
//!
//! Parsed from a byte slice, so it is under test on whichever platform it is
//! being edited on.

use rustc_hash::FxHashMap;

use super::build::SecurityFragment;
use super::runs::{self, Run};
use crate::platform::sid;

/// The record number of `$Secure`. Fixed by the format, like `$MFT`'s 0 and the
/// root directory's 5.
pub const SECURE_RECORD: u32 = 9;

/// `$SDS`, as the UTF-16 units an attribute name is compared as.
pub const SDS_STREAM: [u16; 4] = [0x24, 0x53, 0x44, 0x53];

/// How far apart the two copies of the stream are, and therefore the block an
/// entry may not straddle.
const BLOCK: usize = 0x40000;

/// The entry header.
const HEADER: usize = 20;

/// Entries start on a sixteen-byte boundary.
const ALIGN: usize = 16;

/// The shortest a `SECURITY_DESCRIPTOR` can be: the header alone, naming
/// nothing. Anything below this is not an entry.
const MIN_DESCRIPTOR: usize = 20;

/// How much of `$SDS` is worth reading.
///
/// A busy file server's is a few megabytes; this is two orders beyond the
/// largest anybody has measured, and exists so that a corrupt length in the
/// attribute header cannot ask for a gigabyte of memory.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Who owns each security id on the volume.
///
/// Empty when a scan did not ask for owners, and empty is exactly right: every
/// lookup then misses, and nothing is pushed into the tree's owner table at
/// all, which is what [`Owners::were_collected`](crate::Owners::were_collected)
/// reports as "not collected".
pub type Security = FxHashMap<u32, String>;

/// Put `$SDS`'s fragments back into stream order, and say how long they cover.
///
/// A stream too fragmented to describe in one attribute record is continued in
/// another, and the continuations reach us in the order their records happened
/// to go past: on this machine's `C:`, cluster 0 is in MFT record 2022 and
/// cluster 249 is in record 83680. Sorting by starting cluster is what puts the
/// bytes back in the order the offsets inside them assume.
///
/// **A gap stops the read** rather than closing up. If a fragment is missing —
/// unreadable, or a run list that would not decode — every entry after it would
/// land at an offset the stream does not agree with, and `$SDS` entries are
/// self-describing enough that shifted bytes parse as *a table of the wrong
/// owners* rather than as nonsense. Stopping early names fewer accounts;
/// carrying on would name the wrong ones, and nothing downstream could tell.
///
/// Returns the runs in order and the clusters they cover, which is the second
/// bound on how much of the stream is worth reading.
pub fn order(fragments: &[SecurityFragment]) -> (Vec<Run>, u64) {
    let mut sorted: Vec<&SecurityFragment> = fragments.iter().collect();
    sorted.sort_by_key(|fragment| fragment.start_vcn);

    let mut extents: Vec<Run> = Vec::new();
    let mut vcn: u64 = 0;
    for fragment in sorted {
        if fragment.start_vcn != vcn {
            break;
        }
        let Some(decoded) = runs::decode(&fragment.runs) else {
            break;
        };
        vcn += decoded.iter().map(|run| run.clusters).sum::<u64>();
        extents.extend(decoded);
    }
    (extents, vcn)
}

/// Every owner in the table, by security id.
///
/// Only the owner: the rest of a descriptor is the access-control list, which
/// answers a different question — who *may* read this — and is not what "who is
/// using the disk" means. Reading only the owner keeps the map to one short
/// string per distinct descriptor.
///
/// A descriptor that names no owner contributes nothing. So does one that will
/// not parse: `$SDS` is the only structure here that a caller cannot check
/// against anything else, and a made-up owner would be worse than a file
/// attributed to nobody.
pub fn owners(sds: &[u8]) -> Security {
    let mut found: Security = Security::default();
    let mut at = 0usize;

    while at + HEADER <= sds.len() {
        let id = u32_at(sds, at + 0x04);
        let length = u32_at(sds, at + 0x10) as usize;

        // Zero padding at the tail of a block, or a length no entry could have,
        // or one that runs off the end of what was read. All three mean the
        // same thing: there is nothing more to read here, and the next entry
        // starts at the next block boundary.
        let plausible =
            (HEADER + MIN_DESCRIPTOR..=BLOCK).contains(&length) && at + length <= sds.len();
        if !plausible {
            at = (at / BLOCK + 1) * BLOCK;
            continue;
        }

        // The second copy of a descriptor carries the same id as the first, so
        // keeping the one already read is what skips the mirror — and is also
        // what makes a half-written second copy harmless.
        if !found.contains_key(&id)
            && let Some(owner) = sid::owner_of_descriptor(&sds[at + HEADER..at + length])
            && let Some(text) = sid::to_string(owner)
        {
            found.insert(id, text);
        }

        // `length` is at least 40, so this always advances.
        at = (at + length).next_multiple_of(ALIGN);
    }

    found
}

#[inline]
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid_bytes(subs: &[u32]) -> Vec<u8> {
        let mut out = vec![1u8, subs.len() as u8, 0, 0, 0, 0, 0, 5];
        for sub in subs {
            out.extend_from_slice(&sub.to_le_bytes());
        }
        out
    }

    /// A self-relative descriptor naming one owner.
    fn descriptor(owner: &[u8]) -> Vec<u8> {
        let mut out = vec![1u8, 0];
        out.extend_from_slice(&0x8004u16.to_le_bytes()); // SE_SELF_RELATIVE
        out.extend_from_slice(&20u32.to_le_bytes()); // OffsetOwner
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(owner);
        out
    }

    /// Append one `$SDS` entry, padded to the alignment the format uses.
    fn push_entry(sds: &mut Vec<u8>, id: u32, owner: &[u8]) {
        let body = descriptor(owner);
        let length = (HEADER + body.len()) as u32;
        let offset = sds.len() as u64;
        sds.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // hash
        sds.extend_from_slice(&id.to_le_bytes());
        sds.extend_from_slice(&offset.to_le_bytes());
        sds.extend_from_slice(&length.to_le_bytes());
        sds.extend_from_slice(&body);
        while !sds.len().is_multiple_of(ALIGN) {
            sds.push(0);
        }
    }

    // ---------------------------------------------------------------- order

    /// One run of `clusters` clusters starting at `lcn`, as a run list.
    ///
    /// The header nibbles say how many bytes the length and the offset take, so
    /// a two-byte cluster count needs a `0x12` and not a `0x11`. Written out
    /// properly rather than with one byte each, because `$SDS` is routinely a
    /// few hundred clusters and a helper that silently truncated 734 to 222 was
    /// the first thing these tests caught.
    fn run_list(lcn: u64, clusters: u64) -> Vec<u8> {
        let length_bytes = clusters.max(1).ilog2() as usize / 8 + 1;
        // The offset is signed, so a value whose top bit is set needs one byte
        // more than its magnitude suggests or it decodes as a negative jump.
        let offset_bytes = lcn.max(1).ilog2() as usize / 8 + 1;
        let offset_bytes = offset_bytes + usize::from(lcn >> (offset_bytes * 8 - 1) & 1 == 1);
        let mut out = vec![((offset_bytes as u8) << 4) | length_bytes as u8];
        out.extend_from_slice(&clusters.to_le_bytes()[..length_bytes]);
        out.extend_from_slice(&lcn.to_le_bytes()[..offset_bytes]);
        out.push(0x00);
        out
    }

    fn fragment(start_vcn: u64, lcn: u64, clusters: u64) -> SecurityFragment {
        SecurityFragment {
            start_vcn,
            runs: run_list(lcn, clusters),
        }
    }

    /// The shape found on this machine's `C:`: cluster 0 in MFT record 2022 and
    /// cluster 249 in record 83680, so the fragments arrive back to front about
    /// as often as not.
    #[test]
    fn fragments_are_put_back_into_cluster_order() {
        let out_of_order = [fragment(249, 0x50, 100), fragment(0, 0x20, 249)];
        let (extents, clusters) = order(&out_of_order);
        assert_eq!(clusters, 349);
        assert_eq!(extents.len(), 2);
        assert_eq!(extents[0].start, Some(0x20), "cluster 0 must be read first");
        assert_eq!(extents[1].start, Some(0x50));
    }

    #[test]
    fn one_fragment_needs_no_ordering() {
        let (extents, clusters) = order(&[fragment(0, 0x20, 734)]);
        assert_eq!(clusters, 734);
        assert_eq!(extents.len(), 1);
    }

    /// The case the early stop exists for. Every entry after a missing fragment
    /// would sit at an offset the stream does not agree with, and a `$SDS` entry
    /// is self-describing enough that shifted bytes parse as a table of the
    /// *wrong* owners rather than as nonsense.
    #[test]
    fn a_missing_fragment_stops_the_stream_rather_than_closing_the_gap() {
        let gapped = [fragment(0, 0x20, 100), fragment(249, 0x50, 100)];
        let (extents, clusters) = order(&gapped);
        assert_eq!(clusters, 100, "the run after the gap was read anyway");
        assert_eq!(extents.len(), 1);
    }

    #[test]
    fn a_stream_that_does_not_start_at_cluster_zero_reads_nothing() {
        let (extents, clusters) = order(&[fragment(249, 0x50, 100)]);
        assert_eq!(clusters, 0);
        assert!(extents.is_empty());
    }

    #[test]
    fn a_run_list_that_will_not_decode_stops_the_stream() {
        let broken = [
            fragment(0, 0x20, 100),
            SecurityFragment {
                start_vcn: 100,
                // A run header claiming eight length bytes and eight offset
                // bytes, with nothing behind it.
                runs: vec![0x88, 0x01],
            },
        ];
        let (extents, clusters) = order(&broken);
        assert_eq!(clusters, 100);
        assert_eq!(extents.len(), 1);
    }

    #[test]
    fn no_fragments_is_not_a_stream() {
        let (extents, clusters) = order(&[]);
        assert_eq!(clusters, 0);
        assert!(extents.is_empty());
    }

    // --------------------------------------------------------------- owners

    #[test]
    fn each_entry_gives_one_owner() {
        let mut sds = Vec::new();
        push_entry(&mut sds, 256, &sid_bytes(&[18]));
        push_entry(&mut sds, 257, &sid_bytes(&[21, 1, 2, 3, 1001]));
        push_entry(&mut sds, 258, &sid_bytes(&[32, 544]));

        let found = owners(&sds);
        assert_eq!(found.len(), 3);
        assert_eq!(found[&256], "S-1-5-18");
        assert_eq!(found[&257], "S-1-5-21-1-2-3-1001");
        assert_eq!(found[&258], "S-1-5-32-544");
    }

    /// The mirror. Every descriptor is in the stream twice; the second copy
    /// must not become a second answer, and must not stop the parse either.
    #[test]
    fn the_mirrored_copy_is_not_a_second_owner() {
        let mut sds = Vec::new();
        push_entry(&mut sds, 256, &sid_bytes(&[18]));
        push_entry(&mut sds, 257, &sid_bytes(&[21, 1, 2, 3, 1001]));
        let first = sds.clone();
        sds.resize(BLOCK, 0);
        sds.extend_from_slice(&first);

        let found = owners(&sds);
        assert_eq!(found.len(), 2, "the mirror was read as more owners");
        assert_eq!(found[&256], "S-1-5-18");
    }

    /// The tail of a 256 KiB block is zeros, and the next entry starts at the
    /// boundary rather than after the padding. A parser that walked byte by
    /// byte through the zeros would take a quarter of a million steps per block
    /// and read the second copy as garbage.
    #[test]
    fn padding_at_the_end_of_a_block_is_skipped_to_the_boundary() {
        let mut sds = Vec::new();
        push_entry(&mut sds, 256, &sid_bytes(&[18]));
        sds.resize(BLOCK, 0);
        push_entry(&mut sds, 900, &sid_bytes(&[21, 9, 9, 9, 1002]));

        let found = owners(&sds);
        assert_eq!(found.len(), 2);
        assert_eq!(found[&900], "S-1-5-21-9-9-9-1002");
    }

    /// A length that runs off the end of what was read. The stream is read in
    /// whole clusters and the last entry can be cut in half by the cap, so this
    /// is the ordinary case rather than corruption — and reading it would take
    /// the descriptor from whatever follows in memory.
    #[test]
    fn an_entry_running_past_the_end_is_left_alone() {
        let mut sds = Vec::new();
        push_entry(&mut sds, 256, &sid_bytes(&[18]));
        let whole = sds.len();
        push_entry(&mut sds, 257, &sid_bytes(&[21, 1, 2, 3, 1001]));
        sds.truncate(whole + HEADER + 4);

        let found = owners(&sds);
        assert_eq!(found.len(), 1);
        assert_eq!(found[&256], "S-1-5-18");
    }

    /// Every one of these used to be a way to loop forever or to walk
    /// backwards. The parse must terminate on anything.
    #[test]
    fn a_corrupt_length_cannot_loop() {
        for length in [0u32, 1, 8, 19, 39, u32::MAX, BLOCK as u32 + 1] {
            let mut sds = vec![0u8; BLOCK * 2 + 64];
            sds[0x04..0x08].copy_from_slice(&256u32.to_le_bytes());
            sds[0x10..0x14].copy_from_slice(&length.to_le_bytes());
            // Terminates, and that is the whole assertion.
            let found = owners(&sds);
            assert!(found.is_empty(), "a length of {length} produced an owner");
        }
    }

    #[test]
    fn an_empty_stream_has_no_owners() {
        assert!(owners(&[]).is_empty());
        assert!(owners(&[0u8; 8]).is_empty());
    }

    /// A descriptor with no owner is legal — it is what a descriptor carrying
    /// only an access-control list looks like — and is not an owner of nobody.
    #[test]
    fn a_descriptor_naming_no_owner_contributes_nothing() {
        let mut sds = Vec::new();
        push_entry(&mut sds, 256, &sid_bytes(&[18]));
        // Blank the OffsetOwner of the first entry, in place.
        sds[HEADER + 4..HEADER + 8].copy_from_slice(&0u32.to_le_bytes());
        push_entry(&mut sds, 257, &sid_bytes(&[21, 1, 2, 3, 1001]));

        let found = owners(&sds);
        assert_eq!(found.len(), 1);
        assert_eq!(found[&257], "S-1-5-21-1-2-3-1001");
    }
}
