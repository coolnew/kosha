// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! First insight capability: bounded structured locations, always review-only.
//! No downloaded code, shell operations, regexes, or destructive suggestions.

use std::io::Read;
use std::path::Path;

use kosha_analyze::pack::{self, PackRule};
use kosha_search::RecipePackMetadata;
use serde::Deserialize;

pub const KIND: &str = "insight-pack";
pub const CAPABILITY: &str = "insight.locations.v1";
pub(crate) const DIRECTORY: &str = "insight-packs";
pub const MAX_RULES: usize = 64;

#[cfg(test)]
pub(crate) fn fixture(publisher: &str, id: &str, count: usize) -> String {
    serde_json::json!({"schema":1,"kind":KIND,
        "metadata":{"publisher":publisher,"id":id,"version":"1.0.0","title":"Session review","description":"Valuable records to inspect"},
        "requires":[CAPABILITY],"platforms":[],
        "insights":(0..count).map(|n|serde_json::json!({"id":format!("session-{n}"),"title":"Review sessions","detail":"Keep anything valuable; this is not disposable cache.",
            "subject":"directory","location":{"base":"home","path":format!(".assistant/sessions-{n}")},"action":"review-only"})).collect::<Vec<_>>()}).to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    kind: String,
    metadata: RecipePackMetadata,
    requires: Vec<String>,
    #[serde(default)]
    platforms: Vec<String>,
    insights: Vec<serde_json::Value>,
}

pub(crate) struct InsightPack {
    pub metadata: RecipePackMetadata,
    pub platforms: Vec<String>,
    pub rules: Vec<PackRule>,
}

pub(crate) fn parse(text: &str) -> Result<InsightPack, String> {
    if text.len() > pack::MAX_PACK_BYTES {
        return Err("Insight pack exceeds 256 KiB".into());
    }
    let document: Document = serde_json::from_str(text).map_err(|e| e.to_string())?;
    document.metadata.validate()?;
    if document.schema != 1
        || document.kind != KIND
        || document.metadata.publisher.is_none()
        || document.requires != [CAPABILITY]
    {
        return Err(
            "Insight pack requires schema 1, a publisher and exactly insight.locations.v1".into(),
        );
    }
    if document.platforms.len() > 3
        || document
            .platforms
            .iter()
            .any(|p| !["windows", "linux", "macos"].contains(&p.as_str()))
        || document
            .platforms
            .iter()
            .enumerate()
            .any(|(i, p)| document.platforms[..i].contains(p))
    {
        return Err("Invalid insight pack platforms".into());
    }
    if document.insights.is_empty() || document.insights.len() > pack::MAX_PACK_RULES {
        return Err("Insight pack needs 1..32 rules".into());
    }
    for rule in &document.insights {
        let object = rule.as_object().ok_or("Insight rule must be an object")?;
        if object.keys().any(|key| {
            !["id", "title", "detail", "subject", "location", "action"].contains(&key.as_str())
        }) || object.get("action").and_then(|v| v.as_str()) != Some("review-only")
        {
            return Err(
                "Insight capability allows only structured location rules with review-only actions"
                    .into(),
            );
        }
    }
    let reserved: Vec<_> = kosha_analyze::registered_rules()
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect();
    let reserved: Vec<_> = reserved.iter().map(String::as_str).collect();
    let body = serde_json::json!({"version":1,"rules":document.insights}).to_string();
    let mut loaded =
        pack::parse(&body, Path::new("insight-pack"), &reserved).map_err(|e| e.to_string())?;
    if loaded.has_problems() {
        return Err(loaded.problems().join("; "));
    }
    for rule in &mut loaded.rules {
        if !matches!(&rule.matcher, pack::Matcher::Location(_)) {
            return Err("Insight requires a structured location".into());
        }
        // Slashes cannot appear in local rule IDs; qualified provenance cannot collide with them.
        rule.id = format!(
            "marketplace/{}/{}",
            document.metadata.qualified_id(),
            rule.id
        );
    }
    Ok(InsightPack {
        metadata: document.metadata,
        platforms: document.platforms,
        rules: loaded.rules,
    })
}

fn active(pack: &InsightPack) -> bool {
    pack.platforms.is_empty() || pack.platforms.iter().any(|p| p == std::env::consts::OS)
}

fn read(directory: &Path, id: &str) -> Result<Option<InsightPack>, String> {
    let path = super::pack_path(directory, id)?;
    if let Some(parent) = path.parent() {
        super::store_exists(parent)?;
    }
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.file_type().is_file() {
        return Err(format!("Insight pack {id} is not a regular file"));
    }
    let mut text = String::new();
    std::fs::File::open(&path)
        .and_then(|f| {
            f.take(pack::MAX_PACK_BYTES as u64 + 1)
                .read_to_string(&mut text)
        })
        .map_err(|e| e.to_string())?;
    let pack = parse(&text).map_err(|e| format!("Insight pack {id}: {e}"))?;
    if pack.metadata.qualified_id() != id {
        return Err("Insight filename and metadata disagree".into());
    }
    Ok(Some(pack))
}

fn load_from(directory: &Path, pending: Option<&InsightPack>) -> Result<Vec<PackRule>, String> {
    let mut ids = super::pack_ids(directory)?;
    if let Some(pack) = pending {
        let id = pack.metadata.qualified_id();
        if ids.contains(&id) {
            return Err(format!("Insight pack {id} is already installed"));
        }
        ids.push(id);
        ids.sort_unstable();
    }
    let mut rules = Vec::new();
    let pending_id = pending.map(|p| p.metadata.qualified_id());
    // Count inactive rules as well, so a moved profile does not activate an over-budget store.
    let mut count = 0;
    for id in ids {
        let stored;
        let pack = if pending_id.as_deref() == Some(&id) {
            pending
        } else {
            stored = read(directory, &id)?;
            stored.as_ref()
        };
        if let Some(pack) = pack {
            count += pack.rules.len();
            if count > MAX_RULES {
                return Err("Managed insight store exceeds its 64-rule budget".into());
            }
            if active(pack) {
                rules.extend(pack.rules.iter().cloned());
            }
        }
    }
    Ok(rules)
}

pub fn load() -> Result<Vec<PackRule>, String> {
    let Some(config) = kosha_core::dirs::config_dir() else {
        return Ok(Vec::new());
    };
    load_from(&config.join(DIRECTORY), None)
}

pub fn install(text: &str) -> Result<RecipePackMetadata, String> {
    let pack = parse(text)?;
    if !active(&pack) {
        return Err("Insight pack is not available on this OS".into());
    }
    install_in(&super::config()?.join(DIRECTORY), text, &pack)
}

fn install_in(
    directory: &Path,
    text: &str,
    pack: &InsightPack,
) -> Result<RecipePackMetadata, String> {
    super::persist_in(directory, text, &pack.metadata, || {
        load_from(directory, Some(pack)).map(|_| ())
    })
}

pub fn installed() -> Result<Vec<RecipePackMetadata>, String> {
    let directory = super::config()?.join(DIRECTORY);
    let mut result = Vec::new();
    for id in super::pack_ids(&directory)? {
        if let Some(pack) = read(&directory, &id)? {
            result.push(pack.metadata);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kosha_analyze::{Context, Rule, Suggested};

    #[test]
    fn review_only_capability_refuses_actions_code_path_escape_and_unknown_fields() {
        let original: serde_json::Value =
            serde_json::from_str(&fixture("alice", "sessions", 1)).unwrap();
        for (key, value) in [
            ("action", serde_json::json!("trash")),
            (
                "location",
                serde_json::json!({"base":"home","path":"../outside"}),
            ),
            (
                "location",
                serde_json::json!({"base":"home","path":"C:/Windows"}),
            ),
            ("location", serde_json::json!({"base":"home","path":"."})),
            (
                "location",
                serde_json::json!({"base":"home","path":(0..33).map(|_| "x").collect::<Vec<_>>().join("/")}),
            ),
            ("command", serde_json::json!("anything")),
            ("directory", serde_json::json!("sessions")),
        ] {
            let mut bad = original.clone();
            bad["insights"][0][key] = value;
            assert!(parse(&bad.to_string()).is_err(), "{key}");
        }
        for (key, value) in [
            ("schema", serde_json::json!(2)),
            ("requires", serde_json::json!([])),
            ("platforms", serde_json::json!(["windows", "windows"])),
            ("execute", serde_json::json!("no")),
        ] {
            let mut bad = original.clone();
            bad[key] = value;
            assert!(parse(&bad.to_string()).is_err(), "{key}");
        }
    }

    #[test]
    fn independent_synthetic_scan_confirms_only_the_exact_home_directory_matches() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        for path in [
            home.join(".assistant/sessions-0/valuable.jsonl"),
            home.join("projects/.assistant/sessions-0/decoy"),
            temp.path().join("outside/.assistant/sessions-0/decoy"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"valuable history").unwrap();
        }
        let tree = kosha_core::scan(temp.path()).unwrap();
        let context = Context {
            home: Some(kosha_core::path::canonical(&home)),
            min_reclaimable: 1,
            ..Context::at(100)
        };
        let pack = parse(&fixture("alice", "sessions", 1)).unwrap();
        let found = pack.rules[0].evaluate(&tree, &context);
        assert_eq!(found.len(), 1);
        assert_eq!(
            tree.path_of(found[0].nodes[0]),
            kosha_core::path::canonical(&home.join(".assistant/sessions-0"))
        );
        assert_eq!(found[0].action, Suggested::ReviewOnly);
        assert_eq!(
            found[0].rule.as_str(),
            "marketplace/alice/sessions/session-0"
        );
        assert_eq!(kosha_analyze::total_reclaimable(&found), 0);
        assert!(pack.rules[0].evaluate(&tree, &Context::at(100)).is_empty());
        // A scan rooted exactly at the target still resolves the root itself.
        let subtree = kosha_core::scan(&home.join(".assistant/sessions-0")).unwrap();
        assert_eq!(
            pack.rules[0].evaluate(&subtree, &context)[0].nodes,
            [subtree.root()]
        );
    }

    #[test]
    fn aggregate_preflight_preserves_existing_packs_and_personal_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join(DIRECTORY);
        let personal = temp.path().join("rules.d");
        std::fs::create_dir(&personal).unwrap();
        std::fs::write(personal.join("mine.json"), b"preserve personal input").unwrap();
        for publisher in ["alice", "bob"] {
            let text = fixture(publisher, "sessions", 32);
            install_in(&store, &text, &parse(&text).unwrap()).unwrap();
        }
        let rejected = fixture("charlie", "sessions", 1);
        assert!(
            install_in(&store, &rejected, &parse(&rejected).unwrap())
                .unwrap_err()
                .contains("budget")
        );
        assert_eq!(load_from(&store, None).unwrap().len(), 64);
        assert!(!store.join("charlie/pack-sessions.json").exists());
        assert_eq!(
            std::fs::read(personal.join("mine.json")).unwrap(),
            b"preserve personal input"
        );
        assert_eq!(
            std::fs::read_to_string(store.join("alice/pack-sessions.json")).unwrap(),
            fixture("alice", "sessions", 32)
        );
        super::super::uninstall_in(&store, "alice/sessions").unwrap();
        assert_eq!(load_from(&store, None).unwrap().len(), 32);
        std::fs::write(store.join("bob/pack-sessions.json"), b"corrupt").unwrap();
        assert!(load_from(&store, None).is_err());
        super::super::uninstall_in(&store, "bob/sessions").unwrap();
        assert!(load_from(&store, None).unwrap().is_empty());
    }

    #[test]
    fn inactive_rules_count_toward_limits_and_cannot_evade_validation() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join(DIRECTORY);
        for publisher in ["alice", "bob"] {
            let mut data: serde_json::Value =
                serde_json::from_str(&fixture(publisher, "sessions", 32)).unwrap();
            data["platforms"] =
                serde_json::json!([if cfg!(windows) { "linux" } else { "windows" }]);
            let text = data.to_string();
            install_in(&store, &text, &parse(&text).unwrap()).unwrap();
        }
        assert!(load_from(&store, None).unwrap().is_empty());
        let text = fixture("charlie", "sessions", 1);
        assert!(
            install_in(&store, &text, &parse(&text).unwrap())
                .unwrap_err()
                .contains("budget")
        );
    }
}
