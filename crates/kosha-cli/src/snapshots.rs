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

fn resolve_in(path: &Path, store: Option<&Path>) -> io::Result<PathBuf> {
    // Existing files, directories and even dangling links retain local-path
    // semantics. In particular, never hide a corrupt local snapshot with a
    // different saved record of the same name.
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(path.to_owned()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut components = path.components();
    if matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && path
            .extension()
            .is_some_and(|ext| ext == snapshot::EXTENSION)
        && let Some(store) = store
    {
        let candidate = store.join(path);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(path.to_owned())
}
