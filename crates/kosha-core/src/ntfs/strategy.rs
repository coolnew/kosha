// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Opening a volume, and reading its MFT.
//!
//! The one part of this strategy that touches a device rather than a byte
//! slice; everything above it takes bytes and is under test everywhere.
//!
//! Every reason this can decline is a named [`Unavailable`], because a scan
//! that quietly fell back to the ordinary walk would leave somebody wondering
//! why it was slow. The caller puts the reason on the tree and the interface
//! reads it off there — what actually happened, rather than what might.

use std::path::Path;
use std::time::Instant;

use crate::error::ScanError;
use crate::platform;
use crate::scan::{Progress, ScanOptions};
use crate::tree::{SizeTree, Unavailable};

use super::{build, mft, security, volume};

/// Scan `root` by reading the MFT of the volume it is on.
///
/// Three phases: read the records, put the extension records back onto the
/// bases they belong to, rebuild the tree. The first dominates on a large volume
/// and is the only one that touches the disk.
pub fn scan(
    root: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    let started = Instant::now();

    let drive = volume::drive_of(root).ok_or(ScanError::Strategy(Unavailable::NoDriveLetter))?;
    if !platform::elevated() {
        return Err(ScanError::Strategy(Unavailable::NotElevated));
    }
    platform::enable_backup_privilege();

    let device = volume::Volume::open(&drive).map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => ScanError::Strategy(Unavailable::NotElevated),
        _ => ScanError::Strategy(Unavailable::Unreadable),
    })?;

    // Whether this is NTFS is the boot sector's answer, and reading it is the
    // first thing `Mft::open` does — so it is asked here rather than by the
    // code that opened the handle, which would be a second place that has to
    // agree about what NTFS looks like.
    let mut volume = mft::Mft::open(device).map_err(|error| match error.kind() {
        std::io::ErrorKind::InvalidData => ScanError::Strategy(Unavailable::NotNtfs),
        _ => ScanError::Strategy(Unavailable::Unreadable),
    })?;

    // The record the tree starts at. The volume root is always record 5, fixed
    // by the format; anywhere else is asked of the file itself, so a scan of
    // one directory reads the whole MFT and then keeps the part below it.
    let start = if is_volume_root(root) {
        build::ROOT_RECORD
    } else {
        volume::record_of(root).ok_or(ScanError::Strategy(Unavailable::Unreadable))?
    };

    let mut table = volume
        .read_table(
            |records| {
                publish(Progress {
                    dirs_scanned: None,
                    entries: records,
                    dirs_pending: 0,
                    depth: 0,
                })
            },
            cancelled,
        )
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::Interrupted => ScanError::Cancelled,
            _ => ScanError::Strategy(Unavailable::Unreadable),
        })?;

    build::consolidate(&mut table);

    // One read of `$Secure`, only when somebody asked who owns what. Failing to
    // read it is not failing the scan: the tree is complete and correct, and
    // every entry in it simply has an owner that could not be read — which the
    // report already has a row for, and which is a far better outcome than
    // refusing a scan of a million files over a table of a few hundred.
    let security = if options.collect_owners && !table.security_stream.fragments.is_empty() {
        volume
            .read_security(&table.security_stream)
            .unwrap_or_default()
    } else {
        security::Security::default()
    };

    let mut tree = build::build(
        &table,
        build::Root {
            record: start,
            path: root,
        },
        options,
        &security,
        publish,
        cancelled,
    );
    tree.stats.duration = started.elapsed();
    Ok(tree)
}

/// `C:\` and nothing else. A trailing component means a subdirectory, and the
/// record number for that has to be asked of the file rather than assumed.
fn is_volume_root(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(std::path::Component::Prefix(_)))
        && matches!(components.next(), Some(std::path::Component::RootDir))
        && components.next().is_none()
}
