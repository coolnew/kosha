// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Turning a flat table of MFT records into a [`SizeTree`].
//!
//! The MFT is not a tree. It is a million records in no particular order, each
//! naming its parent, and the shape has to be rebuilt afterwards. That is the
//! trade the strategy makes: one sequential read instead of a directory open
//! per directory, at the cost of a join.
//!
//! The join has to produce an arena with exactly the invariants the ordinary
//! walk produces, because everything downstream — aggregation, the treemap, the
//! query engine, export — is written against them:
//!
//! * node 0 is the root, and is its own parent;
//! * a directory's children occupy a contiguous run of indices;
//! * a child's index is greater than its parent's, so the single reverse pass
//!   in [`crate::scan::aggregate`] sees every child before its parent;
//! * children are sorted by encoded name bytes, so two scans of an unchanged
//!   volume produce identical arenas.
//!
//! A breadth-first walk of the parent map gives all four for free, which is why
//! this is breadth-first rather than the recursion the shape invites.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::names::NameArena;
use crate::node::{Flags, Node};
use crate::owner::{OwnerId, Owners};
use crate::scan::{Progress, ScanOptions};
use crate::tree::{ScanStats, SizeTree, Strategy};

use super::record::{NAMESPACE_DOS, Name, Summary};
use super::security::Security;

/// Everything one pass over the MFT produced.
pub struct Table {
    /// Indexed by record number. A record that was never read — past the end
    /// of `$MFT`, or in a sparse hole in it — is `Summary::default()`, which is
    /// not in use and therefore never reached.
    pub records: Vec<Summary>,
    /// Every usable name on the volume, in record order.
    pub names: Vec<Name>,
    /// Which record each of those names belongs to, parallel to `names`.
    ///
    /// Carried beside the name rather than inside it because `Name` is already
    /// the width of its own padding, and there are as many of these as there
    /// are files on the volume.
    pub owners: Vec<u32>,
    /// The bytes those names point into.
    pub name_bytes: Vec<u8>,
    /// Where `$Secure`'s `$SDS` stream lives, noticed as its records went past.
    ///
    /// Captured during the one pass rather than found afterwards, because
    /// finding it afterwards means computing the offset of a particular record
    /// inside a fragmented `$MFT` — arithmetic the pass has already done. It is
    /// a few hundred bytes, so it is taken whether or not anybody asked for
    /// owners; reading the stream it points at is the conditional part.
    pub security_stream: SecurityStream,
}

/// Where `$Secure`'s `$SDS` stream lives.
///
/// **Not in record 9.** On this machine's `C:`, `$Secure`'s own record holds
/// only `$STANDARD_INFORMATION`, an `$ATTRIBUTE_LIST`, a `$FILE_NAME` and the
/// two resident index roots — there is no `$DATA` in it at all. `$SDS` is in
/// record 2022 from cluster 0 and in record 83680 from cluster 249, and the
/// indexes are in a third record again. So the stream is gathered from every
/// record that names `$Secure` as its base, and put back in order afterwards.
///
/// Both halves of that matter. Looking only at record 9 finds nothing and
/// attributes every file on the volume to nobody — which is loud. Taking only
/// the first fragment found reads 1 MiB of a 2.87 MiB table, which is silent:
/// most files get an owner, some do not, and nothing says which.
#[derive(Clone, Default, Debug)]
pub struct SecurityStream {
    /// In the order the records went past, which is not the order of the
    /// stream. Sorted on the way in to [`read_security`](super::volume).
    pub fragments: Vec<SecurityFragment>,
    /// The stream's real length, from whichever fragment starts at cluster
    /// zero. Zero if that fragment was never seen, which means there is nothing
    /// worth reading.
    pub logical: u64,
}

/// One attribute record's worth of `$SDS`.
#[derive(Clone, Default, Debug)]
pub struct SecurityFragment {
    /// The first cluster of the stream this fragment covers.
    pub start_vcn: u64,
    /// The encoded run list.
    pub runs: Vec<u8>,
}

/// DOS attribute bits, repeated here rather than imported from
/// `platform::windows`: this module compiles on every platform so that the join
/// stays under test wherever it is being edited.
const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0000_0200;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;

/// Whether a reparse tag means "the bytes are somewhere else".
///
/// The walk asks the attribute bits this question, because Win32 answers it
/// there: `RECALL_ON_OPEN` and `RECALL_ON_DATA_ACCESS` are synthesised for it by
/// whichever filter driver owns the file. Neither bit is ever *written to a
/// record*, so from here the question has to be put to the reparse tag, which
/// is what the driver keys off in the first place.
///
/// The cloud family — OneDrive and everything else built on `cldflt` — is nine
/// consecutive tags differing in one nibble. `HSM` and `HSM2` are the older
/// hierarchical-storage tags with the same meaning.
/// `IO_REPARSE_TAG_WOF`: the file's bytes are in a compressed named stream.
const WOF_TAG: u32 = 0x8000_0017;

fn data_is_elsewhere(tag: u32) -> bool {
    const CLOUD: u32 = 0x9000_001A;
    const CLOUD_FAMILY: u32 = 0xFFFF_0FFF;
    const HSM: u32 = 0xC000_0004;
    const HSM2: u32 = 0x8000_0006;
    tag & CLOUD_FAMILY == CLOUD || tag == HSM || tag == HSM2
}

/// The root directory is always record 5. This is fixed by the format, not a
/// convention: `$MFT` is 0, `$MFTMirr` 1, and `.` is 5 on every NTFS volume
/// ever created.
pub const ROOT_RECORD: u32 = 5;

impl Table {
    /// Group the names by the directory they are in.
    ///
    /// A counting sort rather than a hash map of vectors: the parent is a dense
    /// record number, so two `u32` arrays do what a million small allocations
    /// would, and the children of any directory come out as a contiguous slice
    /// of `order`.
    fn group(&self) -> Grouped {
        let mut starts = vec![0u32; self.records.len() + 1];
        for name in &self.names {
            if let Some(slot) = starts.get_mut(name.parent as usize + 1) {
                *slot += 1;
            }
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }

        let mut cursor = starts.clone();
        let mut order = vec![0u32; self.names.len()];
        for (index, name) in self.names.iter().enumerate() {
            if let Some(slot) = cursor.get_mut(name.parent as usize) {
                order[*slot as usize] = index as u32;
                *slot += 1;
            }
        }
        Grouped { starts, order }
    }
}

struct Grouped {
    starts: Vec<u32>,
    order: Vec<u32>,
}

impl Grouped {
    /// Indices into `Table::names` of everything inside this directory.
    fn children(&self, parent: u32) -> &[u32] {
        let at = parent as usize;
        match (self.starts.get(at), self.starts.get(at + 1)) {
            (Some(&from), Some(&to)) => &self.order[from as usize..to as usize],
            _ => &[],
        }
    }
}

/// Put every extension record back onto the record it belongs to.
///
/// A file with more attributes than fit in one record — more fragments, or more
/// names — spills into *extension* records, and the base names itself in each
/// one's header. NTFS keeps `$STANDARD_INFORMATION` in the base and is free to
/// put everything else anywhere, so a record read in isolation can hold a name
/// and a size with no timestamp, or a timestamp with no size.
///
/// Two things follow, and the second is the one that bites:
///
/// - **Sizes move down to the base.** The fragment carrying VCN zero is the only
///   one whose header holds them, and it can be in any record. Folding them
///   afterwards gets the same answer as parsing `$ATTRIBUTE_LIST` to find out
///   which, in one pass and with no second lookup.
/// - **Names move down to the base too.** A name found in an extension record is
///   a name for the *file*, not for the extension. Leaving it pointing at the
///   extension gives that name a record with no `$STANDARD_INFORMATION` — so no
///   modification time and no attributes — and, worse, splits one file's names
///   across several record numbers, which is exactly the identity hardlink
///   accounting is built on. On this machine's `C:` that was 23,822 records:
///   every heavily hardlinked Windows component file, each showing an epoch
///   timestamp and each counted as a separate file.
///
/// - **And only then can the 8.3 names go.** A short name is a second name for
///   the same file, and listing it puts a phantom `NEWSAP~2.PNG` beside every
///   long name on the volume. Dropping it per *record* is not enough: the long
///   name and the short one need not be in the same record, and where they are
///   not, the record holding only the short one looks like a file that has no
///   long name and must keep it. The rule is per *file*, and a file is only
///   whole once its extensions have been folded in.
///
/// Returns how many records were folded, which is only ever used by a test.
pub fn consolidate(table: &mut Table) -> u64 {
    let mut folded = 0;
    for number in 0..table.records.len() {
        let record = table.records[number];
        if record.base == 0 || !record.in_use {
            continue;
        }
        let Some(base) = table.records.get_mut(record.base as usize) else {
            continue;
        };
        if !base.in_use {
            continue;
        }
        if record.has_data && !base.has_data {
            base.logical = record.logical;
            base.allocated = record.allocated;
            base.has_data = true;
        }
        base.named_data_allocated = base
            .named_data_allocated
            .saturating_add(record.named_data_allocated);
        // NTFS keeps `$STANDARD_INFORMATION` in the base record, so this is
        // belt and braces rather than a case anybody has seen — and it is the
        // same shape as the sizes above, where the attribute genuinely can be
        // anywhere.
        if base.security_id == 0 {
            base.security_id = record.security_id;
        }
        if base.reparse_tag == 0 {
            base.reparse_tag = record.reparse_tag;
            base.is_mounted_volume |= record.is_mounted_volume;
        }
        folded += 1;
    }

    for owner in &mut table.owners {
        let record = table.records[*owner as usize];
        if record.base != 0
            && table
                .records
                .get(record.base as usize)
                .is_some_and(|base| base.in_use)
        {
            *owner = record.base;
        }
    }

    // A file keeps its 8.3 name only when it has nothing else to be called by.
    let mut has_long_name = vec![false; table.records.len()];
    for (name, owner) in table.names.iter().zip(&table.owners) {
        if name.namespace != NAMESPACE_DOS
            && let Some(slot) = has_long_name.get_mut(*owner as usize)
        {
            *slot = true;
        }
    }

    // Compacted in place over both vectors at once: they are parallel, and
    // `retain` on either alone would silently unpair them.
    let mut write = 0;
    for read in 0..table.names.len() {
        let owner = table.owners[read];
        let long = has_long_name
            .get(owner as usize)
            .copied()
            .unwrap_or_default();
        if table.names[read].namespace == NAMESPACE_DOS && long {
            continue;
        }
        table.names[write] = table.names[read];
        table.owners[write] = owner;
        write += 1;
    }
    table.names.truncate(write);
    table.owners.truncate(write);

    folded
}

/// Which record the tree should start from, and what to call it.
pub struct Root<'a> {
    pub record: u32,
    pub path: &'a Path,
}

/// Build the tree.
///
/// `publish` is handed progress at each level boundary, and `cancelled` is
/// consulted there too — the same two points the ordinary walk uses, so a
/// cancel behaves identically whichever strategy is running.
pub fn build(
    table: &Table,
    root: Root<'_>,
    options: &ScanOptions,
    security: &Security,
    mut publish: impl FnMut(Progress),
    mut cancelled: impl FnMut() -> bool,
) -> SizeTree {
    let grouped = table.group();

    let mut nodes: Vec<Node> = Vec::with_capacity(table.names.len() + 1);
    let mut names = NameArena::with_capacity(table.name_bytes.len() + 64);
    let mut stats = ScanStats {
        strategy: Strategy::MasterFileTable,
        // The record number is in the table being read, so this costs nothing
        // here and the walk on the same volume pays a syscall per file for it.
        // Two strategies, two defaults, one machine — which is why a snapshot
        // has to carry the rule it was taken under.
        dedup_hardlinks: options.dedups_hardlinks(true),
        ..ScanStats::default()
    };

    let root_summary = table
        .records
        .get(root.record as usize)
        .copied()
        .unwrap_or_default();

    let mut owners = if options.collect_owners {
        Owners::with_capacity(table.names.len() + 1)
    } else {
        Owners::default()
    };
    // Security ids repeat across whole directory trees, so this turns a million
    // hash lookups into a few hundred. The map is from the volume's id to the
    // tree's own owner slot.
    let mut slot_of: FxHashMap<u32, Option<u32>> = FxHashMap::default();
    let mut owner_slot = |security_id: u32, owners: &mut Owners| -> Option<u32> {
        *slot_of.entry(security_id).or_insert_with(|| {
            let sid = security.get(&security_id)?;
            Some(owners.intern(OwnerId::Sid(sid.clone())))
        })
    };

    let root_label = root.path.file_name().unwrap_or(root.path.as_os_str());
    let mut created = vec![root_summary.created.unwrap_or(u64::MAX)];
    let (root_off, root_len) = names.push(root_label);
    nodes.push(Node {
        // Zero for the same reason the walk zeroes it: a directory's apparent
        // size is its contents', never its own index.
        logical: 0,
        allocated: 0,
        parent: 0,
        first_child: 0,
        child_count: 0,
        subtree_files: 0,
        name_off: root_off,
        mtime: root_summary.mtime,
        name_len: root_len,
        flags: Flags::DIR,
        depth: 0,
    });
    if options.collect_owners {
        let slot = owner_slot(root_summary.security_id, &mut owners);
        owners.push(slot);
    }
    stats.dirs = 1;
    stats.entries = 1;

    // A file record is one file however many names point at it, so the MFT
    // settles hardlinks by identity rather than by inode bookkeeping: the first
    // name reached in breadth-first order owns the bytes, and every later one
    // contributes zero. The ordinary Windows walk pays an open per file for
    // exactly this answer.
    let mut counted: FxHashSet<u32> = FxHashSet::default();
    // Directories cannot be hardlinked, but a corrupt or hostile volume can
    // still describe a cycle, and a cycle here is an infinite tree.
    let mut entered: FxHashSet<u32> = FxHashSet::default();
    entered.insert(root.record);

    let mut frontier: Vec<(u32, u32)> = vec![(0, root.record)];
    let mut depth: u8 = 0;

    while !frontier.is_empty() {
        if cancelled() {
            stats.cancelled = true;
            stats.partial = true;
            break;
        }
        if options.depth_limit_reached(depth) {
            break;
        }

        let child_depth = depth.saturating_add(1);
        let mut next: Vec<(u32, u32)> = Vec::new();
        // Reused across the level so sorting a directory does not allocate.
        let mut children: Vec<(u32, u32)> = Vec::new();

        for &(parent_index, parent_record) in &frontier {
            children.clear();
            for &name_index in grouped.children(parent_record) {
                // Record 5's own `.` entry names itself as its parent, so
                // without this the root is a child of the root.
                let record = table.owners[name_index as usize];
                if record == parent_record {
                    continue;
                }
                // A name pointing at a record that is not in use is a directory
                // index that has not caught up with a deletion; those bytes are
                // already gone.
                if !table
                    .records
                    .get(record as usize)
                    .is_some_and(|summary| summary.in_use)
                {
                    continue;
                }
                children.push((name_index, record));
            }

            // Sorted by encoded name bytes, exactly as the walk sorts a
            // directory, so the two strategies produce the same order.
            children.sort_unstable_by(|a, b| name_bytes(table, a.0).cmp(name_bytes(table, b.0)));

            let first_child = nodes.len() as u32;
            for &(name_index, record) in &children {
                let summary = table.records[record as usize];
                let (name_off, name_len) = names.push_bytes(name_bytes(table, name_index));

                let mut flags = Flags::default();
                let mut logical = summary.logical;
                let mut allocated = summary.allocated;

                // A file compressed by the Windows Overlay Filter — CompactOS,
                // and what most installers now use — is a reparse point that
                // Windows does not present as one: it reports no reparse
                // attribute, no compression attribute, and the *named* stream's
                // allocation as the size on disk. So neither do we. To anybody
                // using the machine this is an ordinary file that happens to be
                // small, and the two strategies have to agree on that or every
                // CompactOS install diffs against itself.
                let overlay = summary.reparse_tag == WOF_TAG;
                if overlay {
                    allocated = summary.named_data_allocated;
                }

                let offline = !summary.is_dir
                    && (summary.attributes & FILE_ATTRIBUTE_OFFLINE != 0
                        || data_is_elsewhere(summary.reparse_tag));
                let reparse = !overlay
                    && summary.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    && !summary.is_mounted_volume;

                // The order below is `scan::read_directory`'s, deliberately and
                // exactly: link first, then directory, then file. A directory
                // symlink is a *link*, not a directory — it has no children
                // here and is measured as itself — and classifying it the other
                // way changes the `items` count of every directory above it.
                // Following the walk's order is how the two strategies stay
                // comparable row for row.
                let is_link = reparse && !offline;
                let is_dir = summary.is_dir && !is_link;

                if is_link {
                    flags.insert(Flags::SYMLINK);
                } else if is_dir {
                    flags.insert(Flags::DIR);
                    // A directory's apparent size is its contents', never its
                    // own index, and NTFS reports zero for both.
                    logical = 0;
                    allocated = 0;
                }

                // Set on directories too, matching the walk: a directory
                // carrying the compressed attribute is where compression is
                // inherited from, and that is worth saying even though the
                // directory itself occupies nothing.
                if !overlay
                    && (summary.attributes
                        & (FILE_ATTRIBUTE_SPARSE_FILE | FILE_ATTRIBUTE_COMPRESSED)
                        != 0
                        || offline)
                {
                    // Unlike the walk, the number beside this flag is exact:
                    // `$DATA` carries what is really allocated, so a compressed
                    // file's saving is measured rather than hidden.
                    flags.insert(Flags::SPARSE);
                }
                if offline {
                    flags.insert(Flags::OFFLINE);
                }
                if summary.is_mounted_volume {
                    // The MFT of this volume says nothing whatever about the
                    // volume mounted here, so this is a boundary the strategy
                    // cannot cross even when the setting says to. Named as the
                    // boundary it is rather than left looking like an empty
                    // folder.
                    flags.insert(Flags::OTHER_DEVICE);
                    stats.skipped_other_device += 1;
                }

                if !is_dir && !counted.insert(record) && options.dedups_hardlinks(true) {
                    flags.insert(Flags::HARDLINK_DUP);
                    logical = 0;
                    allocated = 0;
                    stats.hardlinks_deduped += 1;
                }

                if is_dir {
                    stats.dirs += 1;
                } else {
                    stats.files += 1;
                }

                nodes.push(Node {
                    logical,
                    allocated,
                    parent: parent_index,
                    first_child: 0,
                    child_count: 0,
                    subtree_files: u32::from(!is_dir),
                    name_off,
                    mtime: summary.mtime,
                    name_len,
                    flags,
                    depth: child_depth,
                });
                created.push(summary.created.unwrap_or(u64::MAX));
                if options.collect_owners {
                    // `None` is a record whose security id is not in `$Secure`
                    // — a volume upgraded from NTFS 1.2 still has records with
                    // no id at all — which the tree records as an owner it
                    // could not read rather than as nobody.
                    let slot = owner_slot(summary.security_id, &mut owners);
                    owners.push(slot);
                }

                // A link has no children on this volume, and a mounted volume's
                // children are in another MFT entirely.
                if is_dir && !summary.is_mounted_volume && entered.insert(record) {
                    next.push((nodes.len() as u32 - 1, record));
                }
            }

            nodes[parent_index as usize].first_child = first_child;
            nodes[parent_index as usize].child_count = children.len() as u32;
            stats.entries += children.len() as u64;
        }

        stats.max_depth = stats.max_depth.max(child_depth);
        publish(Progress {
            dirs_scanned: None,
            entries: stats.entries,
            dirs_pending: next.len() as u64,
            depth,
        });

        frontier = next;
        depth = child_depth;
    }

    crate::scan::aggregate(&mut nodes);

    // Once per distinct account, after the tree is whole. The same arrangement
    // as the walk, and for the same reason: a lookup can reach a domain
    // controller, and doing that per file would undo everything the strategy
    // buys.
    owners.resolve(crate::platform::resolve_owner);

    nodes.shrink_to_fit();
    names.shrink_to_fit();
    owners.shrink_to_fit();
    created.shrink_to_fit();

    stats.bytes_logical = nodes[0].logical;
    stats.bytes_allocated = nodes[0].allocated;

    SizeTree {
        nodes,
        names,
        owners,
        created,
        root_path: root.path.to_path_buf(),
        stats,
    }
}

#[inline]
fn name_bytes(table: &Table, name_index: u32) -> &[u8] {
    let name = table.names[name_index as usize];
    &table.name_bytes[name.off as usize..name.off as usize + name.len as usize]
}
