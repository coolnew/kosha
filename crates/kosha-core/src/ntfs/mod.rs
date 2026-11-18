// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The elevated scan: reading NTFS' Master File Table instead of walking
//! directories.
//!
//! Every file on an NTFS volume has a record in one file, `$MFT`, and that
//! record holds the name, the parent, the timestamps, the attribute bits and —
//! the part that matters — the exact number of bytes the file occupies. Reading
//! that file once and rebuilding the tree from it is what lets WizTree and
//! Everything report a whole volume in seconds, and it is a fundamentally
//! different shape of work from opening every directory in turn.
//!
//! **It is a second strategy, not a replacement.** The Win32 walk is quicker
//! for a small or medium folder, because reading the MFT means reading *all* of
//! it whatever was asked for. The MFT only wins on a whole volume, where the
//! walk's per-directory opens are the entire cost. So this is off by default,
//! and the scan records which strategy produced it — the two can legitimately
//! disagree about what exists, and a number whose provenance is not stated is a
//! number nobody can check.
//!
//! **What it costs to use.** A handle on `\\.\C:`, which needs administrator,
//! and `SeBackupPrivilege` enabled in the token, which is the part that is easy
//! to miss: the privilege is present but *disabled* in an elevated token, and
//! without asking for it every read hits an ACL check. See
//! `platform::windows::enable_backup_privilege`.
//!
//! **NTFS only.** FAT32 and exFAT have no central metadata table — directory
//! entries live only inside the directory clusters — so there is nothing to
//! enumerate in one pass and nothing to fall back on but the ordinary walk,
//! which is what Everything does too. ReFS has a different structure again and
//! is not attempted.
//!
//! **Not `FSCTL_ENUM_USN_DATA`.** The obvious approach walks the MFT through
//! one ioctl and hands back a name, a parent and the attributes for every
//! record without any of this parsing. It was the plan until the records were
//! looked at: a `USN_RECORD` carries no size at version 2, 3 or 4, and
//! `MFT_ENUM_DATA_V1` only chooses which of those versions you get. It gives
//! the tree and not one number in it. The sizes are in the `$DATA` attribute,
//! and the `$DATA` attribute is only reachable by reading the records.
//!
//! ## Layout
//!
//! | Module | What it does | Tested where |
//! | --- | --- | --- |
//! | [`boot`] | The volume's geometry | everywhere, from bytes |
//! | [`runs`] | Where a non-resident attribute's clusters are | everywhere, from bytes |
//! | [`record`] | One MFT record | everywhere, from bytes |
//! | [`security`] | `$Secure`, so records can name an owner | everywhere, from bytes |
//! | [`build`] | Flat records into a [`SizeTree`](crate::SizeTree) | everywhere, from a synthetic table |
//! | [`mft`] | walking `$MFT`, wherever it is | everywhere, from a disk image |
//! | [`volume`] | Opening a device and reading at offsets | Windows, by hand |
//!
//! Only the last of those needs a real disk, which is the point of the split.

// Compiled on every platform, exercised by the tests on every platform, and
// called in anger only on Windows — the same arrangement as `platform::wsl` and
// `platform::reparse`, and for the same reason: a byte format decided by reading
// the specification should be verifiable on whichever machine it is being edited
// on, not only on the one that has the disk. Off Windows the only callers are
// those tests, which is the point rather than an oversight, so the dead-code
// lint is told so explicitly instead of being worked around with a fake caller.
pub mod boot;
pub mod build;
// Reading `$MFT` needs somewhere to read it from, and a disk image is
// somewhere. On every platform, because none of this needs a device: the one
// thing that does is `volume`, below.
pub mod mft;
pub mod record;
pub mod runs;
pub mod security;

#[cfg(windows)]
pub mod volume;

#[cfg(windows)]
mod strategy;

#[cfg(windows)]
pub use strategy::scan;

#[cfg(test)]
mod tests;
