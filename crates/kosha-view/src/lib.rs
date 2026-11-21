// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Inert metadata disclosure, not a process sandbox. No ambient filesystem I/O.
//! Only newly constructed shared records cross this boundary; original arenas,
//! paths, ownership tables and directory allocations never do.

use std::{collections::HashSet, ffi::OsStr, io::Read, path::PathBuf};

use kosha_core::{Flags, NameArena, Node, NodeId, Owners, ScanStats, SizeTree};
use kosha_query::{Expr, Field, Flag, Query};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "kosha.view.v1";
pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 250_000;
pub const MAX_DEPTH: u8 = 128;
pub const MAX_NAMES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    pub modified: bool,
    pub created: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Directory,
    File,
    Link,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub parent: u32,
    pub name: String,
    pub kind: Kind,
    pub allocated: u64,
    pub apparent: u64,
    pub unreadable: bool,
    pub boundary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    /// These booleans describe the source, not counts/names of hidden entries.
    pub source_incomplete: bool,
    pub source_access_limited: bool,
    pub lower_bound: bool,
    pub allocation_exact: bool,
    pub scope: String,
    pub totals: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub schema: String,
    pub alias: String,
    pub observed_unix: u64,
    pub fields: Fields,
    pub coverage: Coverage,
    pub entries: Vec<Entry>,
}

/// An immutable, validated view plus a freshly rebuilt query arena. Its root is
/// a logical alias, never authority to open an original filesystem path.
pub struct Prepared {
    view: View,
    tree: SizeTree,
}

pub struct Selection<'a> {
    pub root: &'a str,
    pub alias: &'a str,
    pub excludes: &'a [String],
    pub fields: Fields,
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name.chars().any(|c| c.is_control()
            || matches!(c, '/' | '\\' | ':' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

fn alias_ok(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 64
        && alias.as_bytes()[0].is_ascii_alphanumeric()
        && alias
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn components(path: &str) -> Result<Vec<&str>, String> {
    if path == "." {
        return Ok(Vec::new());
    }
    if path.len() > 4096 || path.split('/').any(|name| !name_ok(name)) {
        return Err("selection_invalid: use portable relative components separated by /".into());
    }
    Ok(path.split('/').collect())
}

fn find(tree: &SizeTree, root: NodeId, path: &str) -> Result<NodeId, String> {
    let mut id = root;
    for name in components(path)? {
        id = tree
            .children(id)
            .find(|child| tree.name(*child) == OsStr::new(name))
            .ok_or("selection_missing: a selected recorded path is unavailable")?;
    }
    Ok(id)
}

/// Owner-side projection from an explicitly authorized already-loaded source.
pub fn publish(
    tree: &SizeTree,
    observed_unix: u64,
    selection: Selection<'_>,
) -> Result<View, String> {
    if !alias_ok(selection.alias) || selection.excludes.len() > 256 {
        return Err("selection_invalid: alias or exclusion budget".into());
    }
    let root = find(tree, tree.root(), selection.root)?;
    if !tree.node(root).is_dir() {
        return Err("selection_invalid: root must be a recorded directory".into());
    }
    let mut excluded = HashSet::new();
    for path in selection.excludes {
        let id = find(tree, root, path)?;
        if id == root {
            return Err("selection_invalid: cannot exclude the shared root".into());
        }
        excluded.insert(id.index());
    }
    let mut queue = vec![(root, 0u32)];
    let mut entries: Vec<Entry> = Vec::new();
    let mut names = 0usize;
    let mut position = 0;
    while position < queue.len() {
        let (id, parent) = queue[position];
        let node = tree.node(id);
        let name = if position == 0 {
            selection.alias.to_owned()
        } else {
            // A name the snapshot reader had to replace is a rewrite, not the
            // recorded name, and is refused like any other unportable name.
            let recorded = if tree.name_replaced(id) {
                None
            } else {
                tree.name(id).to_str()
            };
            recorded
                .filter(|s| name_ok(s))
                .ok_or("selection_invalid: selected name is not portable UTF-8")?
                .to_owned()
        };
        names += name.len();
        if names > MAX_NAMES {
            return Err("view_limit: name budget exceeded".into());
        }
        if !node.is_dir() && node.flags.contains(Flags::HARDLINK_DUP) {
            return Err("accounting_unsupported: source folded selected hardlinks; take a --no-hardlinks snapshot".into());
        }
        entries.push(Entry {
            parent,
            name,
            kind: if node.is_dir() {
                Kind::Directory
            } else if node.is_symlink() {
                Kind::Link
            } else {
                Kind::File
            },
            allocated: if node.is_dir() { 0 } else { node.allocated },
            apparent: if node.is_dir() { 0 } else { node.logical },
            unreadable: node.flags.contains(Flags::UNREADABLE),
            boundary: node.flags.contains(Flags::OTHER_DEVICE),
            modified: (selection.fields.modified && node.mtime != 0).then_some(node.mtime),
            created: if selection.fields.created {
                tree.created(id)
            } else {
                None
            },
        });
        for child in tree.children(id) {
            if excluded.contains(&child.index()) {
                continue;
            }
            if queue.len() == MAX_ENTRIES {
                return Err("view_limit: entry budget exceeded".into());
            }
            queue.push((child, position as u32));
        }
        position += 1;
    }
    for i in (1..entries.len()).rev() {
        let parent = entries[i].parent as usize;
        entries[parent].allocated = entries[parent]
            .allocated
            .checked_add(entries[i].allocated)
            .ok_or("view_invalid: allocated total overflow")?;
        entries[parent].apparent = entries[parent]
            .apparent
            .checked_add(entries[i].apparent)
            .ok_or("view_invalid: apparent total overflow")?;
    }
    let view = View {
        schema: SCHEMA.into(),
        alias: selection.alias.into(),
        observed_unix,
        fields: selection.fields,
        coverage: Coverage {
            source_incomplete: tree.is_incomplete(),
            source_access_limited: tree.stats().unreadable > 0 || tree.root_unreadable(),
            lower_bound: tree.is_partial()
                || tree.is_incomplete()
                || entries.iter().any(|e| e.unreadable || e.boundary),
            allocation_exact: tree.stats().exact_allocated,
            scope: "owner_selected_metadata".into(),
            totals: "shared_non_directory_entries_only".into(),
        },
        entries,
    };
    // Validate the same invariants as an imported view before exposing bytes.
    rebuild(&view)?;
    Ok(view)
}

pub fn read(reader: impl Read) -> Result<Prepared, String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "view_io: could not read the supplied artifact")?;
    if bytes.len() > MAX_BYTES {
        return Err("view_limit: artifact exceeds 64 MiB".into());
    }
    let view: View = serde_json::from_slice(&bytes)
        .map_err(|_| "view_invalid: expected the strict kosha.view.v1 schema")?;
    prepare(view)
}

pub fn encode(view: &View) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(view).map_err(|_| "view_invalid: cannot encode artifact")?;
    if bytes.len() > MAX_BYTES {
        return Err("view_limit: artifact exceeds 64 MiB".into());
    }
    Ok(bytes)
}

pub fn prepare(view: View) -> Result<Prepared, String> {
    let tree = rebuild(&view)?;
    Ok(Prepared { view, tree })
}

fn rebuild(view: &View) -> Result<SizeTree, String> {
    if view.schema != SCHEMA
        || !alias_ok(&view.alias)
        || view.entries.is_empty()
        || view.entries.len() > MAX_ENTRIES
        || view.coverage.scope != "owner_selected_metadata"
        || view.coverage.totals != "shared_non_directory_entries_only"
    {
        return Err("view_invalid: schema, scope or entry budget".into());
    }
    let root = &view.entries[0];
    if !view.coverage.lower_bound
        && (view.coverage.source_incomplete
            || view.coverage.source_access_limited
            || view.entries.iter().any(|e| e.unreadable || e.boundary))
    {
        return Err("view_invalid: coverage contradicts recorded limitations".into());
    }
    if root.parent != 0 || root.name != view.alias || root.kind != Kind::Directory {
        return Err("view_invalid: root must be the logical alias directory".into());
    }
    let mut arena = NameArena::with_capacity(0);
    let mut nodes: Vec<Node> = Vec::with_capacity(view.entries.len());
    let mut created = Vec::new();
    let mut previous_parent = 0;
    let mut previous_name: Option<&str> = None;
    let mut name_bytes = 0usize;
    let mut path_lengths = Vec::with_capacity(view.entries.len());
    for (i, entry) in view.entries.iter().enumerate() {
        let parent = entry.parent as usize;
        if !name_ok(&entry.name)
            || (i > 0
                && (parent >= i
                    || parent < previous_parent
                    || view.entries[parent].kind != Kind::Directory))
            || (!view.fields.modified && entry.modified.is_some())
            || (!view.fields.created && entry.created.is_some())
            || entry.modified == Some(0)
            || entry.created == Some(u64::MAX)
            || (entry.boundary && entry.kind != Kind::Directory)
        {
            return Err("view_invalid: entry, graph or undisclosed field".into());
        }
        if i > 0
            && parent == previous_parent
            && previous_name.is_some_and(|name| name >= entry.name.as_str())
        {
            return Err("view_invalid: siblings must have unique sorted names".into());
        }
        if i > 0 {
            previous_name = Some(&entry.name);
            previous_parent = parent;
        }
        name_bytes += entry.name.len();
        if name_bytes > MAX_NAMES {
            return Err("view_limit: name budget".into());
        }
        let path_length = if i == 0 {
            entry.name.len()
        } else {
            path_lengths[parent] + 1 + entry.name.len()
        };
        if path_length > 4096 {
            return Err("view_limit: logical path exceeds 4096 bytes".into());
        }
        path_lengths.push(path_length);
        let depth = if i == 0 {
            0
        } else {
            nodes[parent]
                .depth
                .checked_add(1)
                .filter(|d| *d <= MAX_DEPTH)
                .ok_or("view_limit: depth exceeds 128")?
        };
        let (name_off, name_len) = arena.push(OsStr::new(&entry.name));
        let mut flags = Flags::default();
        if entry.kind == Kind::Directory {
            flags.insert(Flags::DIR);
        }
        if entry.kind == Kind::Link {
            flags.insert(Flags::SYMLINK);
        }
        if entry.unreadable {
            flags.insert(Flags::UNREADABLE);
        }
        if entry.boundary {
            flags.insert(Flags::OTHER_DEVICE);
        }
        if entry.kind == Kind::Directory && view.coverage.lower_bound {
            flags.insert(Flags::PARTIAL);
        }
        nodes.push(Node {
            logical: if entry.kind == Kind::Directory {
                0
            } else {
                entry.apparent
            },
            allocated: if entry.kind == Kind::Directory {
                0
            } else {
                entry.allocated
            },
            parent: entry.parent,
            first_child: 0,
            child_count: 0,
            subtree_files: u32::from(entry.kind != Kind::Directory),
            name_off,
            mtime: entry.modified.unwrap_or(0),
            name_len,
            flags,
            depth,
        });
        if view.fields.created {
            created.push(entry.created.unwrap_or(u64::MAX));
        }
        if i > 0 {
            if nodes[parent].child_count == 0 {
                nodes[parent].first_child = i as u32;
            }
            nodes[parent].child_count += 1;
        }
    }
    for i in (1..nodes.len()).rev() {
        let parent = nodes[i].parent as usize;
        nodes[parent].logical = nodes[parent]
            .logical
            .checked_add(nodes[i].logical)
            .ok_or("view_invalid: total overflow")?;
        nodes[parent].allocated = nodes[parent]
            .allocated
            .checked_add(nodes[i].allocated)
            .ok_or("view_invalid: total overflow")?;
        nodes[parent].subtree_files += nodes[i].subtree_files;
    }
    for (node, entry) in nodes.iter().zip(&view.entries) {
        if node.logical != entry.apparent || node.allocated != entry.allocated {
            return Err(
                "view_invalid: directory totals include unshared or inconsistent bytes".into(),
            );
        }
    }
    let stats = ScanStats {
        entries: nodes.len() as u64,
        partial: view.coverage.source_access_limited,
        cancelled: view.coverage.source_incomplete,
        ..ScanStats::default()
    };
    let tree = SizeTree::from_parts(
        nodes,
        arena,
        Owners::default(),
        PathBuf::from(&view.alias),
        stats,
    )
    .with_creation_times(created);
    Ok(tree)
}

impl Prepared {
    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn check_query(&self, query: &Query) -> Result<(), String> {
        fn check(expr: &Expr, fields: Fields) -> bool {
            let allowed = |field: Field| match field {
                Field::Owner => false,
                Field::Modified | Field::Age => fields.modified,
                Field::Created => fields.created,
                _ => true,
            };
            match expr {
                Expr::And(a, b) | Expr::Or(a, b) => check(a, fields) && check(b, fields),
                Expr::Not(a) => check(a, fields),
                Expr::Compare { field, .. } | Expr::In { field, .. } => allowed(*field),
                Expr::Flag(flag) => matches!(
                    flag,
                    Flag::Dir | Flag::File | Flag::Link | Flag::Empty | Flag::Unreadable
                ),
                Expr::NameContains(_) => true,
            }
        }
        if check(query.expression(), self.view.fields) {
            Ok(())
        } else {
            Err("operation_not_granted: query needs metadata not shared in this view".into())
        }
    }

    pub fn matches(&self, query: &Query, index: usize, now: u32) -> bool {
        if index >= self.view.entries.len() {
            return false;
        }
        if query.needs_path() {
            query.matches_with_path(
                &self.tree,
                NodeId::from_index(index),
                now,
                &self.path(index),
            )
        } else {
            query.matches(&self.tree, NodeId::from_index(index), now)
        }
    }

    /// Logical portable path, solely from shared names. Never a live path.
    pub fn path(&self, mut index: usize) -> String {
        let mut parts = Vec::new();
        loop {
            let entry = &self.view.entries[index];
            parts.push(entry.name.as_str());
            if index == 0 {
                break;
            }
            index = entry.parent as usize;
        }
        parts.reverse();
        parts.join("/")
    }
}
