// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! UDF, read out of a disc image or a volume.
//!
//! **What a Windows installer is.** ISO 9660 cannot hold a file larger than four
//! gigabytes in one extent and `install.wim` is larger than that, so every
//! Windows image since 7 is a UDF volume — usually with an ISO 9660 structure
//! beside it for anything that can only read one, and that structure does not
//! describe the large file. It is also what a rewritable disc is, what a
//! Blu-ray is, and what Windows writes when asked to format a stick as UDF.
//!
//! Until this, kosha read the ISO 9660 half of such a disc where there was one
//! and named UDF where there was not. Neither answer was wrong and both were
//! worse than reading it: [`image`](crate::image) now prefers UDF wherever a
//! volume carries both, because the ISO 9660 tree on a bridge disc is the
//! compatibility copy and the UDF one is the disc.
//!
//! ## Layout
//!
//! | Module | What it does | Tested where |
//! | --- | --- | --- |
//! | [`mod@format`] | tags, descriptors, extents, names, dates | everywhere, from bytes |
//! | [`reader`] | finding the volume, and turning a block into an offset | everywhere, from a synthetic volume |
//! | [`build`] | the walk, and the tree it makes | everywhere |
//!
//! None of them opens a file. Getting to the bytes is [`image`](crate::image),
//! which is indifferent to what is inside them.
//!
//! ## Why this is the largest of the readers per feature read
//!
//! ECMA-167 is a construction kit and UDF is the subset of it that anybody
//! writes, so almost everything here is one of several ways a volume may say the
//! same thing — and a reader has to do all of them because the choice belongs to
//! whoever wrote the volume:
//!
//! - **two file entry layouts**, because UDF 2.x inserted two fields into the
//!   middle of the 1.x one;
//! - **three allocation descriptor forms**, plus a fourth arrangement where the
//!   file's bytes sit where its descriptors would have been — which is how most
//!   small directories are stored, so it is the common case rather than a corner;
//! - **four ways a logical block becomes a physical one**, three of which exist
//!   because optical media are strange. All four are read.
//!
//! ## What is not read
//!
//! **An image of a write-once disc that was padded past its recording.** Such a
//! disc addresses every block through a table, and the table is written where
//! the recording stopped — so an image of the disc has it in its last block, and
//! an image padded out to the size of the medium has zeros there and the table
//! somewhere above, with nothing to say where. This looks back a bounded
//! distance and then refuses by name rather than searching a disc's worth of
//! zeros one block at a time.
//!
//! That is measured rather than reasoned about: `mkudffs` writing such a volume
//! into a pre-sized file leaves a perfectly good table at block 299 and thirteen
//! thousand blocks of zeros after it. Cut the file where the recording ended —
//! which is what an image of the disc is — and the same bytes read.
//!
//! **A sparable partition's spare table.** A rewritable disc keeps a list of
//! blocks that went bad and where their contents were moved to. kosha reads such
//! a volume as though nothing had ever been replaced, which is true of every one
//! that has not had a read error — and the note on the tree says the volume has
//! one, so a total that came from a disc with replacements is not quietly exact.

pub mod build;
pub mod format;
pub mod reader;

#[cfg(test)]
mod tests;
