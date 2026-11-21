// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Who a file belongs to, and how much disk each owner is using.
//!
//! On a machine one person uses this answers nothing worth asking. On a shared
//! one it answers the only question that matters when a disk fills up, and it
//! is a question no amount of staring at a treemap will answer: *whose* files
//! are these. A build server with six accounts on it, a family desktop, a lab
//! box — the 400 GiB is somebody's, and the directory it is in usually does not
//! say whose.
//!
//! ## An owner is not a name
//!
//! Windows records a security identifier and Unix records a numeric user id.
//! Neither is a name, and turning one into a name is a *lookup* that can fail —
//! which is the interesting part of this feature rather than a detail of it.
//! Three outcomes, kept apart on purpose:
//!
//! | | What it means | How it reads |
//! | --- | --- | --- |
//! | resolved | the machine knows the account | `PC\neeraj`, `root` |
//! | unresolved | a real owner the machine cannot name | the SID or uid itself |
//! | unreadable | the owner could not be read at all | counted, never merged |
//!
//! **An unresolved owner is never folded into an "other" bucket.** On a shared
//! machine the account nobody recognises is frequently the one using the disk:
//! a domain user whose account was deleted, a profile copied from another
//! computer, files restored from a backup taken on different hardware. Summing
//! those into one row would hide exactly the thing somebody opened this to
//! find. So each distinct SID gets its own row, named by its SID, and the
//! interface says the machine does not know it rather than pretending it is
//! nobody.
//!
//! **Unreadable is a fourth thing again**, and gets its own row with no owner.
//! A file whose security descriptor we were refused is not owned by nobody; it
//! is owned by somebody we were not allowed to ask about. Rolling it into a
//! named account would be a lie, and dropping it would silently lose bytes from
//! a total that is supposed to add up to the tree.
//!
//! ## What it costs
//!
//! Free on Unix: `st_uid` arrives with the `lstat` the walk already does.
//! One extra call per file on Windows during the walk, which is why
//! [`ScanOptions::collect_owners`](crate::ScanOptions::collect_owners) is off by
//! default — and free again in an MFT scan, where every record carries a
//! security id and `$Secure` maps the few dozen distinct ones to descriptors in
//! a single read.
//!
//! The per-node table is four bytes an entry and is allocated only when the
//! option is on.

use std::borrow::Cow;
use std::fmt;

use crate::node::NodeId;
use crate::tree::SizeTree;

/// What a platform calls the owner of a file.
///
/// Two shapes because the two platforms genuinely disagree about what an owner
/// *is*, and flattening them into one string would lose the ability to look
/// either back up. Never compared across platforms: a uid and a SID are not
/// two spellings of one idea.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum OwnerId {
    /// A Windows security identifier in canonical form, `S-1-5-21-…`.
    ///
    /// The text rather than the bytes, because it is the form a user can act
    /// on — `icacls`, `Get-Acl`, a web search for an unfamiliar service
    /// account all take this — and because a variable-length structure makes a
    /// poor hash key. See `platform::sid`.
    Sid(String),
    /// A Unix user id.
    Uid(u32),
}

impl fmt::Display for OwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OwnerId::Sid(sid) => f.write_str(sid),
            OwnerId::Uid(uid) => write!(f, "uid {uid}"),
        }
    }
}

/// What sort of account an owner turned out to be.
///
/// Worth carrying because the honest answer to "who is using this disk" on a
/// Windows machine starts with two rows nobody cares about: `TrustedInstaller`
/// owns most of `C:\Windows` and `SYSTEM` owns most of the rest. Both are real
/// owners and both must be shown — but a report that cannot tell them apart
/// from the people using the machine buries the answer under the operating
/// system.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OwnerKind {
    /// A user account somebody logs in as.
    Person,
    /// A group or alias that owns files directly — `Administrators` on Windows
    /// is the usual one, since a file created by an elevated process is owned
    /// by the group rather than by the person.
    Group,
    /// The operating system, a service, or a built-in principal. Nobody's
    /// files in the sense the question means.
    Machine,
    /// The account exists and the machine could not say what it is.
    Unknown,
}

impl OwnerKind {
    /// Whether these bytes belong to somebody who uses the machine, as opposed
    /// to the machine itself.
    ///
    /// `Unknown` counts as a person: an account this computer cannot name is
    /// far more likely to be a user from somewhere else than a service, and
    /// guessing the other way would hide it.
    pub fn is_somebody(self) -> bool {
        matches!(
            self,
            OwnerKind::Person | OwnerKind::Group | OwnerKind::Unknown
        )
    }
}

/// One account, as far as this machine could work it out.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Owner {
    pub id: OwnerId,
    /// What the machine calls this account, when it knows. `None` is a real
    /// answer rather than a missing one — see the module note.
    pub name: Option<String>,
    pub kind: OwnerKind,
}

impl Owner {
    /// An owner nothing is known about beyond its identifier.
    pub fn unnamed(id: OwnerId) -> Owner {
        Owner {
            id,
            name: None,
            kind: OwnerKind::Unknown,
        }
    }

    /// What to show. The account name where there is one, and the identifier
    /// itself where there is not — never a placeholder, because the identifier
    /// is the only thing a user can then go and look up.
    ///
    /// Borrowed wherever it can be, which is every case but a bare uid. The
    /// query engine compares this once per node over a whole tree, and an
    /// allocation per node is the difference between a filter that runs on a
    /// keystroke and one that does not.
    pub fn label(&self) -> Cow<'_, str> {
        match (&self.name, &self.id) {
            (Some(name), _) => Cow::Borrowed(name),
            (None, OwnerId::Sid(sid)) => Cow::Borrowed(sid),
            (None, id) => Cow::Owned(id.to_string()),
        }
    }

    /// The account name without the domain it is qualified by.
    ///
    /// `EXAMPLE-PC\alice` is how Windows spells an account and is not
    /// a string anybody wants to type, especially since the domain part is the
    /// same for every local account on the machine. So a query comparing to an
    /// owner tries this as well as the full label. Identical to the label
    /// everywhere else, including on Unix, where there is nothing to strip.
    pub fn account(&self) -> Cow<'_, str> {
        match self.label() {
            Cow::Borrowed(text) => Cow::Borrowed(after_domain(text)),
            Cow::Owned(text) => Cow::Owned(after_domain(&text).to_string()),
        }
    }

    /// Whether this machine could put a name to the account.
    pub fn is_named(&self) -> bool {
        self.name.is_some()
    }
}

/// The part of `DOMAIN\account` after the separator, or the whole thing.
fn after_domain(label: &str) -> &str {
    match label.rfind('\\') {
        Some(at) => &label[at + 1..],
        None => label,
    }
}

/// Who owns each node of a tree.
///
/// A side table rather than a field on [`Node`](crate::Node): the node is 48
/// bytes and every one of them is spoken for, and this is four bytes an entry
/// that most scans never ask for. Parallel to the arena, so `of_node[i]` is the
/// owner of node `i`.
#[derive(Clone, Default, Debug)]
pub struct Owners {
    people: Vec<Owner>,
    of_node: Vec<u32>,
}

/// The owner slot for an entry whose owner could not be read.
///
/// Not an index into `people`, and deliberately not a `people` entry of its
/// own: "we were refused" is not an account.
const UNREADABLE: u32 = u32::MAX;

impl Owners {
    /// Whether this scan collected owners at all.
    ///
    /// Distinct from "collected them and found none", which cannot happen: a
    /// tree always has a root and the root always has an owner or an explicit
    /// refusal.
    pub fn were_collected(&self) -> bool {
        !self.of_node.is_empty()
    }

    /// Every distinct account seen, in the order they were first met.
    pub fn people(&self) -> &[Owner] {
        &self.people
    }

    /// The per-entry slots, parallel to the arena. [`UNREADABLE`] where the
    /// owner could not be read. For writing a snapshot.
    pub fn slots(&self) -> &[u32] {
        &self.of_node
    }

    /// Rebuild from a snapshot. Slots that point nowhere read back as
    /// unreadable, which is what [`Owners::of`] already does with them.
    pub fn from_parts(people: Vec<Owner>, of_node: Vec<u32>) -> Owners {
        Owners { people, of_node }
    }

    /// Who owns this entry. `None` when the owner could not be read, or when
    /// the scan did not collect owners.
    pub fn of(&self, id: NodeId) -> Option<&Owner> {
        let slot = *self.of_node.get(id.index())?;
        self.people.get(slot as usize)
    }

    /// Approximate resident size, for the memory readout.
    pub fn memory_bytes(&self) -> usize {
        self.of_node.capacity() * size_of::<u32>()
            + self.people.capacity() * size_of::<Owner>()
            + self
                .people
                .iter()
                .map(|p| {
                    p.name.as_ref().map_or(0, String::capacity)
                        + match &p.id {
                            OwnerId::Sid(s) => s.capacity(),
                            OwnerId::Uid(_) => 0,
                        }
                })
                .sum::<usize>()
    }

    // ------------------------------------------------------------ building

    /// Room for a tree of `entries` nodes.
    pub(crate) fn with_capacity(entries: usize) -> Owners {
        Owners {
            people: Vec::new(),
            of_node: Vec::with_capacity(entries),
        }
    }

    /// Give this account an index, adding it if it is new.
    ///
    /// Linear, because the number of distinct owners on a volume is a couple of
    /// dozen at the outside and a hash map of that size is slower than the scan
    /// it would be inside. Callers intern once per *distinct* owner per
    /// directory, not once per file.
    pub(crate) fn intern(&mut self, id: OwnerId) -> u32 {
        if let Some(at) = self.people.iter().position(|p| p.id == id) {
            return at as u32;
        }
        self.people.push(Owner::unnamed(id));
        (self.people.len() - 1) as u32
    }

    /// Record the owner of the next node. Must be called once per node, in
    /// arena order.
    pub(crate) fn push(&mut self, slot: Option<u32>) {
        self.of_node.push(slot.unwrap_or(UNREADABLE));
    }

    /// Put a name to every account collected, once the tree is built.
    ///
    /// Deliberately after the scan rather than during it: a lookup can hit the
    /// network on a domain-joined machine, and paying that per *file* rather
    /// than per *account* is the difference between a scan and a hang.
    pub(crate) fn resolve(
        &mut self,
        mut lookup: impl FnMut(&OwnerId) -> Option<(String, OwnerKind)>,
    ) {
        for person in &mut self.people {
            if let Some((name, kind)) = lookup(&person.id) {
                person.name = Some(name);
                person.kind = kind;
            }
        }
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        self.people.shrink_to_fit();
        self.of_node.shrink_to_fit();
    }

    /// The raw slot for a node, for the aggregation below.
    #[inline]
    fn slot(&self, index: usize) -> u32 {
        self.of_node.get(index).copied().unwrap_or(UNREADABLE)
    }
}

/// One account's share of a scanned tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Share {
    /// `None` for the bytes whose owner could not be read.
    pub owner: Option<Owner>,
    /// On-disk bytes of the entries this account owns.
    pub allocated: u64,
    /// Apparent bytes of the same entries.
    pub logical: u64,
    /// How many of them are files.
    pub files: u64,
    /// How many are directories.
    pub dirs: u64,
}

impl Share {
    /// What to call this row.
    pub fn label(&self) -> Cow<'_, str> {
        match &self.owner {
            Some(owner) => owner.label(),
            None => Cow::Borrowed("(owner unreadable)"),
        }
    }
}

/// Who is using the disk, largest share first.
///
/// The shares add up to the tree's own total, which is the property that makes
/// this worth trusting: every byte is attributed to exactly one row, including
/// the bytes whose owner could not be read.
pub fn shares(tree: &SizeTree) -> Vec<Share> {
    let owners = tree.owners();
    if !owners.were_collected() {
        return Vec::new();
    }

    // One bucket per account, plus one at the end for the unreadable.
    let buckets = owners.people().len() + 1;
    let unreadable = buckets - 1;
    let bucket = |slot: u32| {
        if slot as usize >= owners.people().len() {
            unreadable
        } else {
            slot as usize
        }
    };

    // Signed, because the arithmetic below subtracts. A node's `allocated` is
    // its whole subtree after aggregation, so attributing it directly would
    // count a file once for itself and again for every directory above it. The
    // bytes that belong to an entry *itself* are its total less its children's,
    // and summing that per owner is these two passes: add every node to its own
    // owner, then take every node back off its parent's. Equivalent to walking
    // each directory's children, and one linear pass instead of two.
    let mut allocated = vec![0i128; buckets];
    let mut logical = vec![0i128; buckets];
    let mut files = vec![0u64; buckets];
    let mut dirs = vec![0u64; buckets];

    let nodes = tree.nodes();
    for (index, node) in nodes.iter().enumerate() {
        let at = bucket(owners.slot(index));
        allocated[at] += i128::from(node.allocated);
        logical[at] += i128::from(node.logical);
        if node.is_dir() {
            dirs[at] += 1;
        } else {
            files[at] += 1;
        }
    }
    for node in nodes.iter().skip(1) {
        let at = bucket(owners.slot(node.parent as usize));
        allocated[at] -= i128::from(node.allocated);
        logical[at] -= i128::from(node.logical);
    }

    let mut rows: Vec<Share> = (0..buckets)
        .map(|at| Share {
            owner: owners.people().get(at).cloned(),
            // Clamped rather than wrapped. Only reachable if the invariant
            // above is broken — a parent whose total is less than its
            // children's — and a row of zero is a better answer than a row of
            // eighteen exabytes.
            allocated: allocated[at].max(0) as u64,
            logical: logical[at].max(0) as u64,
            files: files[at],
            dirs: dirs[at],
        })
        .filter(|row| row.allocated > 0 || row.files > 0 || row.dirs > 0)
        .collect();

    // Largest first, and ties broken on the label so two runs of the same scan
    // print the same table.
    rows.sort_by(|a, b| {
        b.allocated
            .cmp(&a.allocated)
            .then_with(|| b.files.cmp(&a.files))
            .then_with(|| a.label().cmp(&b.label()))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_with_no_name_shows_its_identifier() {
        let owner = Owner::unnamed(OwnerId::Sid("S-1-5-21-1-2-3-1001".into()));
        assert_eq!(owner.label(), "S-1-5-21-1-2-3-1001");
        assert!(!owner.is_named());

        let owner = Owner::unnamed(OwnerId::Uid(1000));
        assert_eq!(owner.label(), "uid 1000");
    }

    #[test]
    fn a_named_owner_shows_the_name() {
        let owner = Owner {
            id: OwnerId::Uid(0),
            name: Some("root".into()),
            kind: OwnerKind::Machine,
        };
        assert_eq!(owner.label(), "root");
        assert!(owner.is_named());
    }

    /// The one judgement call in the type: an account the machine cannot name
    /// is counted as somebody rather than as the operating system, because a
    /// report that hides it hides the answer.
    #[test]
    fn an_unnamed_account_counts_as_a_person() {
        assert!(OwnerKind::Unknown.is_somebody());
        assert!(OwnerKind::Person.is_somebody());
        assert!(OwnerKind::Group.is_somebody());
        assert!(!OwnerKind::Machine.is_somebody());
    }

    /// The account running the tests is one this machine can name, and it is
    /// the same account that owns a file it just created — which is the whole
    /// claim `owner = me` rests on.
    #[test]
    fn the_running_account_is_the_one_that_owns_what_it_writes() {
        use crate::{ScanOptions, Scanner};

        let me = crate::platform::current_owner().expect("the process token names its own user");
        assert!(me.is_named(), "the running account came back unnamed");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mine.txt"), b"hello").unwrap();
        let tree = Scanner::new(ScanOptions {
            collect_owners: true,
            ..Default::default()
        })
        .scan(dir.path())
        .unwrap();

        let file = tree.children(tree.root()).next().expect("one file");
        let owner = tree.owners().of(file).expect("a file this process wrote");

        // Windows can hand a new file to `BUILTIN\Administrators` rather than
        // to the person, depending on a local policy, and only for an elevated
        // process. The claim being tested is about the ordinary case; the
        // elevated one is checked but not required to match, because failing
        // there would mean the suite could not be run from an admin shell.
        if crate::platform::elevated() && owner.id != me.id {
            return;
        }
        assert_eq!(
            owner.id, me.id,
            "a file written by this process is owned by somebody else"
        );
    }

    #[test]
    fn interning_gives_one_slot_per_distinct_account() {
        let mut owners = Owners::with_capacity(4);
        let a = owners.intern(OwnerId::Uid(1000));
        let b = owners.intern(OwnerId::Uid(0));
        let again = owners.intern(OwnerId::Uid(1000));
        assert_eq!(a, again);
        assert_ne!(a, b);
        assert_eq!(owners.people().len(), 2);
    }

    #[test]
    fn a_scan_that_did_not_collect_owners_says_so() {
        let owners = Owners::default();
        assert!(!owners.were_collected());
        assert!(owners.of(NodeId::ROOT).is_none());
    }

    // ------------------------------------------------------------- shares

    /// Four entries with three different answers about who owns them, laid out
    /// by hand so the expected numbers can be worked out on paper:
    ///
    /// ```text
    /// C:\           dir,  owner A,  4 KiB of its own
    ///   sub\        dir,  owner B,  4 KiB of its own
    ///     a.bin     file, owner A,  100 000 bytes
    ///     b.bin     file, unreadable, 8 192 bytes
    /// ```
    ///
    /// The point is the subtraction. After aggregation `C:\` holds 116 384
    /// bytes, all but 4 096 of which belong to entries below it — so attributing
    /// a directory's total to its owner would credit A with the whole tree,
    /// including the file owned by B and the one whose owner could not be read.
    fn tree_with_three_owners() -> SizeTree {
        use crate::names::NameArena;
        use crate::node::{Flags, Node};
        use crate::tree::ScanStats;

        let mut names = NameArena::with_capacity(64);
        let mut owners = Owners::with_capacity(4);
        let a = owners.intern(OwnerId::Uid(1000));
        let b = owners.intern(OwnerId::Uid(1001));

        let entry = |name: (u32, u16), parent: u32, allocated: u64, dir: bool, depth: u8| Node {
            logical: if dir { 0 } else { allocated },
            allocated,
            parent,
            first_child: 0,
            child_count: 0,
            subtree_files: u32::from(!dir),
            name_off: name.0,
            mtime: 0,
            name_len: name.1,
            flags: if dir { Flags::DIR } else { Flags::default() },
            depth,
        };

        let root_name = names.push(std::ffi::OsStr::new("C:"));
        let sub_name = names.push(std::ffi::OsStr::new("sub"));
        let a_name = names.push(std::ffi::OsStr::new("a.bin"));
        let b_name = names.push(std::ffi::OsStr::new("b.bin"));

        let mut nodes = vec![
            entry(root_name, 0, 4096, true, 0),
            entry(sub_name, 0, 4096, true, 1),
            entry(a_name, 1, 100_000, false, 2),
            entry(b_name, 1, 8_192, false, 2),
        ];
        nodes[0].first_child = 1;
        nodes[0].child_count = 1;
        nodes[1].first_child = 2;
        nodes[1].child_count = 2;

        owners.push(Some(a));
        owners.push(Some(b));
        owners.push(Some(a));
        owners.push(None);

        crate::scan::aggregate(&mut nodes);

        SizeTree {
            nodes,
            names,
            owners,
            created: Vec::new(),
            root_path: std::path::PathBuf::from("C:\\"),
            stats: ScanStats::default(),
            replaced_names: Vec::new(),
        }
    }

    #[test]
    fn a_directorys_total_is_not_credited_to_its_owner() {
        let tree = tree_with_three_owners();
        assert_eq!(tree.total_allocated(), 116_384);

        let rows = shares(&tree);
        assert_eq!(rows.len(), 3);

        // Largest first: A holds its own 4 KiB plus the 100 000-byte file.
        assert_eq!(rows[0].owner.as_ref().unwrap().id, OwnerId::Uid(1000));
        assert_eq!(rows[0].allocated, 104_096);
        assert_eq!(rows[0].files, 1);
        assert_eq!(rows[0].dirs, 1);

        // The unreadable row, which is a row rather than a silence.
        assert!(rows[1].owner.is_none());
        assert_eq!(rows[1].allocated, 8_192);
        assert_eq!(rows[1].files, 1);
        assert_eq!(rows[1].label(), "(owner unreadable)");

        // B owns the directory and none of what is inside it.
        assert_eq!(rows[2].owner.as_ref().unwrap().id, OwnerId::Uid(1001));
        assert_eq!(rows[2].allocated, 4_096);
        assert_eq!(rows[2].files, 0);
        assert_eq!(rows[2].dirs, 1);
    }

    /// The property that makes the table worth trusting: every byte lands in
    /// exactly one row, so the rows add up to the tree.
    #[test]
    fn the_shares_add_up_to_the_whole_tree() {
        let tree = tree_with_three_owners();
        let rows = shares(&tree);
        assert_eq!(
            rows.iter().map(|row| row.allocated).sum::<u64>(),
            tree.total_allocated()
        );
        assert_eq!(
            rows.iter().map(|row| row.logical).sum::<u64>(),
            tree.total_logical()
        );
        assert_eq!(
            rows.iter().map(|row| row.files + row.dirs).sum::<u64>(),
            tree.len() as u64
        );
    }

    /// A real scan of a real directory, which on a machine with one account
    /// gives one row — and that row has to hold everything, or the walk is
    /// dropping entries on the floor somewhere between the stat and the table.
    #[test]
    fn a_scanned_tree_attributes_every_byte_it_measured() {
        use crate::{ScanOptions, Scanner};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100_000]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.bin"), vec![7u8; 40_000]).unwrap();
        std::fs::write(dir.path().join("sub/c.txt"), b"small").unwrap();

        let tree = Scanner::new(ScanOptions {
            collect_owners: true,
            ..Default::default()
        })
        .scan(dir.path())
        .unwrap();

        assert!(
            tree.owners().were_collected(),
            "the scan was asked for owners and collected none"
        );
        assert!(
            !tree.owners().people().is_empty(),
            "a directory this process just created has an owner it can read"
        );

        let rows = shares(&tree);
        assert_eq!(
            rows.iter().map(|row| row.allocated).sum::<u64>(),
            tree.total_allocated()
        );
        assert_eq!(
            rows.iter().map(|row| row.files + row.dirs).sum::<u64>(),
            tree.len() as u64
        );

        // The account that created these files is one this machine can name.
        let mine = tree
            .owners()
            .of(tree.root())
            .expect("the root has an owner");
        assert!(
            mine.is_named(),
            "the running user's own account came back unnamed as {}",
            mine.label()
        );
    }

    /// The option is what turns the table on, and leaving it off must leave the
    /// tree exactly as it was — same entries, same totals, no table.
    #[test]
    fn leaving_the_option_off_changes_nothing_but_the_table() {
        use crate::{ScanOptions, Scanner};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100_000]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.bin"), vec![7u8; 40_000]).unwrap();

        let scan = |collect_owners| {
            Scanner::new(ScanOptions {
                collect_owners,
                ..Default::default()
            })
            .scan(dir.path())
            .unwrap()
        };

        let with = scan(true);
        let without = scan(false);
        assert_eq!(with.len(), without.len());
        assert_eq!(with.total_allocated(), without.total_allocated());
        assert_eq!(with.total_logical(), without.total_logical());
        assert!(!without.owners().were_collected());
    }
}
