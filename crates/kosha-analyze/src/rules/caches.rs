// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Directories that exist to be regenerated.

use std::ffi::OsStr;

use kosha_core::{NodeId, SizeTree};
use rustc_hash::FxHashSet;

use crate::context::Context;
use crate::insight::{Confidence, Insight, RuleId, Severity, Suggested};
use crate::rules::Rule;
use crate::rules::util;

/// A directory name, and the sibling file that proves what it is.
///
/// The marker is the whole point. Thousands of directories are named `build`;
/// a `build` next to a `CMakeLists.txt` is a build directory, and one next to
/// nothing is somebody's source folder. Reporting the latter would teach users
/// to ignore the panel, which is worse than reporting nothing.
struct Marker {
    dir: &'static str,
    /// Any one of these beside it is sufficient. Empty means the directory name
    /// is unambiguous on its own.
    siblings: &'static [&'static str],
    tool: &'static str,
}

const MARKERS: &[Marker] = &[
    Marker {
        dir: "node_modules",
        siblings: &["package.json"],
        tool: "npm",
    },
    Marker {
        dir: "target",
        siblings: &["Cargo.toml"],
        tool: "Cargo",
    },
    Marker {
        dir: "build",
        siblings: &[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "CMakeLists.txt",
        ],
        tool: "Gradle or CMake",
    },
    Marker {
        dir: "dist",
        siblings: &["package.json"],
        tool: "a JavaScript bundler",
    },
    Marker {
        dir: ".next",
        siblings: &["package.json"],
        tool: "Next.js",
    },
    Marker {
        dir: "Pods",
        siblings: &["Podfile"],
        tool: "CocoaPods",
    },
    Marker {
        dir: "__pycache__",
        siblings: &[],
        tool: "Python",
    },
    Marker {
        dir: ".pytest_cache",
        siblings: &[],
        tool: "pytest",
    },
    Marker {
        dir: ".mypy_cache",
        siblings: &[],
        tool: "mypy",
    },
    Marker {
        dir: ".turbo",
        siblings: &[],
        tool: "Turborepo",
    },
];

pub struct BuildArtifacts;

impl Rule for BuildArtifacts {
    fn id(&self) -> RuleId {
        RuleId::new("build-artifacts")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let mut claimed: FxHashSet<NodeId> = FxHashSet::default();
        let mut found: Vec<NodeId> = Vec::new();

        // Arena order is breadth-first, so a parent is always visited before
        // its children and `claimed` is populated by the time we need it.
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let node = tree.node(id);
            if !node.is_dir() || !util::is_countable(tree, id) {
                continue;
            }
            let name = tree.name(id);
            let Some(marker) = MARKERS.iter().find(|m| OsStr::new(m.dir) == name) else {
                continue;
            };
            let proven = marker.siblings.is_empty()
                || marker
                    .siblings
                    .iter()
                    .any(|s| tree.has_sibling_named(id, OsStr::new(s)));
            if !proven {
                continue;
            }
            // A node_modules inside a node_modules is already inside the bytes
            // we are about to report.
            if util::has_claimed_ancestor(tree, id, &claimed) {
                continue;
            }
            claimed.insert(id);
            found.push(id);
        }

        if found.is_empty() {
            return Vec::new();
        }

        let reclaimable = util::total_allocated(tree, &found);
        if reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        let tools: FxHashSet<&str> = found
            .iter()
            .filter_map(|&id| {
                let name = tree.name(id);
                MARKERS
                    .iter()
                    .find(|m| OsStr::new(m.dir) == name)
                    .map(|m| m.tool)
            })
            .collect();
        let mut tools: Vec<&str> = tools.into_iter().collect();
        tools.sort_unstable();

        vec![Insight {
            rule: self.id(),
            title: "Build output".into(),
            subject: "project".into(),
            detail: format!(
                "Regenerated output from {}. Every one of these sits beside the \
                 manifest that produced it, and rebuilding recreates it.",
                tools.join(", "),
            ),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            confidence: Confidence::Certain,
            action: Suggested::Trash,
            severity: Severity::Important,
        }]
    }
}

/// A well-known cache location, matched by its path relative to the home
/// directory.
struct CacheDir {
    rel: &'static str,
    label: &'static str,
    confidence: Confidence,
}

const CACHE_DIRS: &[CacheDir] = &[
    CacheDir {
        rel: ".cargo/registry/cache",
        label: "Cargo's downloaded crate archives",
        confidence: Confidence::Certain,
    },
    CacheDir {
        rel: ".cargo/registry/src",
        label: "Cargo's unpacked crate sources",
        confidence: Confidence::Certain,
    },
    CacheDir {
        rel: ".npm/_cacache",
        label: "npm's content-addressable cache",
        confidence: Confidence::Certain,
    },
    CacheDir {
        rel: ".gradle/caches",
        label: "Gradle's build cache",
        confidence: Confidence::Certain,
    },
    CacheDir {
        rel: ".cache",
        label: "the XDG cache directory",
        confidence: Confidence::Certain,
    },
    CacheDir {
        rel: "Library/Caches",
        label: "the macOS application cache directory",
        confidence: Confidence::Certain,
    },
    CacheDir {
        rel: "AppData/Local/Temp",
        label: "the Windows per-user temp directory",
        confidence: Confidence::Certain,
    },
    CacheDir {
        rel: ".nuget/packages",
        label: "NuGet's global package folder",
        confidence: Confidence::Likely,
    },
    CacheDir {
        rel: "go/pkg/mod",
        label: "the Go module cache",
        confidence: Confidence::Likely,
    },
    CacheDir {
        rel: ".m2/repository",
        label: "Maven's local repository",
        confidence: Confidence::Likely,
    },
    CacheDir {
        rel: ".pub-cache",
        label: "the Dart/Flutter package cache",
        confidence: Confidence::Likely,
    },
];

pub struct PackageManagerCache;

impl Rule for PackageManagerCache {
    fn id(&self) -> RuleId {
        RuleId::new("package-manager-cache")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let Some(home) = ctx.home.as_ref() else {
            return Vec::new();
        };

        // Only build a path for nodes whose name could possibly end a pattern.
        // Path reconstruction is cheap but not free, and this filter takes it
        // from every directory in the tree down to a handful.
        let mut insights = Vec::new();
        for pattern in CACHE_DIRS {
            let target = home.join(pattern.rel);
            let Some(leaf) = target.file_name() else {
                continue;
            };

            for index in 0..tree.len() {
                let id = NodeId::from_index(index);
                let node = tree.node(id);
                if !node.is_dir() || tree.name(id) != leaf || !util::is_countable(tree, id) {
                    continue;
                }
                if tree.path_of(id) != target {
                    continue;
                }
                let reclaimable = node.allocated;
                if reclaimable < ctx.min_reclaimable {
                    continue;
                }
                insights.push(Insight {
                    rule: self.id(),
                    title: pattern.rel.to_string(),
                    subject: "directory".into(),
                    detail: format!(
                        "This is {}. Emptying it costs a re-download the next time \
                         the tool needs something, and nothing else.",
                        pattern.label
                    ),
                    nodes: vec![id],
                    superseded: 0,
                    reclaimable,
                    confidence: pattern.confidence,
                    action: Suggested::EmptyContents,
                    severity: Severity::Notice,
                });
            }
        }
        insights
    }
}

/// Directory names browsers use for regenerable cache data.
///
/// Deliberately excludes `Service Worker`, `Local Storage`, `IndexedDB` and
/// `Session Storage`: those hold offline application data, and emptying them
/// logs people out of things or loses work. "Cache" in the name is not enough
/// on its own.
const BROWSER_CACHE_DIRS: &[&str] = &[
    "Cache",
    "Cache_Data",
    "cache2",
    "Code Cache",
    "GPUCache",
    "ShaderCache",
    "GrShaderCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "startupCache",
    "thumbnails",
];

/// Vendor directories that identify a browser profile tree. A cache directory
/// only counts when one of these is above it.
const BROWSER_MARKERS: &[&str] = &[
    "Chrome",
    "Chromium",
    "chromium",
    "google-chrome",
    "Edge",
    "microsoft-edge",
    "Firefox",
    "Mozilla",
    "BraveSoftware",
    "Vivaldi",
    "Opera",
    "Opera Software",
    "Safari",
    "Arc",
    "Zen",
];

pub struct BrowserCache;

impl Rule for BrowserCache {
    fn id(&self) -> RuleId {
        RuleId::new("browser-cache")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        let found = collect_cache_dirs(tree, BROWSER_CACHE_DIRS, |tree, id| {
            util::under_directory_named(tree, id, BROWSER_MARKERS)
        });

        let reclaimable = util::total_allocated(tree, &found);
        if found.is_empty() || reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.id(),
            title: "Browser cache".into(),
            subject: "cache directory".into(),
            detail: "Cache directories under browser profiles. Browsers refill these \
                 on demand; emptying one costs a slower first page load and \
                 nothing else. Logins, history and offline app data live \
                 elsewhere and are not touched."
                .into(),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            confidence: Confidence::Certain,
            action: Suggested::EmptyContents,
            severity: Severity::Notice,
        }]
    }
}

/// Generic cache directory names, for applications that are not browsers.
const APP_CACHE_DIRS: &[&str] = &["Cache", "Caches", "CachedData", "cache", ".cache"];

pub struct AppCache;

impl Rule for AppCache {
    fn id(&self) -> RuleId {
        RuleId::new("app-cache")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        // Browser caches are reported by their own rule with higher confidence
        // and a better explanation, so exclude them here rather than relying on
        // the engine to untangle the overlap afterwards.
        let found = collect_cache_dirs(tree, APP_CACHE_DIRS, |tree, id| {
            !util::under_directory_named(tree, id, BROWSER_MARKERS)
        });

        let reclaimable = util::total_allocated(tree, &found);
        if found.is_empty() || reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.id(),
            title: "Application caches".into(),
            subject: "directory".into(),
            detail: "Directories named for caching. Most applications rebuild these \
                 without being asked, but a few keep real state in a directory \
                 called \"cache\" — worth a glance at the list before emptying."
                .into(),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            // Deliberately not Certain: the name is a convention, not a
            // guarantee, and this rule has no marker file to lean on.
            confidence: Confidence::Likely,
            action: Suggested::EmptyContents,
            severity: Severity::Notice,
        }]
    }
}

/// Find directories matching `names` that satisfy `accept`, skipping any nested
/// inside one already found.
///
/// The nesting check is what stops a browser's `Cache/Code Cache` being counted
/// both on its own and inside its parent.
fn collect_cache_dirs(
    tree: &SizeTree,
    names: &[&str],
    accept: impl Fn(&SizeTree, NodeId) -> bool,
) -> Vec<NodeId> {
    let mut claimed: FxHashSet<NodeId> = FxHashSet::default();
    let mut found = Vec::new();

    // Arena order is breadth-first, so an ancestor is always visited first and
    // `claimed` is populated by the time a descendant is considered.
    for index in 0..tree.len() {
        let id = NodeId::from_index(index);
        if !tree.node(id).is_dir() || !util::is_countable(tree, id) {
            continue;
        }
        if !util::named(tree, id, names) || !accept(tree, id) {
            continue;
        }
        if util::has_claimed_ancestor(tree, id, &claimed) {
            continue;
        }
        claimed.insert(id);
        found.push(id);
    }
    found
}

pub struct TrashNotEmptied;

impl Rule for TrashNotEmptied {
    fn id(&self) -> RuleId {
        RuleId::new("trash-not-emptied")
    }

    fn evaluate(&self, tree: &SizeTree, ctx: &Context) -> Vec<Insight> {
        // `$Recycle.Bin` is unambiguous by name; the Unix trash directories are
        // matched by name too, since both sit at a known depth under $HOME and
        // no other directory is called `.Trash`.
        const TRASH_DIRS: &[&str] = &["$Recycle.Bin", ".Trash", ".Trashes"];

        let mut found = Vec::new();
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            if !tree.node(id).is_dir() || !util::is_countable(tree, id) {
                continue;
            }
            if util::named(tree, id, TRASH_DIRS) || is_xdg_trash(tree, id) {
                found.push(id);
            }
        }

        let reclaimable = util::total_allocated(tree, &found);
        if found.is_empty() || reclaimable < ctx.min_reclaimable {
            return Vec::new();
        }

        vec![Insight {
            rule: self.id(),
            title: "Waiting in the trash".into(),
            subject: "trash directory".into(),
            detail: "Already deleted, still occupying the disk. Empty it from your \
                 file manager — the system's own command understands the trash's \
                 bookkeeping, and doing it any other way can leave orphaned \
                 metadata behind."
                .into(),
            nodes: util::sort_by_size(tree, found),
            superseded: 0,
            reclaimable,
            confidence: Confidence::Certain,
            // ReviewOnly on purpose. Emptying the trash is a first-class OS
            // operation with semantics we would have to reimplement badly, and
            // the guards refuse `$Recycle.Bin` anyway.
            action: Suggested::ReviewOnly,
            severity: Severity::Notice,
        }]
    }
}

/// `~/.local/share/Trash`, the freedesktop.org location.
fn is_xdg_trash(tree: &SizeTree, id: NodeId) -> bool {
    if !util::named(tree, id, &["Trash"]) {
        return false;
    }
    let path = tree.path_of(id);
    let mut components = path.components().rev().skip(1);
    matches!(components.next(), Some(c) if c.as_os_str() == "share")
        && matches!(components.next(), Some(c) if c.as_os_str() == ".local")
}
