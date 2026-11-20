// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Declarative locations resolve against injected context and recorded metadata,
//! never by opening, canonicalizing or scanning an author-supplied path.

use std::path::PathBuf;

use kosha_core::SizeTree;
use serde::{Deserialize, Serialize};

use crate::Context;

pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_COMPONENTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationBase {
    Home,
    ScanRoot,
    Absolute,
    Cache,
    Temp,
    LocalAppData,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub base: LocationBase,
    pub path: String,
}

impl Location {
    pub fn validate(mut self) -> Result<Self, &'static str> {
        if self.path.len() > MAX_PATH_BYTES {
            return Err("location.path exceeds 1024 UTF-8 bytes");
        }
        if self.path.trim() != self.path || self.path.chars().any(char::is_control) {
            return Err("location.path cannot have surrounding whitespace or control characters");
        }
        self.path = self.path.replace('\\', "/");
        if self.base == LocationBase::Absolute {
            if let Some(unc) = self.path.strip_prefix("//?/UNC/") {
                self.path = format!("//{unc}");
            } else if self.path.starts_with("//?/") && style(&self.path[4..]) == 2 {
                self.path = self.path[4..].to_owned();
            }
        }
        let rest = if self.base == LocationBase::Absolute {
            if let Some(unc) = self.path.strip_prefix("//") {
                let mut parts = unc.splitn(3, '/');
                let server = parts.next().unwrap_or("");
                let share = parts.next().unwrap_or("");
                if !normal(server) || !normal(share) || server == "?" || share == "?" {
                    return Err("absolute UNC locations need a server and share");
                }
                parts.next().unwrap_or("")
            } else if self.path.len() >= 3
                && self.path.as_bytes()[0].is_ascii_alphabetic()
                && &self.path.as_bytes()[1..3] == b":/"
            {
                &self.path[3..]
            } else if let Some(rest) = self.path.strip_prefix('/') {
                rest
            } else {
                return Err("absolute locations need an anchored Unix, drive or UNC path");
            }
        } else {
            self.path.as_str()
        };
        if !rest.is_empty()
            && (rest.split('/').count() > MAX_COMPONENTS
                || rest.split('/').any(|part| !normal(part)))
        {
            return Err(
                "location.path needs at most 32 normal components; traversal and empty components are refused",
            );
        }
        Ok(self)
    }

    /// Empty relative paths describe the base itself. Unknown OS providers do
    /// not fall back to home or the scan root. Absolute paths remain literal:
    /// a foreign-platform path simply cannot match this recorded root.
    pub fn target(&self, tree: &SizeTree, context: &Context) -> Option<PathBuf> {
        let root = match self.base {
            LocationBase::Home => context.home.as_ref()?,
            LocationBase::ScanRoot => tree.root_path(),
            LocationBase::Absolute => {
                let recorded_root = tree.root_path().to_string_lossy().replace('\\', "/");
                if style(&self.path) != style(&recorded_root) {
                    return None;
                }
                return Some(PathBuf::from(&self.path));
            }
            LocationBase::Cache => context.locations.cache.as_ref()?,
            LocationBase::Temp => context.locations.temp.as_ref()?,
            LocationBase::LocalAppData => context.locations.local_app_data.as_ref()?,
        };
        Some(root.join(&self.path))
    }
}

fn normal(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.contains(':')
}

fn style(path: &str) -> u8 {
    if path.starts_with("//") {
        1
    } else if path.len() >= 3
        && path.as_bytes()[0].is_ascii_alphabetic()
        && &path.as_bytes()[1..3] == b":/"
    {
        2
    } else if path.starts_with('/') {
        3
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn extended_absolute_locations_match_beyond_max_path() {
        use crate::Rule;
        let fixture = tempfile::tempdir().unwrap();
        let root = kosha_core::path::canonical(fixture.path());
        let mut deep = root.clone();
        while deep.as_os_str().len() < 400 {
            deep.push("a-long-directory-component");
        }
        let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", deep.display()));
        std::fs::create_dir_all(&verbatim).unwrap();
        std::fs::write(verbatim.join("valuable.txt"), b"keep").unwrap();
        let location = Location {
            base: LocationBase::Absolute,
            path: verbatim.to_string_lossy().into_owned(),
        }
        .validate()
        .unwrap();
        let tree = kosha_core::scan(&root).unwrap();
        assert!(!tree.is_partial());
        let rule = crate::pack::PackRule {
            id: "deep".into(),
            title: "Review".into(),
            detail: "Keep valuable data".into(),
            subject: "directory".into(),
            matcher: crate::pack::Matcher::Location(location),
            action: crate::Suggested::ReviewOnly,
        };
        let found = rule.evaluate(
            &tree,
            &Context {
                min_reclaimable: 1,
                ..Context::at(100)
            },
        );
        assert_eq!(found.len(), 1);
        assert_eq!(tree.path_of(found[0].nodes[0]), deep);
    }

    #[test]
    fn every_base_resolves_only_recorded_directories_and_stays_review_only() {
        use crate::{KnownLocations, Rule, Suggested};
        let fixture = tempfile::tempdir().unwrap();
        let root = kosha_core::path::canonical(fixture.path());
        for name in ["home", "cache", "temp", "local", "decoy/home"] {
            std::fs::create_dir_all(root.join(name).join("records")).unwrap();
            std::fs::write(root.join(name).join("records/valuable.txt"), b"valuable").unwrap();
        }
        let tree = kosha_core::scan(&root).unwrap();
        let context = Context {
            home: Some(root.join("home")),
            min_reclaimable: 1,
            locations: KnownLocations {
                cache: Some(root.join("cache")),
                temp: Some(root.join("temp")),
                local_app_data: Some(root.join("local")),
            },
            ..Context::at(100)
        };
        for (base, path, expected) in [
            ("home", "records".to_owned(), root.join("home/records")),
            (
                "scan_root",
                "home/records".to_owned(),
                root.join("home/records"),
            ),
            ("cache", "records".to_owned(), root.join("cache/records")),
            ("temp", "records".to_owned(), root.join("temp/records")),
            (
                "local_app_data",
                "records".to_owned(),
                root.join("local/records"),
            ),
            (
                "absolute",
                root.join("home/records").to_string_lossy().into_owned(),
                root.join("home/records"),
            ),
            ("temp", "".to_owned(), root.join("temp")),
        ] {
            let doc = serde_json::json!({"version":1,"rules":[{"id":"example","title":"Review","detail":"Keep valuable data","subject":"directory","location":{"base":base,"path":path},"action":"review-only"}]});
            let loaded =
                crate::pack::parse(&doc.to_string(), std::path::Path::new("example.json"), &[])
                    .unwrap();
            assert!(!loaded.has_problems(), "{base}: {:?}", loaded.problems());
            let found = loaded.rules[0].evaluate(&tree, &context);
            assert_eq!(found.len(), 1, "{base}");
            assert_eq!(tree.path_of(found[0].nodes[0]), expected, "{base}");
            assert_eq!(found[0].action, Suggested::ReviewOnly);
            let mut destructive = doc.clone();
            destructive["rules"][0]["action"] = serde_json::json!("trash");
            assert!(
                crate::pack::parse(
                    &destructive.to_string(),
                    std::path::Path::new("bad.json"),
                    &[]
                )
                .unwrap()
                .rules
                .is_empty()
            );
        }
        let missing = Location {
            base: LocationBase::Cache,
            path: "records".into(),
        };
        assert!(missing.target(&tree, &Context::at(100)).is_none());
        let outside = crate::pack::PackRule {
            id: "outside".into(),
            title: "Outside".into(),
            detail: "Outside scan".into(),
            subject: "directory".into(),
            matcher: crate::pack::Matcher::Location(
                Location {
                    base: LocationBase::Absolute,
                    path: root
                        .parent()
                        .unwrap()
                        .join("outside")
                        .to_string_lossy()
                        .into_owned(),
                }
                .validate()
                .unwrap(),
            ),
            action: Suggested::ReviewOnly,
        };
        assert!(outside.evaluate(&tree, &context).is_empty());
    }

    #[test]
    fn obsolete_key_is_rejected_without_an_alias() {
        let doc = r#"{"version":1,"rules":[{"id":"old","title":"Old","detail":"Old syntax","subject":"directory","under-home":"records","action":"review-only"}]}"#;
        assert!(crate::pack::parse(doc, std::path::Path::new("old.json"), &[]).is_err());
    }

    #[test]
    fn shared_authoring_corpus_matches_native_validation() {
        for (line, text) in include_str!("../tests/fixtures/location_cases.jsonl")
            .lines()
            .enumerate()
        {
            let case: serde_json::Value = serde_json::from_str(text).unwrap();
            let result = serde_json::from_value::<Location>(case["location"].clone())
                .map_err(|_| "invalid location")
                .and_then(Location::validate);
            assert_eq!(
                result.is_ok(),
                case["valid"].as_bool().unwrap(),
                "case {}: {text}",
                line + 1
            );
            if let Ok(location) = result {
                assert_eq!(location.path, case["normalized"].as_str().unwrap());
            }
        }
    }

    #[test]
    fn relative_and_absolute_locations_have_portable_lexical_limits() {
        for path in ["", "logs", "folder with spaces/cache", r"folder\cache"] {
            assert!(
                Location {
                    base: LocationBase::ScanRoot,
                    path: path.into()
                }
                .validate()
                .is_ok(),
                "{path}"
            );
        }
        for path in [
            "../outside",
            "a/../outside",
            "a/./b",
            "a//b",
            "/root",
            "C:/Windows",
            "C:Windows",
            " logs",
            "logs ",
        ] {
            assert!(
                Location {
                    base: LocationBase::Home,
                    path: path.into()
                }
                .validate()
                .is_err(),
                "{path}"
            );
        }
        for path in [
            "/",
            "/var/log",
            "C:/",
            r"C:\ProgramData\Example",
            r"\\server\share\logs",
        ] {
            assert!(
                Location {
                    base: LocationBase::Absolute,
                    path: path.into()
                }
                .validate()
                .is_ok(),
                "{path}"
            );
        }
        for path in [
            "",
            "relative/path",
            "C:Windows",
            "//server",
            "//server/../logs",
            "C:/a/../b",
        ] {
            assert!(
                Location {
                    base: LocationBase::Absolute,
                    path: path.into()
                }
                .validate()
                .is_err(),
                "{path}"
            );
        }
        let path = vec!["a"; 33].join("/");
        assert!(
            Location {
                base: LocationBase::Home,
                path
            }
            .validate()
            .is_err()
        );
    }
}
