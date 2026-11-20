// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Discovery and verified downloads from the public GitHub marketplace.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kosha_search::{MAX_RECIPE_BYTES, RecipeBook, RecipePackMetadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const HEAD: &str = "https://api.github.com/repos/coolnew/kosha-marketplace/git/ref/heads/main";
const RAW: &str = "https://raw.githubusercontent.com/coolnew/kosha-marketplace";
const MAX_CATALOG_BYTES: usize = 256 * 1024;

const MAX_SHARD_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 16_384;
const MAX_GENERIC_ARTIFACT_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    #[serde(default)]
    packages: Vec<Entry>,
    #[serde(default)]
    indexes: Vec<Index>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Index {
    kind: String,
    path: String,
    bytes: usize,
    sha256: String,
    entries: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: String,
    #[serde(default)]
    pub publisher: Option<String>,
    pub metadata: RecipePackMetadata,
    pub platforms: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    pub path: String,
    #[serde(default)]
    pub bytes: Option<usize>,
    pub sha256: String,
}

impl Entry {
    pub fn unavailable_reason(&self) -> Option<String> {
        if self.kind != "recipe-pack" && self.kind != super::insights::KIND {
            return Some(format!("Unsupported package kind: {}", self.kind));
        }
        if self.kind == super::insights::KIND && self.requires != [super::insights::CAPABILITY] {
            return Some("Insight packs require insight.locations.v1".into());
        }
        if !self.platforms.is_empty() && !self.platforms.iter().any(|p| p == std::env::consts::OS) {
            return Some("Not available on this OS".into());
        }
        if let Some(capability) = self.requires.iter().find(|r| {
            if self.kind == super::insights::KIND {
                r.as_str() != super::insights::CAPABILITY
            } else {
                !kosha_search::RECIPE_CAPABILITIES.contains(&r.as_str())
            }
        }) {
            return Some(format!("Unsupported requirement: {capability}"));
        }
        None
    }
}

pub struct Catalog {
    pub revision: String,
    entries: Vec<Entry>,
    search_terms: Vec<String>,
}

impl Catalog {
    pub fn search<'a>(&'a self, query: &str) -> impl Iterator<Item = &'a Entry> + use<'a> {
        let query = query.to_lowercase();
        let words: Vec<_> = query.split_whitespace().map(str::to_owned).collect();
        self.entries
            .iter()
            .zip(&self.search_terms)
            .filter(move |(_, terms)| words.iter().all(|word| terms.contains(word.as_str())))
            .map(|(entry, _)| entry)
    }

    pub fn install(&self, id: &str) -> Result<RecipePackMetadata, String> {
        if self.find(id)?.kind == super::insights::KIND {
            return super::insights::install(&download_insight(self, id, fetch)?);
        }
        let (text, book) = download(self, id, fetch)?;
        super::install_validated(&text, &book)
    }

    pub fn find(&self, id: &str) -> Result<&Entry, String> {
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.metadata.qualified_id() == id)
        {
            return Ok(entry);
        }
        let mut entries = self.entries.iter().filter(|e| {
            e.metadata.qualified_id() == id || (!id.contains('/') && e.metadata.id == id)
        });
        let found = entries
            .next()
            .ok_or_else(|| format!("Package {id} is absent from the catalog"))?;
        if entries.next().is_some() {
            return Err(format!("Ambiguous package {id}; use publisher/id"));
        }
        Ok(found)
    }
}

fn fixed_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_path(path: &str) -> bool {
    path.len() <= 512
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
        })
}

fn validate_entry(entry: &Entry, indexed: bool) -> Result<(), String> {
    entry.metadata.validate()?;
    if !super::valid_id(&entry.kind)
        || entry.publisher != entry.metadata.publisher
        || (indexed && (entry.publisher.is_none() || entry.bytes.is_none()))
        || !safe_path(&entry.path)
        || !fixed_hex(&entry.sha256, 64)
        || entry.bytes.is_some_and(|b| {
            b == 0
                || b > if entry.kind == super::insights::KIND {
                    kosha_analyze::pack::MAX_PACK_BYTES
                } else if entry.kind == "recipe-pack" {
                    MAX_RECIPE_BYTES
                } else {
                    MAX_GENERIC_ARTIFACT_BYTES
                }
        })
        || entry.platforms.len() > 16
        || entry.platforms.iter().any(|p| !super::valid_id(p))
        || entry.requires.len() > 32
        || entry.requires.iter().any(|r| {
            r.is_empty()
                || r.len() > 80
                || !r
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        })
    {
        return Err(format!(
            "Invalid catalog entry for {}",
            entry.metadata.qualified_id()
        ));
    }
    if indexed {
        if !entry.path.starts_with("packages/") {
            return Err("Indexed artifact must be under packages/".into());
        }
        if entry.kind == "recipe-pack" {
            let pack = &entry.metadata;
            let expected = format!(
                "packages/recipes/{}/{}/{}/pack.json",
                pack.publisher.as_deref().unwrap(),
                pack.id,
                pack.version
            );
            if entry.path != expected {
                return Err("Recipe artifact path disagrees with namespace/version".into());
            }
        }
        if entry.kind == super::insights::KIND {
            let pack = &entry.metadata;
            if entry.path
                != format!(
                    "packages/insights/{}/{}/{}/pack.json",
                    pack.publisher.as_deref().unwrap(),
                    pack.id,
                    pack.version
                )
            {
                return Err("Insight artifact path disagrees with its namespace/version".into());
            }
        }
    } else if !(entry
        .path
        .strip_prefix("recipes/")
        .and_then(|p| p.strip_suffix(".json"))
        .is_some_and(super::valid_id)
        || entry.path.starts_with("packages/"))
    {
        return Err("Invalid legacy artifact location".into());
    }
    Ok(())
}

fn finish(revision: &str, mut entries: Vec<Entry>) -> Result<Catalog, String> {
    if entries.len() > MAX_ENTRIES {
        return Err("Too many marketplace entries".into());
    }
    entries.sort_by_cached_key(|entry| entry.metadata.qualified_id());
    if entries.windows(2).any(|p| {
        p[0].metadata.id == p[1].metadata.id && p[0].metadata.publisher == p[1].metadata.publisher
    }) {
        return Err("Duplicate package namespace in marketplace".into());
    }
    // Words cannot contain the whitespace separating fields, so a substring
    // cannot accidentally match across two fields. Normalize once per load.
    let search_terms = entries
        .iter()
        .map(|entry| {
            let pack = &entry.metadata;
            [&pack.id, &pack.title, &pack.description, &entry.kind]
                .into_iter()
                .chain(pack.publisher.iter())
                .chain(&pack.tags)
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    Ok(Catalog {
        revision: revision.into(),
        entries,
        search_terms,
    })
}

#[cfg(test)]
fn parse(revision: &str, bytes: &[u8]) -> Result<Catalog, String> {
    resolve(revision, bytes, |_, _| {
        Err("Catalog indexes require sync".into())
    })
}

fn resolve(
    revision: &str,
    bytes: &[u8],
    fetch: impl Fn(&str, usize) -> Result<Vec<u8>, String>,
) -> Result<Catalog, String> {
    if !fixed_hex(revision, 40) || bytes.len() > MAX_CATALOG_BYTES {
        return Err("Invalid marketplace revision or root size".into());
    }
    let doc: Document = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if doc.schema != 1
        || doc.indexes.len() > 64
        || (!doc.indexes.is_empty() && !doc.packages.is_empty())
    {
        return Err("Unsupported or mixed marketplace catalog".into());
    }
    let mut entries = doc.packages;
    for entry in &entries {
        validate_entry(entry, false)?;
    }
    let mut total = bytes.len();
    let mut paths = std::collections::HashSet::new();
    for index in doc.indexes {
        if !super::valid_id(&index.kind)
            || !safe_path(&index.path)
            || !index.path.starts_with("indexes/")
            || !index.path.ends_with(".jsonl")
            || !paths.insert(index.path.clone())
            || index.bytes == 0
            || index.bytes > MAX_SHARD_BYTES
            || !fixed_hex(&index.sha256, 64)
            || index.entries == 0
            || index.entries > MAX_ENTRIES
            || entries.len() + index.entries > MAX_ENTRIES
        {
            return Err("Invalid marketplace index descriptor".into());
        }
        total = total
            .checked_add(index.bytes)
            .ok_or("Index size overflow")?;
        if total > MAX_TOTAL_BYTES {
            return Err("Marketplace indexes exceed total byte limit".into());
        }
        let shard = fetch(&format!("{RAW}/{revision}/{}", index.path), index.bytes)?;
        if shard.len() != index.bytes || format!("{:x}", Sha256::digest(&shard)) != index.sha256 {
            return Err("Marketplace index length/digest mismatch".into());
        }
        let text = std::str::from_utf8(&shard).map_err(|e| e.to_string())?;
        let start = entries.len();
        for line in text.lines() {
            if line.trim().is_empty() {
                return Err("Blank marketplace index entry".into());
            }
            let entry: Entry = serde_json::from_str(line).map_err(|e| e.to_string())?;
            validate_entry(&entry, true)?;
            if entry.kind != index.kind {
                return Err("Index kind disagrees with entry".into());
            }
            entries.push(entry);
            if entries.len() - start > index.entries {
                return Err("Index entry count mismatch".into());
            }
        }
        if entries.len() - start != index.entries {
            return Err("Index entry count mismatch".into());
        }
    }
    finish(revision, entries)
}

fn cache_path() -> Result<PathBuf, String> {
    kosha_core::dirs::data_dir()
        .map(|directory| directory.join("marketplace-catalog.snapshot"))
        .ok_or_else(|| "Kosha data directory is unavailable".into())
}

fn bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err(format!("Marketplace response exceeds {limit} bytes"));
    }
    Ok(bytes)
}

fn fetch(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();
    let response = agent
        .get(url)
        .header("User-Agent", "kosha-marketplace")
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(409) if url == HEAD => {
                "The public marketplace does not have a published main branch yet".into()
            }
            ureq::Error::StatusCode(404) => {
                "The marketplace file or branch is unavailable on GitHub".into()
            }
            _ => format!("Marketplace download failed: {error}"),
        })?;
    if response.status() != 200 {
        return Err(format!("Marketplace returned HTTP {}", response.status()));
    }
    bounded(response.into_body().into_reader(), limit)
}

fn sync_to(
    path: &Path,
    fetch: impl Fn(&str, usize) -> Result<Vec<u8>, String>,
) -> Result<Catalog, String> {
    #[derive(Deserialize)]
    struct Reference {
        object: Object,
    }
    #[derive(Deserialize)]
    struct Object {
        sha: String,
    }
    let head: Reference = serde_json::from_slice(&fetch(HEAD, 4096)?)
        .map_err(|error| format!("Could not resolve marketplace revision: {error}"))?;
    let revision = head.object.sha;
    if !fixed_hex(&revision, 40) {
        return Err("Invalid marketplace revision".into());
    }
    let bytes = fetch(&format!("{RAW}/{revision}/catalog.json"), MAX_CATALOG_BYTES)?;
    let catalog = resolve(&revision, &bytes, &fetch)?;
    let bytes = snapshot_bytes(&catalog, MAX_TOTAL_BYTES)?;
    let parent = path.parent().ok_or("Missing marketplace cache directory")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    writeln!(file, "{revision}")
        .and_then(|()| file.write_all(&bytes))
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    file.persist(path).map_err(|error| error.to_string())?;
    Ok(catalog)
}

fn snapshot_bytes(catalog: &Catalog, limit: usize) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(&Document {
        schema: 1,
        packages: catalog.entries.clone(),
        indexes: Vec::new(),
    })
    .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Normalized marketplace snapshot exceeds cache byte limit".into());
    }
    Ok(bytes)
}

pub fn sync() -> Result<Catalog, String> {
    sync_to(&cache_path()?, fetch)
}

fn read_snapshot(path: &Path) -> Result<Catalog, String> {
    let file = super::open_recipe_file(path).map_err(|error| {
        format!("Cannot open cached marketplace catalog. Run marketplace sync. {error}")
    })?;
    let bytes = bounded(file, 41 + MAX_TOTAL_BYTES)?;
    let separator = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or("Invalid cached marketplace snapshot")?;
    let (revision, bytes) = (&bytes[..separator], &bytes[separator + 1..]);
    let revision = std::str::from_utf8(revision).map_err(|error| error.to_string())?;
    if !fixed_hex(revision, 40) {
        return Err("Invalid cached marketplace revision".into());
    }
    let document: Document = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if document.schema != 1 || !document.indexes.is_empty() {
        return Err("Invalid cached marketplace snapshot".into());
    }
    for entry in &document.packages {
        validate_entry(entry, false)?;
    }
    finish(revision, document.packages)
}

pub fn cached() -> Result<Catalog, String> {
    read_snapshot(&cache_path()?)
}

fn download(
    catalog: &Catalog,
    id: &str,
    fetch: impl FnOnce(&str, usize) -> Result<Vec<u8>, String>,
) -> Result<(String, RecipeBook), String> {
    let entry = catalog.find(id)?;
    if let Some(reason) = entry.unavailable_reason() {
        return Err(reason);
    }
    let bytes = fetch(
        &format!("{RAW}/{}/{}", catalog.revision, entry.path),
        MAX_RECIPE_BYTES,
    )?;
    if entry.bytes.is_some_and(|size| size != bytes.len())
        || bytes.len() > MAX_RECIPE_BYTES
        || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256
    {
        return Err(format!("Marketplace digest mismatch for {id}"));
    }
    let text = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    let book = RecipeBook::from_json(&text)?;
    if let Some(capability) = book.required_capabilities().into_iter().find(|capability| {
        *capability != "recipe.exact"
            && !entry.requires.iter().any(|required| required == capability)
    }) {
        return Err(format!(
            "Artifact uses undeclared catalog requirement: {capability}"
        ));
    }
    if book.metadata() != Some(&entry.metadata) || book.recipes().is_empty() {
        return Err(format!(
            "Pack {id} metadata or platform does not match its catalog entry"
        ));
    }
    Ok((text, book))
}

fn download_insight(
    catalog: &Catalog,
    id: &str,
    fetch: impl FnOnce(&str, usize) -> Result<Vec<u8>, String>,
) -> Result<String, String> {
    let entry = catalog.find(id)?;
    if let Some(reason) = entry.unavailable_reason() {
        return Err(reason);
    }
    if entry.kind != super::insights::KIND {
        return Err("Expected insight-pack".into());
    }
    let bytes = fetch(
        &format!("{RAW}/{}/{}", catalog.revision, entry.path),
        kosha_analyze::pack::MAX_PACK_BYTES,
    )?;
    if entry.bytes != Some(bytes.len())
        || bytes.len() > kosha_analyze::pack::MAX_PACK_BYTES
        || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256
    {
        return Err(format!("Marketplace digest mismatch for {id}"));
    }
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let pack = super::insights::parse(&text)?;
    if pack.metadata != entry.metadata
        || pack.platforms != entry.platforms
        || entry.requires != [super::insights::CAPABILITY]
    {
        return Err(
            "Insight artifact metadata, platforms or capabilities disagree with catalog".into(),
        );
    }
    Ok(text)
}

pub fn install(id: &str) -> Result<RecipePackMetadata, String> {
    cached()?.install(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn indexed_future_insight_capabilities_are_unavailable_without_poisoning_catalog() {
        let text = super::super::insights::fixture("alice", "sessions", 1);
        let pack: serde_json::Value = serde_json::from_str(&text).unwrap();
        let entry: Entry = serde_json::from_value(json!({"kind":"insight-pack","publisher":"alice","metadata":pack["metadata"],
            "platforms":[],"requires":["insight.future.v9"],"path":"packages/insights/alice/sessions/1.0.0/pack.json",
            "bytes":text.len(),"sha256":format!("{:x}",Sha256::digest(text.as_bytes()))})).unwrap();
        validate_entry(&entry, true).unwrap();
        assert!(entry.unavailable_reason().is_some());
    }

    #[test]
    fn insight_download_is_pinned_bounded_and_checks_the_complete_contract() {
        let text = super::super::insights::fixture("alice", "sessions", 1);
        let pack: serde_json::Value = serde_json::from_str(&text).unwrap();
        let entry = json!({"kind":"insight-pack","publisher":"alice","metadata":pack["metadata"],
            "platforms":[],"requires":[super::super::insights::CAPABILITY],
            "path":"packages/insights/alice/sessions/1.0.0/pack.json","bytes":text.len(),"sha256":format!("{:x}",Sha256::digest(text.as_bytes()))});
        let catalog = parse(
            REVISION,
            json!({"schema":1,"packages":[entry.clone()]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let fetched = download_insight(&catalog, "alice/sessions", |url, limit| {
            assert_eq!(
                url,
                format!("{RAW}/{REVISION}/packages/insights/alice/sessions/1.0.0/pack.json")
            );
            assert_eq!(limit, kosha_analyze::pack::MAX_PACK_BYTES);
            Ok(text.as_bytes().to_vec())
        })
        .unwrap();
        assert_eq!(fetched, text);
        assert!(download_insight(&catalog, "sessions", |_, _| Ok(b"corrupt".to_vec())).is_err());
        let mut different = entry.clone();
        different["metadata"]["title"] = json!("Different title");
        let changed = parse(
            REVISION,
            json!({"schema":1,"packages":[different]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert!(
            download_insight(&changed, "sessions", |_, _| Ok(text.as_bytes().to_vec()))
                .unwrap_err()
                .contains("disagree")
        );
        let mut unknown = entry;
        unknown["requires"] = json!(["insight.exec"]);
        let unavailable = parse(
            REVISION,
            json!({"schema":1,"packages":[unknown]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert!(
            download_insight(&unavailable, "sessions", |_, _| panic!(
                "unsupported capability must never fetch"
            ))
            .is_err()
        );
    }

    fn fixture() -> (serde_json::Value, Vec<u8>) {
        let pack = json!({
            "schema": 1, "kind": "recipe-pack",
            "metadata": {"id": "tools", "title": "Build tools", "description": "Developer artifacts",
                         "version": "1.0.0", "tags": ["cmake"]},
            "recipes": [{"phrase": "build files", "dsl": "file and ext = txt"}]
        });
        let bytes = serde_json::to_vec(&pack).unwrap();
        (
            json!({
                "schema": 1,
                "packages": [{
                    "kind": "recipe-pack", "metadata": pack["metadata"], "platforms": [],
                    "path": "recipes/tools.json", "sha256": format!("{:x}", Sha256::digest(&bytes))
                }]
            }),
            bytes,
        )
    }

    #[test]
    fn sync_pins_urls_and_preserves_cache_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog");
        let (document, pack) = fixture();
        let json = serde_json::to_vec(&document).unwrap();
        sync_to(&path, |url, _| {
            if url == HEAD {
                return Ok(format!(r#"{{"object":{{"sha":"{REVISION}"}}}}"#).into_bytes());
            }
            assert_eq!(url, format!("{RAW}/{REVISION}/catalog.json"));
            Ok(json.clone())
        })
        .unwrap();
        let result = sync_to(&path, |url, _| {
            if url == HEAD {
                return Ok(format!(r#"{{"object":{{"sha":"{REVISION}"}}}}"#).into_bytes());
            }
            Err("connection interrupted".into())
        });
        assert!(result.is_err());
        let catalog = read_snapshot(&path).unwrap();
        assert_eq!(catalog.search("CMAKE artifacts").count(), 1);
        assert_eq!(catalog.search("CMAKE pdf").count(), 0);
        let (_, book) = download(&catalog, "tools", |url, _| {
            assert_eq!(url, format!("{RAW}/{REVISION}/recipes/tools.json"));
            Ok(pack)
        })
        .unwrap();
        assert_eq!(book.metadata().unwrap().id, "tools");
        assert!(download(&catalog, "tools", |_, _| Ok(b"bad digest".to_vec())).is_err());
        assert!(
            download(&catalog, "absent", |_, _| panic!(
                "absent pack must never fetch"
            ))
            .is_err()
        );
    }

    #[test]
    fn rejects_unsafe_paths_duplicate_entries_and_large_payloads() {
        let (mut doc, _) = fixture();
        let mut duplicate = doc.clone();
        let packages = duplicate["packages"].as_array_mut().unwrap();
        packages.push(packages[0].clone());
        assert!(parse(REVISION, duplicate.to_string().as_bytes()).is_err());
        for path in [
            "recipes/../tools.json",
            "recipes/a%2fb.json",
            "/recipes/tools.json",
        ] {
            doc["packages"][0]["path"] = json!(path);
            assert!(parse(REVISION, doc.to_string().as_bytes()).is_err());
        }
        assert!(bounded(&b"123"[..], 2).is_err());
    }

    #[test]
    fn artifact_rule_capabilities_must_be_declared_by_the_catalog() {
        let (mut doc, bytes) = fixture();
        let mut pack: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        pack["requires"] = json!(["recipe.regex"]);
        pack["recipes"] = json!([{"type":"regex", "phrase":"tag ABC", "pattern":"tag (?P<label>[A-Z]+)", "bindings":{"label":"text"}, "dsl":"name = {label}"}]);
        let bytes = serde_json::to_vec(&pack).unwrap();
        doc["packages"][0]["sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
        let catalog = parse(REVISION, doc.to_string().as_bytes()).unwrap();
        assert!(
            download(&catalog, "tools", |_, _| Ok(bytes.clone()))
                .err()
                .unwrap()
                .contains("undeclared catalog requirement")
        );
        doc["packages"][0]["requires"] = json!(["recipe.regex"]);
        let catalog = parse(REVISION, doc.to_string().as_bytes()).unwrap();
        assert!(download(&catalog, "tools", |_, _| Ok(bytes.clone())).is_ok());
    }

    fn indexed_fixture() -> (serde_json::Value, Vec<u8>, Vec<u8>) {
        let (document, bytes) = fixture();
        let mut pack: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        pack["metadata"]["publisher"] = json!("alice");
        let bytes = serde_json::to_vec(&pack).unwrap();
        let mut entry = document["packages"][0].clone();
        entry["publisher"] = json!("alice");
        entry["metadata"] = pack["metadata"].clone();
        entry["path"] = json!("packages/recipes/alice/tools/1.0.0/pack.json");
        entry["bytes"] = json!(bytes.len());
        entry["sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
        entry["requires"] = json!(["recipe.exact"]);
        let shard = format!("{entry}\n").into_bytes();
        let root = json!({"schema":1,"indexes":[{"kind":"recipe-pack","path":"indexes/recipes-0001.jsonl","bytes":shard.len(),"sha256":format!("{:x}",Sha256::digest(&shard)),"entries":1}]});
        (root, shard, bytes)
    }

    #[test]
    fn indexes_are_pinned_verified_and_cached_only_when_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog");
        let (root, shard, pack) = indexed_fixture();
        let root = root.to_string().into_bytes();
        let sync = |broken: bool| {
            sync_to(&path, |url, _| {
                if url == HEAD {
                    return Ok(format!(r#"{{"object":{{"sha":"{REVISION}"}}}}"#).into_bytes());
                }
                assert!(url.starts_with(&format!("{RAW}/{REVISION}/")));
                if url.ends_with("catalog.json") {
                    return Ok(root.clone());
                }
                assert!(url.ends_with("indexes/recipes-0001.jsonl"));
                Ok(if broken {
                    b"partial".to_vec()
                } else {
                    shard.clone()
                })
            })
        };
        sync(false).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(sync(true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let catalog = read_snapshot(&path).unwrap();
        assert_eq!(
            catalog.find("tools").unwrap().metadata.qualified_id(),
            "alice/tools"
        );
        download(&catalog, "alice/tools", |url, limit| {
            assert!(url.ends_with("packages/recipes/alice/tools/1.0.0/pack.json"));
            assert_eq!(limit, MAX_RECIPE_BYTES);
            Ok(pack)
        })
        .unwrap();
    }

    #[test]
    fn rejects_bad_index_descriptors_counts_and_unknown_requirements_refuse_install() {
        let (root, shard, _) = indexed_fixture();
        for bad in [
            "indexes/../x.jsonl",
            "indexes/a%2fb.jsonl",
            "indexes/a\\b.jsonl",
            "/indexes/x.jsonl",
        ] {
            let mut root = root.clone();
            root["indexes"][0]["path"] = json!(bad);
            assert!(
                resolve(REVISION, root.to_string().as_bytes(), |_, _| panic!(
                    "unsafe path must not fetch"
                ))
                .is_err()
            );
        }
        let mut wrong_count = root.clone();
        wrong_count["indexes"][0]["entries"] = json!(2);
        assert!(
            resolve(REVISION, wrong_count.to_string().as_bytes(), |_, _| Ok(
                shard.clone()
            ))
            .is_err()
        );
        let mut entry: Entry = serde_json::from_slice(&shard).unwrap();
        entry.requires = vec!["future.capability".into()];
        let catalog = finish(REVISION, vec![entry]).unwrap();
        assert_eq!(catalog.search("tools").count(), 1);
        assert!(
            download(&catalog, "tools", |_, _| panic!(
                "unavailable must not fetch"
            ))
            .is_err()
        );
        let mut entry = catalog.entries.into_iter().next().unwrap();
        entry.kind = "plugin".into();
        entry.path = "packages/plugins/alice/tools/1.0.0/plugin.bin".into();
        entry.bytes = Some(MAX_RECIPE_BYTES + 1);
        entry.requires.clear();
        validate_entry(&entry, true).unwrap();
        let catalog = finish(REVISION, vec![entry]).unwrap();
        assert_eq!(catalog.search("plugin").count(), 1);
        assert!(download(&catalog, "tools", |_, _| panic!("plugin must not execute")).is_err());
    }

    #[test]
    fn index_bounds_hashes_and_namespace_ambiguity_are_enforced() {
        let (root, shard, _) = indexed_fixture();
        for (field, value) in [
            ("bytes", json!(MAX_SHARD_BYTES + 1)),
            ("bytes", json!(0)),
            ("entries", json!(MAX_ENTRIES + 1)),
            ("sha256", json!("not-a-sha256")),
        ] {
            let mut bad = root.clone();
            bad["indexes"][0][field] = value;
            assert!(
                resolve(REVISION, bad.to_string().as_bytes(), |_, _| panic!(
                    "descriptor must fail before fetch"
                ))
                .is_err()
            );
        }
        let mut bad = root.clone();
        bad["indexes"][0]["sha256"] = json!("0".repeat(64));
        assert!(
            resolve(REVISION, bad.to_string().as_bytes(), |_, _| Ok(
                shard.clone()
            ))
            .is_err()
        );
        let first: Entry = serde_json::from_slice(&shard).unwrap();
        let mut second = first.clone();
        second.publisher = Some("bob".into());
        second.metadata.publisher = Some("bob".into());
        let catalog = finish(REVISION, vec![first.clone(), second]).unwrap();
        assert!(catalog.find("tools").err().unwrap().contains("Ambiguous"));
        assert_eq!(
            catalog.find("alice/tools").unwrap().metadata.qualified_id(),
            "alice/tools"
        );
        let mut legacy = first.clone();
        legacy.publisher = None;
        legacy.metadata.publisher = None;
        let mixed = finish(REVISION, vec![first.clone(), legacy]).unwrap();
        assert!(mixed.find("tools").unwrap().metadata.publisher.is_none());
        assert_eq!(
            mixed
                .find("alice/tools")
                .unwrap()
                .metadata
                .publisher
                .as_deref(),
            Some("alice")
        );
        assert!(finish(REVISION, vec![first.clone(), first]).is_err());
    }

    #[test]
    fn namespaced_inline_compatibility_catalog_round_trips_and_cache_encoding_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog");
        let (mut document, _) = fixture();
        document["packages"][0]["publisher"] = json!("alice");
        document["packages"][0]["metadata"]["publisher"] = json!("alice");
        let bytes = document.to_string().into_bytes();
        let catalog = sync_to(&path, |url, _| {
            if url == HEAD {
                return Ok(format!(r#"{{"object":{{"sha":"{REVISION}"}}}}"#).into_bytes());
            }
            assert_eq!(url, format!("{RAW}/{REVISION}/catalog.json"));
            Ok(bytes.clone())
        })
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        assert_eq!(
            read_snapshot(&path)
                .unwrap()
                .find("alice/tools")
                .unwrap()
                .path,
            "recipes/tools.json"
        );
        let encoded = snapshot_bytes(&catalog, MAX_TOTAL_BYTES).unwrap();
        assert!(
            encoded.len() > bytes.len(),
            "serialization adds optional/default fields"
        );
        assert!(snapshot_bytes(&catalog, encoded.len() - 1).is_err());
        assert!(snapshot_bytes(&catalog, encoded.len()).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn versions_are_single_safe_segments_with_build_metadata() {
        let (_, shard, _) = indexed_fixture();
        let mut entry: Entry = serde_json::from_slice(&shard).unwrap();
        for version in ["1.0.0+build.7", "v1.0-rc.1"] {
            entry.metadata.version = version.into();
            entry.path = format!("packages/recipes/alice/tools/{version}/pack.json");
            validate_entry(&entry, true).unwrap();
        }
        for version in ["", "../1", "1/2", "1\\2", "1%2f2", ".", "-1", "1 build"] {
            entry.metadata.version = version.into();
            entry.path = format!("packages/recipes/alice/tools/{version}/pack.json");
            assert!(validate_entry(&entry, true).is_err(), "{version}");
        }
    }

    #[test]
    fn cached_search_preserves_word_and_field_boundaries() {
        let (mut doc, _) = fixture();
        doc["packages"][0]["metadata"]["title"] = json!("Über");
        doc["packages"][0]["metadata"]["description"] = json!("Straße notes");
        let catalog = parse(REVISION, doc.to_string().as_bytes()).unwrap();
        assert_eq!(catalog.search("ÜBER notes").count(), 1);
        assert_eq!(catalog.search("überstraße").count(), 0);
        assert_eq!(catalog.search("  ").count(), 1);
        assert_eq!(catalog.search("STRASSE").count(), 0); // lowercase, not case folding
    }

    #[test]
    fn catalog_capabilities_cover_inactive_os_rules_too() {
        let (mut doc, bytes) = fixture();
        let mut pack: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let other = if std::env::consts::OS == "windows" {
            "linux"
        } else {
            "windows"
        };
        pack["recipes"].as_array_mut().unwrap().push(json!({"type":"template","phrase":"large items over 1 MB","os":other,"template":"large items over {n:size}","dsl":"size > {n}"}));
        pack["requires"] = json!(["recipe.exact", "recipe.template"]);
        let bytes = serde_json::to_vec(&pack).unwrap();
        doc["packages"][0]["sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
        let catalog = parse(REVISION, doc.to_string().as_bytes()).unwrap();
        assert!(
            download(&catalog, "tools", |_, _| Ok(bytes))
                .unwrap_err()
                .contains("undeclared catalog requirement")
        );
    }

    #[test]
    fn unsupported_platform_does_not_download() {
        let (mut doc, _) = fixture();
        let other = if std::env::consts::OS == "windows" {
            "linux"
        } else {
            "windows"
        };
        doc["packages"][0]["platforms"] = json!([other]);
        let catalog = parse(REVISION, doc.to_string().as_bytes()).unwrap();
        assert_eq!(catalog.search("").count(), 1);
        assert!(
            catalog
                .find("tools")
                .unwrap()
                .unavailable_reason()
                .is_some()
        );
        assert!(download(&catalog, "tools", |_, _| panic!("must filter first")).is_err());
    }
}
