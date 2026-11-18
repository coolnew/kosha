// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Naming what is in a disk image even where it cannot be read.
//!
//! **"There is no filesystem kosha reads in this file" is true and it reads
//! like "your file is broken".** Somebody who has just handed over a `dd` of a
//! Fedora laptop has a perfectly good image of a perfectly good filesystem;
//! what they need to be told is *btrfs*, which is a fact they can act on — go
//! and find a tool that reads btrfs, or mount it and scan the mount point.
//! What they get instead is a sentence that sends them looking for corruption
//! that is not there.
//!
//! This is the same lesson the `xfs` refusal taught before XFS was read, and it
//! is written down in [`Unavailable`](crate::tree::Unavailable): *named for
//! what it is rather than for what it is not.* The difference is that naming
//! costs a magic number and reading costs a reader, so this file can cover
//! every common answer at once while the list of filesystems kosha actually
//! reads grows one at a time.
//!
//! ## What is in the table, and why each one
//!
//! Not everything with a magic number. Each of these is something a person
//! plausibly hands to kosha as a disk image and is then owed an explanation
//! for:
//!
//! Every name here is read inside one sentence — *the disk image holds
//! ____, which kosha does not read* — so a container carries its article and a
//! filesystem does not.
//!
//! - **btrfs** is the default on Fedora, openSUSE and most network-attached
//!   storage, so it is the single most likely answer here.
//! - **LVM** and **MD RAID** are not filesystems at all, which is exactly why
//!   they are worth naming: the partition genuinely has no filesystem in it,
//!   and there is one more layer to go through before there is.
//! - **LUKS** and **BitLocker** are encrypted. There *is* a filesystem and
//!   nothing can see it, which is the opposite of what "no filesystem" says.
//! - **swap** has no files in it by design, and being told so ends the
//!   question rather than starting one.
//! - **APFS** and **HFS+** are what a Mac's disk is, **squashfs** is what a
//!   live image and a Snap are, **F2FS** is what a phone's storage is, and
//!   **ReiserFS**, **JFS** and **UFS** are what an old disk somebody kept is.
//!
//! ## What this costs
//!
//! Nothing that matters. It runs only on the failure path of a scan that is
//! already returning an error, and it is a handful of reads of a few bytes
//! each — the deepest is btrfs at 64 KiB in. Never per entry, never on a scan
//! that succeeded.

use crate::vhdx::ReadAt;

/// One thing to look for: a name, where to look, and what to find there.
struct Probe {
    name: &'static str,
    at: u64,
    magic: &'static [u8],
}

/// The probes, in the order they are tried.
///
/// **Order matters in exactly one place.** A LUKS header sits at the front of a
/// partition that may also hold the remains of whatever was there before it, so
/// encryption is looked for before filesystems; being told a disk is encrypted
/// is more use than being told what it used to be.
const PROBES: &[Probe] = &[
    // ---- containers and encryption, which are looked for first
    Probe {
        // Version 1 and version 2 share the signature; the version is the two
        // bytes after it and is not something this needs to know.
        name: "a LUKS encrypted volume",
        at: 0,
        magic: b"LUKS\xBA\xBE",
    },
    Probe {
        name: "a BitLocker encrypted volume",
        at: 3,
        magic: b"-FVE-FS-",
    },
    Probe {
        // `LABELONE` is the label header; it sits in the second sector of the
        // device, which is where `pvcreate` puts it.
        name: "an LVM physical volume",
        at: 512,
        magic: b"LABELONE",
    },
    Probe {
        name: "an LVM physical volume",
        at: 0,
        magic: b"LABELONE",
    },
    Probe {
        // A version 1.1 superblock, at the very front of the device.
        name: "a Linux software RAID member",
        at: 0,
        magic: b"\xFC\x4E\x2B\xA9",
    },
    Probe {
        // Version 1.2, four kilobytes in, which is what `mdadm` writes by
        // default.
        name: "a Linux software RAID member",
        at: 4096,
        magic: b"\xFC\x4E\x2B\xA9",
    },
    // ---- filesystems
    Probe {
        name: "btrfs",
        at: 0x1_0040,
        magic: b"_BHRfS_M",
    },
    Probe {
        name: "squashfs",
        at: 0,
        magic: b"hsqs",
    },
    Probe {
        // The same four bytes the other way round: squashfs records its own
        // byte order in its magic rather than in a field.
        name: "squashfs",
        at: 0,
        magic: b"sqsh",
    },
    Probe {
        name: "F2FS",
        at: 0x400,
        magic: b"\x10\x20\xF5\xF2",
    },
    Probe {
        name: "APFS",
        at: 0x20,
        magic: b"NXSB",
    },
    Probe {
        // **Two bytes would not be enough.** Every other entry here is four
        // bytes or more, which no ordinary file matches by accident; a
        // two-byte signature at a fixed offset is one file in sixty-five
        // thousand, and a confidently wrong name is worse than none. So the
        // version word after it is part of the magic: 4 for HFS+, and it is
        // stored the other way round from everything else because this is a
        // Macintosh format.
        name: "HFS+",
        at: 0x400,
        magic: b"H+\x00\x04",
    },
    Probe {
        // HFSX, which is HFS+ with case-sensitive names and a version of 5.
        name: "HFS+",
        at: 0x400,
        magic: b"HX\x00\x05",
    },
    Probe {
        name: "ReiserFS",
        at: 0x1_0034,
        magic: b"ReIsEr2Fs",
    },
    Probe {
        name: "ReiserFS",
        at: 0x1_0034,
        magic: b"ReIsEr3Fs",
    },
    Probe {
        name: "JFS",
        at: 0x8000,
        magic: b"JFS1",
    },
    Probe {
        // FreeBSD's UFS2, whose superblock is 64 KiB in on a modern disk.
        name: "UFS",
        at: 0x1_0000 + 0x55C,
        magic: b"\x19\x01\x54\x19",
    },
    Probe {
        // And UFS1, whose superblock is 8 KiB in.
        name: "UFS",
        at: 0x2000 + 0x55C,
        magic: b"\x54\x19\x01\x00",
    },
    // ---- last, because it is the one with no files in it
    Probe {
        // The label lives in the last ten bytes of the first page, and a page
        // is four kilobytes on every machine this is likely to meet.
        name: "Linux swap",
        at: 4096 - 10,
        magic: b"SWAPSPACE2",
    },
    Probe {
        name: "Linux swap",
        at: 4096 - 10,
        magic: b"SWAP-SPACE",
    },
];

/// Name what is in a disk: at the front, and then in its partitions.
///
/// **The two places a filesystem can be, in the order they are cheap**, which
/// is the same order [`image::find_filesystem`](crate::image) looks for one it
/// can read. A `dd` of one partition puts the filesystem at the front; a `dd`
/// of the whole disk puts a table there and the filesystem a megabyte in.
///
/// Partitions are tried largest first, because on a disk that has several the
/// large one is the answer somebody wants named: a Fedora disk is a small FAT
/// boot partition and a large btrfs one, and "FAT" is a true answer to the
/// wrong question.
pub(crate) fn name_of<R: ReadAt>(source: &mut R) -> Option<&'static str> {
    if let Some(name) = name_at(source, 0) {
        return Some(name);
    }
    let table = crate::partition::read(source)?;
    name_in(source, &table)
}

/// The same, for a caller that has already read the table.
pub(crate) fn name_in<R: ReadAt>(
    source: &mut R,
    table: &crate::partition::Table,
) -> Option<&'static str> {
    let mut by_size: Vec<&crate::partition::Partition> = table.partitions.iter().collect();
    // Largest first, and the number breaks a tie so that two runs over the same
    // disk cannot disagree — the same rule the choice between readable
    // partitions follows.
    by_size.sort_by(|a, b| b.length.cmp(&a.length).then(a.number.cmp(&b.number)));
    by_size
        .into_iter()
        .find_map(|partition| name_at(source, partition.start))
}

/// What is at `start`, if it is something worth naming.
///
/// `None` means the bytes are not anything in the table, which is the answer
/// for a file that was never a disk image at all.
pub(crate) fn name_at<R: ReadAt>(source: &mut R, start: u64) -> Option<&'static str> {
    let size = source.size();
    let mut buffer = [0u8; 16];
    for probe in PROBES {
        debug_assert!(
            probe.magic.len() <= buffer.len(),
            "a magic that will not fit"
        );
        let Some(at) = start.checked_add(probe.at) else {
            continue;
        };
        // A probe that reaches past the end of the source cannot match, and
        // asking costs a seek and a read that were never going to answer. Most
        // of this table is deeper than a small file is long.
        if at.saturating_add(probe.magic.len() as u64) > size {
            continue;
        }
        let wanted = &mut buffer[..probe.magic.len()];
        // A read that runs off the end of the source is a source too short to
        // hold this structure, which is an answer rather than an error.
        if source.read_exact_at(at, wanted).is_ok() && wanted == probe.magic {
            return Some(probe.name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deepest any probe reaches, which is what a fixture has to be longer
    /// than for every one of them to be asked. Checked against the table
    /// itself below, so it cannot quietly stop being true.
    const DEEPEST: u64 = 0x1_0560;

    /// A source long enough for every probe, with one magic written into it.
    fn image(at: u64, magic: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; (DEEPEST + 4096) as usize];
        let at = at as usize;
        bytes[at..at + magic.len()].copy_from_slice(magic);
        bytes
    }

    #[test]
    fn every_probe_in_the_table_finds_its_own_magic() {
        for probe in PROBES {
            let bytes = image(probe.at, probe.magic);
            assert_eq!(
                name_at(&mut bytes.as_slice(), 0),
                Some(probe.name),
                "{} at {:#x} was not found",
                probe.name,
                probe.at
            );
        }
    }

    #[test]
    fn nothing_is_found_in_a_file_that_is_not_an_image() {
        let bytes = vec![0u8; (DEEPEST + 4096) as usize];
        assert_eq!(name_at(&mut bytes.as_slice(), 0), None);
        // And in a file far too short to hold any of it.
        assert_eq!(name_at(&mut [0u8; 4].as_slice(), 0), None);
    }

    /// A partition does not start at the beginning of the disk, and every
    /// offset in the table is relative to the partition rather than to the
    /// disk. Getting that wrong finds nothing, on every disk that has a table.
    #[test]
    fn a_magic_is_found_relative_to_where_the_partition_starts() {
        const START: u64 = 2048 * 512;
        let mut bytes = vec![0u8; (START + DEEPEST + 4096) as usize];
        let at = (START + 0x1_0040) as usize;
        bytes[at..at + 8].copy_from_slice(b"_BHRfS_M");

        assert_eq!(name_at(&mut bytes.as_slice(), START), Some("btrfs"));
        assert_eq!(
            name_at(&mut bytes.as_slice(), 0),
            None,
            "the same disk read from the front holds nothing"
        );
    }

    /// Encryption is looked for before filesystems, because a partition that
    /// was something else before it was encrypted may still carry the remains
    /// of that — and "this disk is encrypted" is the more useful of the two
    /// true answers.
    #[test]
    fn encryption_is_named_before_whatever_was_there_before_it() {
        let mut bytes = image(0x1_0040, b"_BHRfS_M");
        bytes[0..6].copy_from_slice(b"LUKS\xBA\xBE");
        assert_eq!(
            name_at(&mut bytes.as_slice(), 0),
            Some("a LUKS encrypted volume")
        );
    }

    /// Every magic has to fit the buffer the probe reads into, and a table
    /// entry added later is exactly the sort of thing that would not.
    #[test]
    fn every_magic_fits_the_buffer_it_is_read_into() {
        for probe in PROBES {
            assert!(
                probe.magic.len() <= 16 && !probe.magic.is_empty(),
                "{} has a magic of {} bytes",
                probe.name,
                probe.magic.len()
            );
            assert!(
                probe.at + probe.magic.len() as u64 <= DEEPEST,
                "{} reaches past what DEEPEST claims",
                probe.name
            );
        }
    }
}
