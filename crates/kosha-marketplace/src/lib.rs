// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Local lifecycle for declarative recipe packs. Network discovery is separate
//! from installation, and query translation stays in kosha-search.

pub mod catalog;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use kosha_search::{MAX_RECIPE_BYTES, RECIPE_FILE, RecipeBook, RecipePackMetadata};

const PACK_DIRECTORY: &str = "recipe-packs";
const MAX_PACKS: usize = 32;

fn valid_id(id: &str) -> bool {
    (1..=80).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn config() -> Result<PathBuf, String> {
    kosha_core::dirs::config_dir()
        .ok_or_else(|| "Kosha configuration directory is unavailable".into())
}

fn pack_path(directory: &Path, id: &str) -> Result<PathBuf, String> {
    let parts: Vec<_> = id.split('/').collect();
    match parts.as_slice() {
        [id] if valid_id(id) => Ok(directory.join(format!("pack-{id}.json"))),
        [publisher, id] if valid_id(publisher) && valid_id(id) => {
            Ok(directory.join(publisher).join(format!("pack-{id}.json")))
        }
        _ => Err("Invalid recipe pack identity; use publisher/id".into()),
    }
}

fn read(file: File) -> Result<(String, RecipeBook), String> {
    let mut text = String::new();
    file.take(MAX_RECIPE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    let book = RecipeBook::from_json(&text).map_err(|error| format!("invalid recipes: {error}"))?;
    Ok((text, book))
}

pub fn read_recipe_file(path: &Path) -> Result<RecipeBook, String> {
    open_recipe_file(path)
        .and_then(read)
        .map(|(_, book)| book)
        .map_err(|error| format!("Recipe file {}: {error}", path.display()))
}

fn open_recipe_file(path: &Path) -> Result<File, String> {
    let meta = fs::metadata(path).map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err("Expected a regular recipe file".into());
    }
    File::open(path).map_err(|error| error.to_string())
}

/// Check the store itself without following a symlink in its place.
fn store_exists(directory: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(directory) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Recipe store is not a directory: {}",
            directory.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn pack_ids(directory: &Path) -> Result<Vec<String>, String> {
    if !store_exists(directory)? {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if valid_id(name) && entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            if !store_exists(&entry.path())? {
                continue;
            }
            for child in fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
                let child = child.map_err(|e| e.to_string())?;
                if let Some(id) = child
                    .file_name()
                    .to_str()
                    .and_then(|n| n.strip_prefix("pack-"))
                    .and_then(|n| n.strip_suffix(".json"))
                    && valid_id(id)
                {
                    if ids.len() == MAX_PACKS {
                        return Err(format!(
                            "The recipe store supports at most {MAX_PACKS} packs"
                        ));
                    }
                    ids.push(format!("{name}/{id}"));
                }
            }
        } else if let Some(id) = name
            .strip_prefix("pack-")
            .and_then(|n| n.strip_suffix(".json"))
            && valid_id(id)
        {
            ids.push(id.to_owned());
        }
        if ids.len() > MAX_PACKS {
            return Err(format!(
                "The recipe store supports at most {MAX_PACKS} packs"
            ));
        }
    }

    ids.sort_unstable();
    Ok(ids)
}

fn installed_book(directory: &Path, id: &str) -> Result<Option<RecipeBook>, String> {
    let path = pack_path(directory, id)?;
    if let Some(parent) = path.parent()
        && !store_exists(parent)?
    {
        return Ok(None);
    }
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !meta.file_type().is_file() {
        return Err(format!("Installed recipe pack is not a regular file: {id}"));
    }
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Recipe file {}: {error}", path.display())),
    };
    let (_, book) =
        read(file).map_err(|error| format!("Recipe file {}: {error}", path.display()))?;
    if book.metadata().is_none_or(|pack| pack.qualified_id() != id) {
        return Err(format!(
            "Installed recipe pack ID does not match its filename: {id}"
        ));
    }
    Ok(Some(book))
}

/// Default search sources shared by the CLI and GUI. An explicit recipe file
/// bypasses these defaults.
pub fn load() -> Result<RecipeBook, String> {
    let Some(config) = kosha_core::dirs::config_dir() else {
        return Ok(RecipeBook::default());
    };
    load_from(&config.join(PACK_DIRECTORY), None)
}

fn load_from(directory: &Path, pending: Option<&RecipeBook>) -> Result<RecipeBook, String> {
    let personal = directory
        .parent()
        .ok_or("Missing recipe configuration directory")?
        .join(RECIPE_FILE);
    let mut book = match File::open(&personal) {
        Ok(file) => {
            read(file)
                .map_err(|error| format!("Recipe file {}: {error}", personal.display()))?
                .1
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => RecipeBook::default(),
        Err(error) => return Err(format!("Recipe file {}: {error}", personal.display())),
    };
    let mut ids = pack_ids(directory)?;
    let pending_id = pending
        .and_then(RecipeBook::metadata)
        .map(RecipePackMetadata::qualified_id);
    if let Some(id) = &pending_id {
        if ids.contains(id) {
            return Err(format!("Recipe pack {id} is already installed"));
        }
        ids.push(id.clone());
        ids.sort_unstable();
    }
    for id in ids {
        // An uninstall can occur after enumeration. Readers see either the
        // complete pack or its absence, never a partly written pack.
        let pack = if pending_id.as_deref() == Some(id.as_str()) {
            pending.cloned()
        } else {
            installed_book(directory, &id)?
        };
        if let Some(pack) = pack {
            book.append_fallback(pack)
                .map_err(|e| format!("Recipe pack {id}: {e}"))?;
        }
    }
    Ok(book)
}

/// Serialize cooperating writers, including the count limit. The guard
/// releases when the file closes and survives a killed installation process.
fn lock(directory: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".lock"))
        .map_err(|error| error.to_string())?;
    file.lock().map_err(|error| error.to_string())?;
    Ok(file)
}

pub fn install_local(source: &Path) -> Result<RecipePackMetadata, String> {
    let file =
        open_recipe_file(source).map_err(|error| format!("{}: {error}", source.display()))?;
    let (text, book) = read(file).map_err(|error| format!("{}: {error}", source.display()))?;
    install_validated(&text, &book)
}

fn install_validated(text: &str, book: &RecipeBook) -> Result<RecipePackMetadata, String> {
    install_in(&config()?.join(PACK_DIRECTORY), text, book)
}

fn install_in(
    directory: &Path,
    text: &str,
    book: &RecipeBook,
) -> Result<RecipePackMetadata, String> {
    let pack = book
        .metadata()
        .ok_or("Installation needs a versioned recipe pack with metadata")?;
    if !store_exists(directory)? {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    if !store_exists(directory)? {
        return Err("Recipe store is unavailable".into());
    }
    let _guard = lock(directory)?;
    if pack_ids(directory)?.len() == MAX_PACKS {
        return Err(format!(
            "The recipe store supports at most {MAX_PACKS} packs"
        ));
    }
    // Check the exact prospective load order under the writer lock. A rejected
    // pack must not make a previously usable store exceed execution budgets.
    load_from(directory, Some(book))?;
    let path = pack_path(directory, &pack.qualified_id())?;
    let parent = path.parent().ok_or("Missing pack namespace directory")?;
    if !store_exists(parent)? {
        fs::create_dir(parent).map_err(|e| e.to_string())?;
    }
    if !store_exists(parent)? {
        return Err("Invalid pack namespace directory".into());
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temp.write_all(text.as_bytes())
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temp.persist_noclobber(path).map_err(|error| {
        if error.error.kind() == io::ErrorKind::AlreadyExists {
            format!("Recipe pack {} is already installed", pack.id)
        } else {
            error.error.to_string()
        }
    })?;
    Ok(pack.clone())
}

pub fn installed() -> Result<Vec<RecipePackMetadata>, String> {
    let directory = config()?.join(PACK_DIRECTORY);
    let mut packs = Vec::new();
    for id in pack_ids(&directory)? {
        if let Some(book) = installed_book(&directory, &id)?
            && let Some(metadata) = book.metadata()
        {
            packs.push(metadata.clone());
        }
    }
    Ok(packs)
}

pub fn uninstall(id: &str) -> Result<(), String> {
    uninstall_in(&config()?.join(PACK_DIRECTORY), id)
}

fn uninstall_in(directory: &Path, id: &str) -> Result<(), String> {
    pack_path(directory, id)?;
    if !store_exists(directory)? {
        return Err(format!("Recipe pack {id} is not installed"));
    }
    let _guard = lock(directory)?;
    let ids = pack_ids(directory)?;
    let matches: Vec<_> = if ids.iter().any(|key| key == id) {
        vec![id.to_owned()]
    } else {
        ids.into_iter()
            .filter(|key| !id.contains('/') && key.rsplit('/').next() == Some(id))
            .collect()
    };
    let key = match matches.as_slice() {
        [key] => key,
        [] => return Err(format!("Recipe pack {id} is not installed")),
        _ => return Err(format!("Ambiguous recipe pack {id}; use publisher/id")),
    };
    let path = pack_path(directory, key)?;
    if let Some(parent) = path.parent() {
        store_exists(parent)?;
    }
    // Removal must also recover packs rejected by newer semantic validation.
    // Identity comes from enumerated safe filenames; never follow file links.
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {
            fs::remove_file(path).map_err(|error| error.to_string())
        }
        Ok(_) => Err(format!("Installed recipe pack is not a regular file: {id}")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(format!("Recipe pack {id} is not installed"))
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pack(publisher: Option<&str>) -> (String, RecipeBook) {
        let mut metadata = json!({"id":"tools", "title":"Tools", "description":"Fixture tools", "version":"1.0.0", "tags":[]});
        if let Some(publisher) = publisher {
            metadata["publisher"] = json!(publisher);
        }
        let text = json!({"schema":1,"kind":"recipe-pack","metadata":metadata,"recipes":[{"phrase":"build files","dsl":"file and ext = txt"}]}).to_string();
        let book = RecipeBook::from_json(&text).unwrap();
        (text, book)
    }

    #[test]
    fn namespaces_coexist_and_legacy_unqualified_removal_is_unambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("packs");
        for publisher in [None, Some("alice"), Some("bob")] {
            let (text, book) = pack(publisher);
            install_in(&store, &text, &book).unwrap();
        }
        assert_eq!(
            pack_ids(&store).unwrap(),
            ["alice/tools", "bob/tools", "tools"]
        );
        assert!(store.join("alice/pack-tools.json").is_file());
        uninstall_in(&store, "tools").unwrap();
        assert!(!store.join("pack-tools.json").exists());
        assert!(
            uninstall_in(&store, "tools")
                .unwrap_err()
                .contains("Ambiguous")
        );
        uninstall_in(&store, "alice/tools").unwrap();
        uninstall_in(&store, "bob/tools").unwrap();
        assert!(pack_ids(&store).unwrap().is_empty());
        let (text, book) = pack(Some("alice"));
        install_in(&store, &text, &book).unwrap();
        assert!(
            install_in(&store, &text, &book)
                .unwrap_err()
                .contains("already installed")
        );
        uninstall_in(&store, "tools").unwrap();
    }

    #[test]
    fn installation_budget_refusal_preserves_store_and_personal_recipes() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("packs");
        let rules = |prefix: &str, count| {
            (0..count).map(|i|json!({"type":"template","phrase":format!("{prefix}{i} over 1 MB"),"template":format!("{prefix}{i} over {{n:size}}"),"dsl":"size > {n}"})).collect::<Vec<_>>()
        };
        let personal = json!(rules("personal", 64)).to_string();
        fs::write(temp.path().join(RECIPE_FILE), &personal).unwrap();
        let document = |publisher: &str, count| {
            json!({"schema":1,"kind":"recipe-pack","metadata":{"publisher":publisher,"id":"tools","title":"Tools","description":"Budget fixture","version":"1.0.0"},"requires":["recipe.template"],"recipes":rules(publisher,count)}).to_string()
        };
        let first = document("zeta", 64);
        install_in(&store, &first, &RecipeBook::from_json(&first).unwrap()).unwrap();
        let rejected = document("alpha", 1);
        assert!(
            install_in(
                &store,
                &rejected,
                &RecipeBook::from_json(&rejected).unwrap()
            )
            .unwrap_err()
            .contains("budget")
        );
        assert_eq!(pack_ids(&store).unwrap(), ["zeta/tools"]);
        assert_eq!(load_from(&store, None).unwrap().recipes().len(), 128);
        assert_eq!(
            fs::read_to_string(temp.path().join(RECIPE_FILE)).unwrap(),
            personal
        );
        assert_eq!(
            fs::read_to_string(store.join("zeta/pack-tools.json")).unwrap(),
            first
        );
    }

    #[test]
    fn invalid_pack_can_be_removed_without_following_its_contents() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("packs");
        fs::create_dir_all(store.join("alice")).unwrap();
        let path = store.join("alice/pack-tools.json");
        fs::write(&path, b"corrupt or no longer supported").unwrap();
        assert!(load_from(&store, None).is_err());
        uninstall_in(&store, "alice/tools").unwrap();
        assert!(!path.exists());
        assert!(load_from(&store, None).unwrap().recipes().is_empty());
        // A similarly named directory is not a removable pack file.
        fs::create_dir(&path).unwrap();
        assert!(uninstall_in(&store, "alice/tools").is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn rejects_namespace_traversal_and_metadata_filename_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        for key in [
            "../tools",
            "alice/../tools",
            "alice%2ftools",
            "a/b/c",
            "a\\b",
        ] {
            assert!(pack_path(temp.path(), key).is_err());
        }
        let (text, _) = pack(Some("alice"));
        let dir = temp.path().join("alice");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("pack-other.json"), text).unwrap();
        assert!(installed_book(temp.path(), "alice/other").is_err());
    }
}
