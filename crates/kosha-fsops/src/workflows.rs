// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Saved cleanup intent shared by the desktop and command line.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use kosha_core::monitoring::Location;

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_TARGETS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Workflow {
    location: Location,
    targets: Vec<PathBuf>,
}

impl Workflow {
    /// Build a fresh, read-only review from the originally pinned location.
    pub fn preview(&self) -> io::Result<crate::ActionPlan> {
        if !self.location.is_current() {
            return Err(io::Error::other(
                "The original workflow location is unavailable or changed.",
            ));
        }
        let tree = kosha_core::Scanner::new(kosha_core::ScanOptions {
            use_mft: false,
            ..Default::default()
        })
        .scan(self.location.root())
        .map_err(io::Error::other)?;
        let plan = crate::ActionPlan::preview_workflow(&self.location, &tree, &self.targets)
            .map_err(io::Error::other)?;
        if plan.cloud_risk().is_some() {
            return Err(io::Error::other(
                "A workflow target is stored in the cloud. Review it from a live scan.",
            ));
        }
        Ok(plan)
    }

    pub fn new(root: &Path, targets: Vec<PathBuf>) -> io::Result<Self> {
        let location = Location::pin(root)
            .ok_or_else(|| invalid("Select a verified local folder or drive."))?;
        let workflow = Self { location, targets };
        workflow.validate()?;
        Ok(workflow)
    }

    pub fn location(&self) -> &Location {
        &self.location
    }

    pub fn targets(&self) -> &[PathBuf] {
        &self.targets
    }

    pub fn with_targets(&self, targets: Vec<PathBuf>) -> io::Result<Self> {
        let workflow = Self {
            location: self.location.clone(),
            targets,
        };
        workflow.validate()?;
        Ok(workflow)
    }

    fn validate(&self) -> io::Result<()> {
        let mut seen = std::collections::HashSet::new();
        if self.targets.is_empty()
            || self.targets.len() > MAX_TARGETS
            || self.targets.iter().any(|target| {
                !valid_relative_target(target) || target.to_str().is_none() || !seen.insert(target)
            })
        {
            return Err(invalid("Use 1 to 1000 distinct relative file paths."));
        }
        Ok(())
    }
}

pub(crate) fn valid_relative_target(path: &Path) -> bool {
    path.components().next().is_some()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Store {
    version: u32,
    workflows: BTreeMap<String, Workflow>,
}

pub fn directory() -> Option<PathBuf> {
    Some(kosha_core::dirs::data_dir()?.join("cleanup-workflows"))
}

pub fn list(dir: &Path) -> io::Result<BTreeMap<String, Workflow>> {
    Ok(read(dir)?.workflows)
}

/// Create a workflow or replace the exact definition previously read by an editor.
pub fn save(
    dir: &Path,
    name: &str,
    workflow: Workflow,
    expected: Option<&Workflow>,
) -> io::Result<()> {
    validate_name(name)?;
    workflow.validate()?;
    fs::create_dir_all(dir)?;
    let lock = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".write.lock"))?;
    lock.try_lock().map_err(io::Error::from)?;
    let mut store = read(dir)?;
    if store.workflows.get(name) != expected {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The workflow already exists or changed. Reload it before saving.",
        ));
    }
    store.workflows.insert(name.to_owned(), workflow);
    let bytes = serde_json::to_vec_pretty(&store)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("The saved workflow store exceeds 1 MiB."));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(dir.join("workflows.json"))?;
    Ok(())
}

fn read(dir: &Path) -> io::Result<Store> {
    let file = match File::open(dir.join("workflows.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Store {
                version: VERSION,
                workflows: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("The saved workflow store exceeds 1 MiB."));
    }
    let store: Store = serde_json::from_slice(&bytes)?;
    if store.version != VERSION {
        return Err(invalid("Unsupported cleanup workflow store version."));
    }
    for (name, workflow) in &store.workflows {
        validate_name(name)?;
        workflow.validate()?;
    }
    Ok(store)
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 80
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(invalid(
            "Use a workflow name from 1 to 80 bytes without control characters.",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
