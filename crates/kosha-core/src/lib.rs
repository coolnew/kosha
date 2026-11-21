// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `kosha-core` — scan a directory tree fast, and hand back something compact
//! enough to keep in memory and cheap enough to render at 120 Hz.
//!
//! ```no_run
//! use kosha_core::{Scanner, ScanOptions, format};
//!
//! let tree = Scanner::new(ScanOptions::default()).scan(std::path::Path::new("."))?;
//! println!(
//!     "{} in {} entries",
//!     format::bytes(tree.total_allocated()),
//!     format::count(tree.stats().entries),
//! );
//! # Ok::<(), kosha_core::ScanError>(())
//! ```
//!
//! Three things are worth knowing before reading the code:
//!
//! * A [`SizeTree`] is two flat vectors — a [`Node`] arena and a byte arena of
//!   names. There are no pointers and no per-entry allocations.
//! * A directory's children occupy a *contiguous* range of arena indices, which
//!   is what makes sorting and rendering a directory a sequential scan.
//! * The arena is in breadth-first order, so `child_index > parent_index`
//!   always holds, and size aggregation is one reverse linear pass.
//!
//! Scanning does not rewrite source contents. Requested exports and snapshots
//! do write files; export destinations can be replaced. Cleanup of scanned
//! files lives in `kosha-fsops` and is gated behind read-write mode.

#![forbid(unsafe_op_in_unsafe_fn)]

/// Building a tree from another tool's listing, entry by entry.
pub mod builder;
/// Turning a broken-down date on a disk into seconds since 1970.
mod calendar;
pub mod compare;
pub mod dirs;
mod error;
pub mod export;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod ext4;
/// FAT and exFAT, read out of a disk image.
mod fat;
pub mod format;
/// Getting to the bytes of a disk image: a container, a table, a region.
mod image;
/// ISO 9660, read out of a disc image.
mod iso;
pub mod json;
mod names;
mod node;
mod ntfs;
pub mod owner;
pub mod partition;
pub mod path;
mod platform;
/// Naming what is in an image even where it cannot be read.
mod recognise;
pub mod say;
mod scan;
pub mod snapshot;
mod tree;
/// UDF, read out of a disc image or a volume.
mod udf;
pub mod version;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod vhdx;
/// XFS, read out of a disk image.
mod xfs;

pub use builder::{BuildError, Entry, TreeBuilder};
pub use error::ScanError;
pub use names::{NameArena, is_valid_encoded_name, is_well_formed_wtf8};
pub use node::{Flags, Node, NodeId};
pub use owner::{Owner, OwnerId, OwnerKind, Owners, Share};
pub use platform::wsl;
pub use platform::{
    DiskUsage, EntryMeta, Fragmentation, MediaType, PlatformCtx, allocated_size,
    can_cross_filesystems, compression_available_at, current_owner, current_owners, disk_usage,
    elevated, exact_size_costs_extra, fragmentation, hardlinks_cost_extra, long_paths_enabled,
    media_type, owner_costs_extra, resolve_owner, supports_compression, supports_fragmentation,
};
pub use scan::{Progress, ScanOptions, Scanner, scan};
pub use tree::{ScanStats, SizeTree, Strategy, Unavailable};
