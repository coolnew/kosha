// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Exact saved-filename lookup shared by browsing, comparison and search.

use kosha_core::snapshot;
use std::{
    io,
    path::{Component, Path, PathBuf},
};

pub fn resolve(path: &Path) -> io::Result<PathBuf> {
    resolve_in(path, snapshot::store::dir().as_deref())
}

pub fn report_source(path: &Path, meta: &snapshot::Meta) {
    let selected = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    eprintln!(
        "source: snapshot; taken_unix={}; age_seconds={}; file={}",
        meta.taken,
        u64::from(kosha_core::format::now_unix()).saturating_sub(meta.taken),
        kosha_core::format::printable(&selected.to_string_lossy())
    );
}

/// Say, on stderr, when some of a snapshot's names could not be shown as
/// recorded. Silent otherwise.
pub fn report_replaced_names(tree: &kosha_core::SizeTree) {
    if let Some(note) = snapshot::replaced_names_note(tree) {
        eprintln!("note: {note}");
    }
}

fn resolve_in(path: &Path, store: Option<&Path>) -> io::Result<PathBuf> {
    let mut components = path.components();
    let Some(store) = store.filter(|_| {
        matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none()
            && path
                .extension()
                .is_some_and(|ext| ext == snapshot::EXTENSION)
    }) else {
        // Ordinary and explicit paths belong to the scan/open strategy. A
        // metadata probe here could interfere with device, UNC or image paths.
        return Ok(path.to_owned());
    };
    // Existing files, directories and even dangling links retain local-path
    // semantics. In particular, never hide a corrupt local snapshot with a
    // different saved record of the same name.
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(path.to_owned()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let candidate = store.join(path);
    match std::fs::symlink_metadata(&candidate) {
        Ok(_) => return Ok(candidate),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_and_explicit_paths_are_not_probed_by_snapshot_name_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, b"file").unwrap();
        for path in [file.join("virtual-root"), file.join("explicit.kosha")] {
            // A metadata call would fail with NotADirectory; leave validation
            // and device/image interpretation to the scan/open strategy.
            assert_eq!(resolve_in(&path, Some(dir.path())).unwrap(), path);
        }
    }
}
