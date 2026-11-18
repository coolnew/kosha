// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! ISO 9660, read out of a disc image.
//!
//! **The most common disk image anybody has as a file.** A Linux installer, a
//! Windows installer, a game, a backup somebody burned in 2009 — an `.iso` is
//! a disc image and a folder of them is how a great many gigabytes go missing.
//! Until this, kosha said there was no filesystem it could read in one.
//!
//! **And after FAT was added it said something worse.** A bootable installer
//! carries an EFI system partition laid over the same bytes by a partition
//! table in the space ISO 9660 leaves free at the front — that is what makes
//! it boot on a UEFI machine. So a scan found the partition table, found a FAT
//! filesystem in it, and honestly reported the ten megabytes of boot loader on
//! a five-gigabyte disc. Reading the disc itself is what stops that, and
//! [`image`](crate::image) takes a filesystem at the front of a disk over
//! anything a partition table points at, which is where the two meet.
//!
//! ## Layout
//!
//! | Module | What it does | Tested where |
//! | --- | --- | --- |
//! | [`mod@format`] | descriptors, records, Rock Ridge entries | everywhere, from bytes |
//! | [`reader`] | which tree to walk, and reading it | everywhere, from a synthetic disc |
//! | [`build`] | the walk, and the tree it makes | everywhere |
//!
//! None of them opens a file. Getting to the bytes is [`image`](crate::image),
//! which is indifferent to what is inside them.
//!
//! ## The disc beside this one
//!
//! **UDF** is what a disc holding a file larger than four gigabytes uses, and
//! what a Windows installer is. It is read too, by [`udf`](crate::udf) — and
//! where a disc carries both, which is common, *that* one wins. The ISO 9660
//! tree on such a disc is the compatibility copy: it is there so that something
//! unable to read UDF sees something, and it cannot describe the large file that
//! is usually the reason anybody is scanning the disc.
//!
//! This paragraph used to say UDF was not read at all, and the refusal named it.
//! That was the right answer for as long as it was true.
//!
//! ## Several names for one run of blocks
//!
//! ISO 9660 has no hardlinks, and a disc mastered from a tree that had them has
//! something that behaves like one: every name points at a single extent.
//! `mkisofs` and `xorrisofs` do this with Rock Ridge and without it, and write a
//! `PX` link count of *1* while doing it — so the extent is the only thing on the
//! disc that says the bytes are there once, and it is what
//! [`build`] keys on. Counting them per name reports a disc as larger than the
//! medium it came from, which is not a debatable total but an impossible one.
//!
//! ## What a symbolic link costs here, and why it is not what ext4 says
//!
//! A record's flags have one bit for "directory" and nothing for a link, so the
//! only thing on a disc that says an entry is one is a Rock Ridge `PX` mode —
//! which is read, and until it was, a link on a disc was a file of no bytes.
//!
//! Its *apparent* size is 0, where the same tree on ext4 reports the length of
//! the target string. That is the medium and not a defect: a disc stores a link's
//! target in an `SL` entry beside the name, so there is no file data to have a
//! length. It is the one column on which the same tree read from a disc and from
//! an ext4 image is expected to differ, and it is written down here so that
//! nobody later reconstructs a number out of `SL` to make a comparison tidy.

pub mod build;
pub mod format;
pub mod reader;

// Reachable from the UDF tests, which build a bridge disc: an ISO 9660
// filesystem and a UDF volume over the same bytes, which is what a Windows
// installer is. The same reason `fat::tests` is reachable from here — a fixture
// of two filesystems at once has to be built out of both of their fixtures, or
// it is a fixture of what one module believes about the other.
#[cfg(test)]
pub(crate) mod tests;
