// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reading a WSL distribution by its disk image instead of through the
//! redirector.
//!
//! Windows publishes every WSL distribution at `\\wsl.localhost\<name>` and
//! kosha scans it. The redirector is 9P over a socket, and it is *slow*:
//! `\\wsl.localhost\archlinux` whole is 95 seconds from Windows against 0.9
//! natively, a hundredfold. None of that is kosha's to give back — `cmd dir /s`
//! over the same path takes 287 seconds, so the parallel walk is already 2.6×
//! faster than the platform's own walker. The only way to close it is to stop
//! going through the redirector.
//!
//! The distribution's filesystem is one file on a Windows drive, `ext4.vhdx`.
//! Two formats sit between us and it: [`vhdx`](crate::vhdx), which says where a
//! virtual byte really lives, and this one.
//!
//! ## What it costs to use
//!
//! **Nothing.** No administrator, no mounting, no `AttachVirtualDisk` — the
//! image is opened for reading like any other file, and all three on this
//! machine open while two of their distributions are running. That is the
//! opposite of the MFT strategy, which needs an elevated token to open a
//! volume, and it is why this one is worth having on a machine nobody
//! administers.
//!
//! ## What it costs in accuracy
//!
//! **The image can be a little behind.** A running distribution's filesystem
//! lives in the virtual machine's page cache and reaches the image when Linux
//! gets round to it — seconds for data, and for metadata only once the journal
//! is checkpointed. kosha does not replay the journal, so a file created
//! moments ago may not be in the image at all.
//!
//! That is immaterial for measuring a disk and very material for "where is the
//! file I just made", so it is not a decision to make on somebody's behalf:
//! this is off by default, asked for by a setting or by holding Shift, and
//! every tree says which strategy produced it. The same shape as
//! the `ntfs` strategy, for a different reason.
//!
//! ## What it is better at, beyond speed
//!
//! The redirector presents a distribution as one flat filesystem with no
//! `st_dev` to compare, so a scan of it walks into `/proc`, `/sys` and — worst
//! — `/mnt`, which holds the Windows drives. kosha declines those by name
//! (see [`platform::wsl`](crate::wsl)), which works and is a list of known
//! facts rather than a rule.
//!
//! Reading the image needs no such list. A mount is a runtime arrangement and
//! the image contains only the distribution's own filesystem, so `/proc` and
//! `/mnt` are simply the empty directories they really are on disk. The
//! boundary problem does not exist here rather than being worked around.
//!
//! ## Layout
//!
//! | Module | What it does | Tested where |
//! | --- | --- | --- |
//! | [`mod@format`] | ext2/3/4 structures | everywhere, from bytes |
//! | [`reader`] | geometry, inodes, a directory's blocks | everywhere, from a synthetic filesystem |
//! | [`build`] | two passes into a [`SizeTree`](crate::SizeTree) | everywhere, from a synthetic filesystem |
//!
//! Every row runs on every platform, and none of them opens a file. Getting
//! *to* the bytes — a container, a partition table, the offset a filesystem
//! starts at, a distribution's name — is [`image`](crate::image), which is a
//! separate question with separate answers: a VHDX does not know what is
//! inside it.

// Compiled on every platform and exercised by the tests on every platform: a
// byte format decided by reading a specification should be verifiable on
// whichever machine it is being edited on, not only on the one that has WSL
// installed. Off Windows the only callers are those tests.
pub mod build;
pub mod format;
pub mod reader;

#[cfg(test)]
mod tests;
