// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Where snapshots are kept, and what is in there.
//!
//! One directory, flat, one file per snapshot. No index and no database: the
//! filenames sort into the order the snapshots were taken, and each one's
//! header is a few hundred bytes at the front of the file, so a listing is a
//! directory read and one small read per entry. An index would be a second
//! place for the truth to live and a first place for it to go stale.
//!
//! It is under [`crate::dirs::data_dir`] rather than the configuration
//! directory, on the same argument the audit log is: this is data, it is large,
//! and it is about the machine. Roaming a gigabyte of snapshots with a Windows
//! profile would be a surprising thing to do to somebody.

use std::fs;
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{EXTENSION, Meta, ReadError};
use crate::tree::SizeTree;

/// A snapshot on disk, as a listing sees it.
#[derive(Clone, Debug)]
pub struct Kept {
    pub path: PathBuf,
    pub meta: Meta,
    /// What it occupies. Shown in the window, because the one thing a person
    /// keeping snapshots of a full disk should never have to wonder is what
    /// the snapshots themselves are costing.
    pub bytes: u64,
}

/// The directory snapshots are kept in.
///
/// `None` only where the platform will not say where application data goes,
/// which is the same condition that already leaves the audit log nowhere to
/// live.
pub fn dir() -> Option<PathBuf> {
    Some(crate::dirs::data_dir()?.join("snapshots"))
}

/// Automatic drive records are deliberately outside the explicit snapshot
/// directory: neither the GUI rail nor CLI listings enumerate this cache.
pub fn automatic_dir() -> Option<PathBuf> {
    Some(crate::dirs::data_dir()?.join("drive-cache"))
}

fn root_key(root: &Path) -> Vec<u8> {
    let mut bytes = root.as_os_str().as_encoded_bytes().to_vec();
    if cfg!(windows) {
        bytes.make_ascii_lowercase();
        for byte in &mut bytes {
            if *byte == b'/' {
                *byte = b'\\';
            }
        }
    }
    while bytes.len() > 1
        && (bytes.last() == Some(&b'/') || (cfg!(windows) && bytes.last() == Some(&b'\\')))
    {
        bytes.pop();
    }
    bytes
}

pub fn same_root(a: &Path, b: &Path) -> bool {
    root_key(a) == root_key(b)
}

/// Stable, bounded filename, even for a long mounted-volume path. The header
/// root is checked when opening, so a hash collision cannot show another tree.
pub fn automatic_path_in(dir: &Path, root: &Path) -> PathBuf {
    let hash = root_key(root)
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    dir.join(format!("drive-{hash:016x}.{EXTENSION}"))
}

pub fn open_automatic(root: &Path) -> Result<(PathBuf, Meta, SizeTree), ReadError> {
    let dir = automatic_dir().ok_or_else(|| io::Error::other("application data is unavailable"))?;
    open_automatic_in(&dir, root)
}

pub fn open_automatic_in(dir: &Path, root: &Path) -> Result<(PathBuf, Meta, SizeTree), ReadError> {
    let path = automatic_path_in(dir, root);
    let (meta, tree) = open(&path)?;
    if !same_root(&meta.root, root) || tree.is_incomplete() {
        return Err(io::Error::other("cached drive does not match this completed scan").into());
    }
    Ok((path, meta, tree))
}

pub fn save_automatic(tree: &SizeTree, taken: SystemTime) -> io::Result<PathBuf> {
    let dir = automatic_dir().ok_or_else(|| io::Error::other("application data is unavailable"))?;
    save_automatic_in(&dir, tree, taken)
}

/// The caller selects volume roots only. A cancelled or depth-limited scan
/// must not replace the last usable record. Read errors remain recorded in
/// the tree, just as in an explicit snapshot, and are visible when browsing.
pub fn save_automatic_in(dir: &Path, tree: &SizeTree, taken: SystemTime) -> io::Result<PathBuf> {
    if tree.is_incomplete() {
        return Err(io::Error::other(
            "unfinished scans cannot replace the drive cache",
        ));
    }
    // Background compression tasks can finish out of order. Keep the newest
    // completed record when this process has more than one writer.
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _writing = WRITING
        .lock()
        .map_err(|_| io::Error::other("drive cache writer unavailable"))?;
    let destination = automatic_path_in(dir, tree.root_path());
    if read_meta_at(&destination).is_ok_and(|meta| meta.at_ms() > Meta::of(tree, taken, "").at_ms())
    {
        return Ok(destination);
    }
    // Reuse the checked, flushed, create-new writer, then atomically replace
    // the single drive slot. A write/rename failure preserves the old slot.
    let staged = save_in(dir, tree, "Automatic drive scan", taken)?;
    if let Err(error) = fs::rename(&staged, &destination) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(destination)
}

/// How many names to try before giving up.
///
/// Only reached by something taking snapshots in a loop inside one
/// millisecond. A bound rather than a `loop`, because a filesystem that
/// answers "this exists" to every name it is offered — a permission problem
/// misreported, a full directory — must end in an error and not in a spin.
const ATTEMPTS: u32 = 64;

/// Write `tree` as a new snapshot and return where it went.
///
/// Written to a temporary name and renamed into place, so a snapshot that fails
/// halfway — a full disk, most likely, which is the exact condition somebody
/// running this tool is in — never appears in the listing as a file that cannot
/// be opened.
///
/// **A snapshot never replaces one.** Taking two of the same folder in the same
/// second used to produce one file, because the name was built from whole
/// seconds: the second write landed on the first's name and the first was gone.
/// Milliseconds make that take a script rather than a double click, and
/// `create_new` below makes even a script fall to the next name instead of
/// overwriting — the name is chosen by asking the filesystem for it, which is
/// the only check that cannot lose a race with another kosha doing the same
/// thing.
pub fn save(tree: &SizeTree, label: &str, taken: SystemTime) -> io::Result<PathBuf> {
    let dir = dir().ok_or_else(|| {
        io::Error::other("this platform does not say where application data belongs")
    })?;
    save_in(&dir, tree, label, taken)
}

/// The same, into a directory the caller names.
///
/// Exists so the rule above can be tested. Where snapshots go is read from the
/// environment, and a test that moved the environment to check the rule would
/// be a test every other test in the process runs beside.
pub fn save_in(dir: &Path, tree: &SizeTree, label: &str, taken: SystemTime) -> io::Result<PathBuf> {
    let meta = Meta::of(tree, taken, label);
    save_with_meta_in(dir, tree, &meta, taken)
}

/// Keep a historical record as an explicit snapshot without dating it anew.
pub fn save_record(tree: &SizeTree, meta: &Meta) -> io::Result<PathBuf> {
    let dir = dir().ok_or_else(|| io::Error::other("application data is unavailable"))?;
    save_record_in(&dir, tree, meta)
}

/// Preserves the original header, including milliseconds, measurement options,
/// and tool provenance. Also works while the automatic file is still being saved.
pub fn save_record_in(dir: &Path, tree: &SizeTree, meta: &Meta) -> io::Result<PathBuf> {
    let taken = std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(meta.taken))
        .and_then(|time| {
            time.checked_add(std::time::Duration::from_millis(u64::from(meta.taken_ms)))
        })
        .ok_or_else(|| io::Error::other("the original snapshot date is out of range"))?;
    save_with_meta_in(dir, tree, meta, taken)
}

fn save_with_meta_in(
    dir: &Path,
    tree: &SizeTree,
    meta: &Meta,
    taken: SystemTime,
) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    for attempt in 0..ATTEMPTS {
        let final_path = dir.join(super::file_name_nth(tree.root_path(), taken, attempt));
        let temp_path = final_path.with_extension("writing");

        // `create_new` rather than `create`: it fails when the file is already
        // there, atomically, so two processes cannot both decide a name is
        // free. Whoever loses takes the next one.
        let file = match fs::File::create_new(&temp_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        // And the final name has to be free too: the temporary one being
        // available says nothing about a completed snapshot sitting there.
        if final_path.exists() {
            drop(file);
            let _ = fs::remove_file(&temp_path);
            continue;
        }

        let write = (|| {
            let mut out = BufWriter::new(file);
            super::write(tree, meta, &mut out)?;
            // Flushed explicitly: a `BufWriter` dropped on the way out of
            // scope swallows the error from its final flush, and a snapshot
            // whose last buffer never reached the disk is the one case this
            // whole dance is about.
            out.into_inner()?.sync_all()
        })();

        // Every failure from here takes the half-written file with it, whether
        // it was the writing or the rename that failed. A `.writing` file left
        // in the directory is invisible to the listing but not to the person
        // who goes looking at what snapshots cost.
        if let Err(error) = write.and_then(|()| fs::rename(&temp_path, &final_path)) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        return Ok(final_path);
    }

    Err(io::Error::other(
        "could not find an unused name for this snapshot",
    ))
}

/// A file in the store that is not a snapshot anybody can open.
///
/// Kept beside the good ones rather than dropped, because a snapshot that
/// vanishes from the listing is indistinguishable from one that was never
/// taken — and the difference matters to whoever went looking for it.
#[derive(Debug)]
pub struct Unreadable {
    pub path: PathBuf,
    pub why: ReadError,
}

/// What is in the store: the snapshots, and the files that are not.
#[derive(Debug, Default)]
pub struct Listing {
    /// Every snapshot kept, oldest first.
    pub kept: Vec<Kept>,
    /// Every file with the extension that could not be read, and why.
    pub unreadable: Vec<Unreadable>,
}

/// Every snapshot kept, oldest first.
///
/// Unreadable files are skipped rather than failing the listing: one damaged
/// snapshot must not be able to hide the nine good ones beside it. What they
/// are is [`list_all`]'s to report — this is for callers that only want the
/// list.
pub fn list() -> Vec<Kept> {
    list_all().kept
}

/// The same listing, with what it could not read.
///
/// **A damaged snapshot used to disappear.** The listing skipped it, both front
/// ends showed what was left, and the file was still sitting in the store
/// occupying its megabytes: `kosha --snapshots` said "1 snapshot" over a
/// directory holding four, and the window's rail said the same. The skipping is
/// right — one damaged file must not hide the nine good ones — and the note on
/// `list` said as much, adding that "the window is where somebody would be told
/// about it". Nothing carried the news that far, so nobody was told anywhere.
pub fn list_all() -> Listing {
    let Some(dir) = dir() else {
        return Listing::default();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Listing::default();
    };

    let mut listing = Listing::default();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == EXTENSION) {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match read_meta_at(&path) {
            Ok(meta) => listing.kept.push(Kept { path, meta, bytes }),
            Err(why) => listing.unreadable.push(Unreadable { path, why }),
        }
    }
    let kept = &mut listing.kept;

    // By the stamp inside rather than by the filename, which is only *usually*
    // the same: a snapshot copied in from another machine keeps its header and
    // may have been renamed by anything.
    //
    // To the millisecond, and then by name. Two snapshots the same age is not
    // a hypothetical — a folder that scans in under a second is a folder
    // somebody snapshots twice in a row — and a list whose order changes
    // between two readings of the same directory is one where the row a person
    // is about to click moves out from under them.
    kept.sort_by(|a, b| {
        a.meta
            .at_ms()
            .cmp(&b.meta.at_ms())
            .then_with(|| a.path.cmp(&b.path))
    });
    // The unreadable ones have no stamp to sort by — that is what is wrong with
    // them — so they go in name order, which is at least the same order twice.
    listing.unreadable.sort_by(|a, b| a.path.cmp(&b.path));
    listing
}

/// Read one snapshot's header without inflating its tree.
pub fn read_meta_at(path: &Path) -> Result<Meta, ReadError> {
    let file = fs::File::open(path)?;
    super::read_meta(BufReader::new(file))
}

/// Open a snapshot as a browsable tree.
pub fn open(path: &Path) -> Result<(Meta, SizeTree), ReadError> {
    let file = fs::File::open(path)?;
    // A large buffer because this is one long sequential read of a compressed
    // stream, and the default 8 KiB would be a syscall every few thousand
    // entries.
    super::read(BufReader::with_capacity(256 * 1024, file))
}

/// Delete one.
///
/// The one destructive thing in this module, and it is deliberately not
/// routed through `kosha-fsops`: that crate's grant exists to guard *the
/// user's* files, and a snapshot is kosha's own bookkeeping. Making it need
/// read-write mode would mean arming the window that can delete a photo
/// library in order to tidy up a list.
pub fn remove(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

/// What every snapshot occupies together.
pub fn total_bytes(kept: &[Kept]) -> u64 {
    kept.iter().map(|k| k.bytes).fold(0, u64::saturating_add)
}
