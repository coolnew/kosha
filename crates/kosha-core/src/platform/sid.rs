// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Windows security identifiers, as bytes and as text.
//!
//! A SID is the only thing NTFS records about who owns a file. It is a short
//! binary structure — a revision, a 48-bit authority, and up to fifteen 32-bit
//! sub-authorities — and it reaches kosha from two very different places:
//!
//! * `GetFileSecurityW`, during the ordinary walk, as a pointer into a
//!   descriptor the API filled in;
//! * a security descriptor read straight out of `$Secure` during an MFT scan.
//!
//! Both hand over the same bytes, so both go through here, and the *canonical
//! string* is what the rest of kosha stores. That is deliberate: a SID has a
//! variable length, an owner table wants a hash key, and `S-1-5-21-…` is what
//! a user would recognise, paste into `icacls`, or search for when the machine
//! cannot name the account. Interning strings costs one allocation per
//! *distinct owner*, of which a volume has a few dozen.
//!
//! Parsed from a byte slice with no Windows call anywhere in the file, so the
//! format is under test on whichever platform it is being edited on — the same
//! arrangement as [`reparse`](super::reparse) and the `ntfs` modules.

use std::fmt::Write as _;

/// The most sub-authorities a SID may have, fixed by the format.
const MAX_SUB_AUTHORITIES: usize = 15;

/// Revision 1 is the only revision there has ever been.
const REVISION: u8 = 1;

/// The largest a SID can be: header, plus fifteen sub-authorities.
///
/// `SECURITY_MAX_SID_SIZE` in the Windows headers, spelled out here because
/// this file does not link against them.
pub const MAX_BYTES: usize = 8 + MAX_SUB_AUTHORITIES * 4;

/// How many bytes the SID starting at the front of `bytes` occupies.
///
/// `None` when what is there is not a SID: a wrong revision, an impossible
/// sub-authority count, or a structure that runs off the end of the slice.
/// Used to bound a read before trusting it — a security descriptor is
/// self-describing, and a malformed one must not be able to point us anywhere.
pub fn length(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 8 || bytes[0] != REVISION {
        return None;
    }
    let count = usize::from(bytes[1]);
    if count > MAX_SUB_AUTHORITIES {
        return None;
    }
    let total = 8 + count * 4;
    (total <= bytes.len()).then_some(total)
}

/// The canonical `S-1-5-21-…` form, as `ConvertSidToStringSidW` writes it.
///
/// Matching that spelling exactly is the point: the string is what gets shown,
/// stored and compared, and a SID kosha prints differently from every other
/// tool on the machine is one nobody can look up.
///
/// The authority is six bytes and **big-endian**, which is the one surprise in
/// the layout — everything else in the structure is little-endian like the rest
/// of the platform. Authorities above `0xFFFF_FFFF` are written in hexadecimal,
/// which is what Windows does and what the few SIDs that use them are known by.
pub fn to_string(bytes: &[u8]) -> Option<String> {
    let total = length(bytes)?;
    debug_assert!(total <= bytes.len());
    let count = usize::from(bytes[1]);

    let authority = u64::from(bytes[2]) << 40
        | u64::from(bytes[3]) << 32
        | u64::from(bytes[4]) << 24
        | u64::from(bytes[5]) << 16
        | u64::from(bytes[6]) << 8
        | u64::from(bytes[7]);

    // Fifteen sub-authorities of ten digits, plus separators and the prefix.
    let mut out = String::with_capacity(16 + count * 11);
    out.push_str("S-1-");
    if authority > u64::from(u32::MAX) {
        // Windows writes these as `0x` and twelve hexadecimal digits.
        let _ = write!(out, "{authority:#014x}");
    } else {
        let _ = write!(out, "{authority}");
    }
    for i in 0..count {
        let at = 8 + i * 4;
        let sub = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let _ = write!(out, "-{sub}");
    }
    Some(out)
}

/// The inverse of [`to_string`]: canonical text back to the bytes.
///
/// Needed because kosha stores the *string* — it is the hash key, the display
/// form and the thing a user can paste into another tool — while asking the
/// machine to name the account needs the structure back. Written here rather
/// than reached for through `ConvertStringSidToSidW` so the pair round-trips
/// under test on every platform, and so there is one place where the spelling
/// is decided.
///
/// `None` for anything that is not a SID this module would have written.
pub fn from_string(text: &str) -> Option<Vec<u8>> {
    let rest = text.strip_prefix("S-1-")?;
    let mut parts = rest.split('-');

    let authority_text = parts.next()?;
    let authority = match authority_text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok()?,
        None => authority_text.parse::<u64>().ok()?,
    };
    if authority >= 1 << 48 {
        return None;
    }

    let mut subs: Vec<u32> = Vec::with_capacity(MAX_SUB_AUTHORITIES);
    for part in parts {
        if subs.len() == MAX_SUB_AUTHORITIES {
            return None;
        }
        subs.push(part.parse::<u32>().ok()?);
    }

    let mut out = Vec::with_capacity(8 + subs.len() * 4);
    out.push(REVISION);
    out.push(subs.len() as u8);
    for shift in (0..6).rev() {
        out.push((authority >> (shift * 8)) as u8);
    }
    for sub in subs {
        out.extend_from_slice(&sub.to_le_bytes());
    }
    Some(out)
}

/// The owner's SID inside a self-relative `SECURITY_DESCRIPTOR`.
///
/// The descriptor read out of `$Secure` is always self-relative — that is the
/// only form NTFS stores — so every field in the header is an offset from the
/// front of the descriptor rather than a pointer, which is what makes it
/// readable at all from a file nobody has mapped into a process.
///
/// ```text
/// 0x00  u8   Revision
/// 0x01  u8   Sbz1
/// 0x02  u16  Control
/// 0x04  u32  OffsetOwner
/// 0x08  u32  OffsetGroup
/// 0x0C  u32  OffsetSacl
/// 0x10  u32  OffsetDacl
/// ```
///
/// A zero `OffsetOwner` means the descriptor names no owner, which is legal and
/// is not the same thing as a descriptor we failed to read — see
/// [`Owners`](crate::Owners) for why the two are kept apart.
pub fn owner_of_descriptor(descriptor: &[u8]) -> Option<&[u8]> {
    if descriptor.len() < 20 || descriptor[0] != REVISION {
        return None;
    }
    // `SE_SELF_RELATIVE`. Without it the offsets are absolute pointers into
    // some other process's address space, and reading them here is nonsense.
    const SELF_RELATIVE: u16 = 0x8000;
    let control = u16::from_le_bytes([descriptor[2], descriptor[3]]);
    if control & SELF_RELATIVE == 0 {
        return None;
    }

    let at =
        u32::from_le_bytes([descriptor[4], descriptor[5], descriptor[6], descriptor[7]]) as usize;
    if at == 0 || at >= descriptor.len() {
        return None;
    }
    let rest = &descriptor[at..];
    let total = length(rest)?;
    Some(&rest[..total])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a SID the way the structure is laid out, so the tests are written
    /// against the format rather than against the parser.
    fn sid(authority: u64, subs: &[u32]) -> Vec<u8> {
        let mut out = vec![REVISION, subs.len() as u8];
        for shift in (0..6).rev() {
            out.push((authority >> (shift * 8)) as u8);
        }
        for sub in subs {
            out.extend_from_slice(&sub.to_le_bytes());
        }
        out
    }

    #[test]
    fn local_system_is_s_1_5_18() {
        assert_eq!(to_string(&sid(5, &[18])).unwrap(), "S-1-5-18");
    }

    #[test]
    fn the_administrators_alias_is_s_1_5_32_544() {
        assert_eq!(to_string(&sid(5, &[32, 544])).unwrap(), "S-1-5-32-544");
    }

    /// The shape every account on a machine has: a domain identifier and a RID.
    #[test]
    fn an_account_keeps_all_of_its_sub_authorities() {
        let text = to_string(&sid(5, &[21, 1234567890, 987654321, 1122334455, 1001])).unwrap();
        assert_eq!(text, "S-1-5-21-1234567890-987654321-1122334455-1001");
    }

    /// A sub-authority above `i32::MAX` must not come out negative. Service
    /// accounts routinely land up here — `S-1-5-80-…-2271478464` is
    /// TrustedInstaller, which owns most of a Windows installation and is
    /// therefore the first row of the first report anybody runs.
    #[test]
    fn a_large_sub_authority_stays_unsigned() {
        let text = to_string(&sid(
            5,
            &[
                80, 956008885, 3418522649, 1831038044, 1853292631, 2271478464,
            ],
        ))
        .unwrap();
        assert_eq!(
            text,
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        );
    }

    /// The authority is six bytes and big-endian, which is the one field in the
    /// structure that is not little-endian. Reading it the other way round
    /// turns `S-1-5` into `S-1-1280`, which is a perfectly plausible-looking
    /// SID — so the failure is silent, and this is what catches it.
    #[test]
    fn the_authority_is_big_endian() {
        let bytes = sid(5, &[18]);
        assert_eq!(&bytes[2..8], &[0, 0, 0, 0, 0, 5]);
        assert_eq!(to_string(&bytes).unwrap(), "S-1-5-18");
    }

    #[test]
    fn a_large_authority_is_written_in_hexadecimal() {
        assert_eq!(
            to_string(&sid(0x1_0000_0000, &[1])).unwrap(),
            "S-1-0x000100000000-1"
        );
    }

    #[test]
    fn the_zero_authority_null_sid_survives() {
        assert_eq!(to_string(&sid(0, &[0])).unwrap(), "S-1-0-0");
    }

    #[test]
    fn a_sid_with_no_sub_authorities_is_still_a_sid() {
        assert_eq!(to_string(&sid(5, &[])).unwrap(), "S-1-5");
        assert_eq!(length(&sid(5, &[])).unwrap(), 8);
    }

    #[test]
    fn length_counts_the_sub_authorities() {
        assert_eq!(length(&sid(5, &[21, 1, 2, 3, 1001])).unwrap(), 8 + 5 * 4);
    }

    #[test]
    fn a_truncated_sid_is_refused_rather_than_read_short() {
        let bytes = sid(5, &[21, 1, 2, 3, 1001]);
        for cut in 0..bytes.len() {
            assert!(
                to_string(&bytes[..cut]).is_none(),
                "a SID cut to {cut} bytes was read anyway"
            );
        }
        assert!(to_string(&bytes).is_some());
    }

    #[test]
    fn a_wrong_revision_is_not_a_sid() {
        let mut bytes = sid(5, &[18]);
        bytes[0] = 2;
        assert!(to_string(&bytes).is_none());
    }

    #[test]
    fn more_than_fifteen_sub_authorities_cannot_happen() {
        let mut bytes = sid(5, &[1; 15]);
        bytes[1] = 16;
        bytes.extend_from_slice(&7u32.to_le_bytes());
        assert!(to_string(&bytes).is_none());
        // Fifteen is legal, and is the most there can be.
        assert!(to_string(&sid(5, &[1; 15])).is_some());
        assert_eq!(length(&sid(5, &[1; 15])).unwrap(), MAX_BYTES);
    }

    // ------------------------------------------------------------ round trip

    /// The property the pair exists for. Written as a loop over the shapes that
    /// actually occur rather than as one example, because the failure mode —
    /// an owner that can be shown but never looked up — is invisible until a
    /// machine is asked to name an account and silently declines.
    #[test]
    fn every_sid_survives_a_round_trip_through_its_text() {
        let cases: &[(u64, &[u32])] = &[
            (5, &[18]),
            (5, &[]),
            (5, &[32, 544]),
            (5, &[21, 1234567890, 987654321, 1122334455, 1001]),
            (
                5,
                &[
                    80, 956008885, 3418522649, 1831038044, 1853292631, 2271478464,
                ],
            ),
            (0, &[0]),
            (1, &[0]),
            (16, &[16384]),
            (0x1_0000_0000, &[1]),
            (0xFFFF_FFFF_FFFF, &[u32::MAX; 15]),
        ];
        for (authority, subs) in cases {
            let bytes = sid(*authority, subs);
            let text = to_string(&bytes).unwrap();
            assert_eq!(
                from_string(&text).as_deref(),
                Some(&bytes[..]),
                "{text} did not come back as the bytes it was written from"
            );
        }
    }

    #[test]
    fn text_that_is_not_a_sid_is_refused() {
        for bad in [
            "",
            "S",
            "S-1",
            "S-1-",
            "S-2-5-18",              // revision 2 does not exist
            "1-5-18",                // no prefix
            "S-1-5-18-",             // trailing separator, so an empty component
            "S-1-5--18",             // empty component
            "S-1-5-4294967296",      // a sub-authority past u32
            "S-1-x-18",              // authority that is not a number
            "S-1-5-abc",             // sub-authority that is not a number
            "S-1-5--1",              // negative
            "S-1-0x1000000000000-1", // authority past the 48 bits it has
        ] {
            assert!(from_string(bad).is_none(), "{bad:?} was read as a SID");
        }
    }

    #[test]
    fn more_than_fifteen_sub_authorities_is_refused_in_text_too() {
        let sixteen = format!("S-1-5{}", "-1".repeat(16));
        assert!(from_string(&sixteen).is_none());
        let fifteen = format!("S-1-5{}", "-1".repeat(15));
        assert!(from_string(&fifteen).is_some());
    }

    // ------------------------------------------------------------ descriptors

    /// A self-relative descriptor with its owner immediately after the header.
    fn descriptor(owner: &[u8], control: u16) -> Vec<u8> {
        let mut out = vec![REVISION, 0];
        out.extend_from_slice(&control.to_le_bytes());
        out.extend_from_slice(&20u32.to_le_bytes()); // OffsetOwner
        out.extend_from_slice(&0u32.to_le_bytes()); // OffsetGroup
        out.extend_from_slice(&0u32.to_le_bytes()); // OffsetSacl
        out.extend_from_slice(&0u32.to_le_bytes()); // OffsetDacl
        out.extend_from_slice(owner);
        out
    }

    #[test]
    fn the_owner_comes_out_of_a_self_relative_descriptor() {
        let owner = sid(5, &[21, 1, 2, 3, 1001]);
        let bytes = descriptor(&owner, 0x8004);
        assert_eq!(owner_of_descriptor(&bytes).unwrap(), &owner[..]);
    }

    /// Without `SE_SELF_RELATIVE` the offsets are pointers into a process that
    /// is not this one, and every number in the header means something else.
    #[test]
    fn an_absolute_descriptor_is_refused() {
        let bytes = descriptor(&sid(5, &[18]), 0x0004);
        assert!(owner_of_descriptor(&bytes).is_none());
    }

    #[test]
    fn a_descriptor_naming_no_owner_is_refused_rather_than_read_from_zero() {
        let mut bytes = descriptor(&sid(5, &[18]), 0x8004);
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert!(owner_of_descriptor(&bytes).is_none());
    }

    /// The offset is read out of the descriptor itself, so a corrupt one can
    /// point anywhere. It must not be able to point outside the slice.
    #[test]
    fn an_owner_offset_past_the_end_reads_nothing() {
        let mut bytes = descriptor(&sid(5, &[18]), 0x8004);
        bytes[4..8].copy_from_slice(&9999u32.to_le_bytes());
        assert!(owner_of_descriptor(&bytes).is_none());
    }

    /// An offset that lands inside the descriptor but leaves no room for the
    /// SID it claims is there. The length check is what stops this; without it
    /// the slice indexing panics rather than returning `None`.
    #[test]
    fn an_owner_sid_running_past_the_end_reads_nothing() {
        let owner = sid(5, &[21, 1, 2, 3, 1001]);
        let mut bytes = descriptor(&owner, 0x8004);
        bytes.truncate(bytes.len() - 4);
        assert!(owner_of_descriptor(&bytes).is_none());
    }
}
