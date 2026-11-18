// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! FAT and exFAT, read out of a disk image.
//!
//! **The filesystem that is on every disk kosha could already read, and could
//! not read.** A GPT disk has an EFI system partition and it is FAT32; a
//! Windows installer stick is FAT32 or exFAT; a camera card, a games console's
//! storage, a Raspberry Pi's boot partition, and a `/boot/efi` on any Linux
//! machine that starts with UEFI are all one of these. Until this module, a
//! scan of a whole-disk image found the ext or NTFS partition and stepped
//! straight over the one at the front.
//!
//! ## Layout
//!
//! | Module | What it does | Tested where |
//! | --- | --- | --- |
//! | [`mod@format`] | boot sectors, table entries, both kinds of record | everywhere, from bytes |
//! | [`reader`] | chains, and a directory's clusters | everywhere, from a synthetic filesystem |
//! | [`build`] | the walk, and the tree it makes | everywhere |
//!
//! None of them opens a file. Getting to the bytes is [`image`](crate::image),
//! which is indifferent to what is inside them.
//!
//! ## Two formats, one module
//!
//! FAT12, FAT16 and FAT32 differ only in how wide a table entry is. exFAT
//! shares the *shape* — a heap of clusters, a table that chains them, and
//! directories made of 32-byte records — and none of the bytes. Putting them
//! together is worth it exactly once, at [`reader::Entry`]: above that line the
//! walk, the tree and the interface have no idea which of the two they are
//! looking at, and below it the two parsers share nothing but a constant.

pub mod build;
pub mod format;
pub mod reader;

#[cfg(test)]
pub(crate) mod tests;
