// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! XFS, read out of a disk image.
//!
//! The default filesystem on Red Hat, Fedora, Rocky and Alma, and what a WSL
//! data disk gets when somebody formats one without thinking about it. This
//! machine has a 195 GiB one, which is where the reader was checked.
//!
//! ## Layout
//!
//! | Module | What it does | Tested where |
//! | --- | --- | --- |
//! | [`mod@format`] | XFS structures, big-endian | everywhere, from bytes |
//! | [`reader`] | inodes, and a directory's blocks | everywhere, from a synthetic filesystem |
//!
//! Neither opens a file. Getting to the bytes is [`image`](crate::image),
//! which is indifferent to what is inside them.

pub mod build;
pub mod format;
pub mod reader;

#[cfg(test)]
mod tests;
