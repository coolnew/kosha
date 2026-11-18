// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! What changed since last time.
//!
//! A disk that is full today was not full a month ago, and "what is big" is a
//! much less useful answer than "what grew". The first is dominated by things
//! that have always been big and are supposed to be — the OS, a photo library —
//! while the second is almost always the actual cause.
//!
//! Answering it needs a record of the past, which is what [`crate::export`]
//! writes. So a comparison reads a CSV baseline and holds it against a scan
//! taken now.
//!
//! ## Why CSV and not JSON
//!
//! Both are written by hand here, and a correct RFC 4180 reader is forty lines
//! while a correct JSON reader is not. Reading only the format whose escaping
//! we already specify and test keeps the parser small enough to be obviously
//! right — which matters, because a comparison that silently misreads a
//! baseline reports changes that never happened.
//!
//! ## What is compared
//!
//! Paths, relative to each side's own root. That is the export contract, and it
//! is what makes two different roots comparable: a directory against last
//! month's copy of itself, or against the backup that is supposed to mirror it.
//!
//! Files and directories are reported separately. A directory's growth is the
//! sum of its children's, so mixing them into one list would count the same
//! bytes several times over; kept apart, the directories say *where* and the
//! files say *what*.

use std::collections::hash_map::Entry as MapEntry;
use std::io::BufRead;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::node::NodeId;
use crate::tree::SizeTree;

/// One entry as a baseline recorded it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Recorded {
    pub on_disk: u64,
    pub is_dir: bool,
}

/// A past scan, read back from a CSV export.
#[derive(Clone, Debug, Default)]
pub struct Baseline {
    entries: FxHashMap<String, Recorded>,
}

impl Baseline {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Take a baseline straight from a tree.
    ///
    /// The other way in, beside a CSV export. A snapshot holds a whole tree, so
    /// comparing against one needs no serialised middle step — and comparing
    /// two snapshots is then the same operation as comparing a snapshot with
    /// now, which is the point: one comparison engine, not two that can
    /// disagree about what "grew" means.
    ///
    /// Keyed exactly as [`read_csv`] keys a baseline: paths relative to the
    /// tree's own root, forward-slashed. That is what lets a directory be
    /// compared with last month's copy of itself, or with the backup that is
    /// supposed to mirror it.
    pub fn of_tree(tree: &SizeTree) -> Baseline {
        let mut entries = FxHashMap::default();
        entries.reserve(tree.len());
        for_each_relative_path(tree, |id, path| {
            let node = tree.node(id);
            entries.insert(
                path.to_owned(),
                Recorded {
                    on_disk: node.allocated,
                    is_dir: node.is_dir(),
                },
            );
        });
        Baseline { entries }
    }

    pub fn get(&self, path: &str) -> Option<Recorded> {
        self.entries.get(path).copied()
    }
}

/// Why a baseline could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReadError {
    /// The first line was not the header an export writes.
    NotAnExport {
        found: String,
    },
    /// A row had the wrong number of fields, so the columns are not what the
    /// header promised and nothing after it can be trusted.
    BadRow {
        line: usize,
        fields: usize,
    },
    /// A size column that is not a number.
    BadNumber {
        line: usize,
        value: String,
    },
    Io(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NotAnExport { found } => write!(
                f,
                "this does not look like a kosha export — its first line is `{found}`"
            ),
            ReadError::BadRow { line, fields } => {
                write!(f, "line {line} has {fields} fields, not {EXPECTED_FIELDS}")
            }
            ReadError::BadNumber { line, value } => {
                write!(f, "line {line}: `{value}` is not a size")
            }
            ReadError::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// The columns an export writes, in order.
const COLUMNS: [&str; 9] = [
    "path", "name", "kind", "on_disk", "apparent", "items", "modified", "flags", "lossy",
];
const EXPECTED_FIELDS: usize = COLUMNS.len();

/// Read a CSV export back as a baseline.
///
/// The header is checked rather than skipped. Handing this the wrong CSV
/// otherwise produces a comparison in which everything was added and everything
/// was removed, which looks like a catastrophic month rather than a mistake.
pub fn read_csv<R: BufRead>(reader: R) -> Result<Baseline, ReadError> {
    let mut rows = Rows::new(reader);
    let mut entries = FxHashMap::default();

    let header = match rows.next_row()? {
        Some(header) => header,
        None => {
            return Err(ReadError::NotAnExport {
                found: String::new(),
            });
        }
    };
    if header != COLUMNS {
        return Err(ReadError::NotAnExport {
            found: header.join(","),
        });
    }

    let mut line = 1;
    while let Some(row) = rows.next_row()? {
        line += 1;
        // A blank trailing line is what every text editor adds; it is not a row.
        if row.len() == 1 && row[0].is_empty() {
            continue;
        }
        if row.len() != EXPECTED_FIELDS {
            return Err(ReadError::BadRow {
                line,
                fields: row.len(),
            });
        }
        let on_disk = row[3].parse::<u64>().map_err(|_| ReadError::BadNumber {
            line,
            value: row[3].clone(),
        })?;

        let recorded = Recorded {
            on_disk,
            is_dir: row[2] == "dir",
        };
        // Later wins, but a duplicate path in an export is a bug in the writer
        // rather than something to reconcile, and the writer emits each node
        // once.
        match entries.entry(row[0].clone()) {
            MapEntry::Vacant(slot) => {
                slot.insert(recorded);
            }
            MapEntry::Occupied(mut slot) => {
                slot.insert(recorded);
            }
        }
    }

    Ok(Baseline { entries })
}

/// What happened to one path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Grew,
    Shrank,
    Added,
    Removed,
}

/// One difference worth reporting.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub path: String,
    pub kind: Kind,
    pub is_dir: bool,
    /// On-disk bytes in the baseline. Zero for something added.
    pub before: u64,
    /// On-disk bytes now. Zero for something removed.
    pub after: u64,
}

impl Change {
    /// Signed change in on-disk bytes.
    pub fn delta(&self) -> i64 {
        signed_change(self.after, self.before)
    }

    /// How big the change is, whichever way it went.
    ///
    /// Computed on the unsigned sizes rather than from [`delta`](Self::delta),
    /// so it is exact for every pair a baseline can hold rather than only for
    /// the pairs that fit a signed integer. This is what the report sorts on.
    pub fn magnitude(&self) -> u64 {
        self.after.abs_diff(self.before)
    }
}

/// The signed difference between two unsigned sizes.
///
/// `after as i64 - before as i64` was wrong twice over, and a baseline is
/// exactly the input that finds it: a file somebody chose, exported months ago,
/// possibly hand-edited, possibly copied off another machine. "The numbers in
/// it are sane" is not something this gets to assume.
///
/// A size above `i64::MAX` casts to a *negative* number, so one nonsense row
/// would flip the sign of its own change — reporting as freed what the file
/// claims was used. And the subtraction itself overflows: four rows of
/// `9223372036854775807` in a baseline whose paths are gone was an
/// `attempt to subtract with overflow` panic in a debug build, and a wrapped,
/// quietly wrong net total in the release one people actually run. The second
/// is the worse of the two, because nothing about it looks like a failure.
///
/// Saturating rather than wrapping: a difference too large for an `i64` is not
/// a real difference, and pinning it to the end of the range keeps the
/// ordering honest — an absurd row sorts to the top, where somebody will see
/// it, rather than to the middle with a sign flip.
fn signed_change(after: u64, before: u64) -> i64 {
    if after >= before {
        i64::try_from(after - before).unwrap_or(i64::MAX)
    } else {
        i64::try_from(before - after).map_or(i64::MIN, |difference| -difference)
    }
}

/// What to report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Ignore changes smaller than this, in bytes.
    ///
    /// Without a floor the answer to "what changed" is thousands of log files
    /// that moved by a few kilobytes, which buries the one directory that grew
    /// by nine gigabytes.
    pub min_delta: u64,
    /// Include directories, which say *where*.
    pub directories: bool,
    /// Include files, which say *what*.
    pub files: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            min_delta: 1024 * 1024,
            directories: true,
            files: true,
        }
    }
}

/// The result of holding a baseline against a scan.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Every change above the floor, largest magnitude first.
    pub changes: Vec<Change>,
    /// Net on-disk change across **files only**, so a directory total and the
    /// files inside it are never both counted.
    pub net: i64,
    /// Paths in the baseline, for saying how much of it was examined.
    pub baseline_entries: usize,
    /// Paths in the baseline that the scan has too.
    ///
    /// **How much of the record is this folder at all.** A comparison between
    /// two roots is the documented case — a directory against last month's copy
    /// of itself, or against the backup that is supposed to mirror it — and it
    /// is sound exactly as far as the two sides overlap. When they do not, every
    /// path on one side is "gone" and every path on the other is "new", and the
    /// report says so about files that are sitting where they always were.
    pub matched: usize,
    /// Changes dropped for being below `min_delta`.
    pub below_floor: usize,
    /// Every entry the scan has and the record does not, counted before
    /// `min_delta` drops any of them.
    ///
    /// Counted whole because the question it answers is about the *record*
    /// and not about the list: a floor on the report can hide every one of
    /// these while the record stays as incomplete as it was.
    pub added: usize,
    /// The largest of those, and the smallest thing the record holds.
    ///
    /// Together they are the signature of a record that was written with a
    /// size floor — see [`Report::looks_filtered_by_size`].
    pub largest_added: u64,
    pub smallest_recorded: u64,
}

impl Report {
    /// Whether most of the record is missing from the scan it was held against.
    ///
    /// The test both front ends use, here rather than in either of them: the
    /// window and the command line would otherwise draw the line in different
    /// places, and this is the line between "a folder changed" and "these are
    /// two different folders".
    ///
    /// Half, because a record whose paths are mostly not here is not a record of
    /// what is here. A mirror that has drifted matches nearly all of it; a
    /// folder somebody named by mistake matches little or none of it, and the
    /// few it does match are the `README.md` every project has.
    pub fn mostly_missing(&self) -> bool {
        self.baseline_entries > 0 && self.matched * 2 < self.baseline_entries
    }

    /// Whether the record looks like it was written with a size floor.
    ///
    /// **A CSV export made with `--export-min` is a record that did not see
    /// everything, and nothing in the file says so.** A snapshot carries
    /// `Meta::partial` for exactly this, and the comparison prints it above
    /// the list — including for a `--depth`-limited snapshot,
    /// which had reported "two directories and a file as *new* and 880 KiB
    /// as growth" over a tree nobody had touched. A CSV has no header to
    /// carry a flag, and the same hole was still open through it. Measured on
    /// an unchanged tree against an export taken with `--export-min`:
    /// eighteen files reported `new` and `Net +9.00 MiB across files`.
    ///
    /// So it is read off the numbers instead. Every entry the scan has and
    /// the record does not is smaller than the smallest thing the record
    /// holds — which is what a floor does, and what an ordinary month of
    /// changes does not.
    ///
    /// **This states an observation, not a conclusion**, and the caller's
    /// wording has to keep it that way: somebody really can have added
    /// nothing but small files. The observation is worth making anyway,
    /// because the alternative reading is silent and wrong.
    ///
    /// A floor takes out a *class* of entries, so it shows up as a large
    /// share of what is here being absent from the record. One or two new
    /// small files is an ordinary week, and saying this about that would be
    /// the kind of noise that teaches people to skip the line — so the
    /// additions have to be several, and to be a quarter of what the scan
    /// holds. Every entry the scan has either matched the record or is an
    /// addition, so that share is `added` against `matched + added`.
    ///
    /// Conservative at the other end too. No additions, no claim. A record
    /// holding something of zero bytes has no floor above it, so nothing can
    /// be smaller than everything in it and this stays quiet.
    pub fn looks_filtered_by_size(&self) -> bool {
        self.added >= 4
            && self.added.saturating_mul(3) >= self.matched
            && self.baseline_entries > 0
            && self.largest_added < self.smallest_recorded
    }
}

/// Hold a baseline against a scan taken now.
pub fn compare(baseline: &Baseline, tree: &SizeTree, opts: &Options) -> Report {
    let mut report = Report {
        baseline_entries: baseline.len(),
        smallest_recorded: baseline
            .entries
            .values()
            .map(|was| was.on_disk)
            .min()
            .unwrap_or(0),
        ..Report::default()
    };
    // Only matched baseline keys matter for removal detection. Borrow them
    // instead of allocating a second full set of every path in the new tree.
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    seen.reserve(baseline.len().min(tree.len()));

    for_each_relative_path(tree, |id, path| {
        let node = tree.node(id);
        let is_dir = node.is_dir();
        if (is_dir && !opts.directories) || (!is_dir && !opts.files) {
            return;
        }

        let after = node.allocated;

        let change = match baseline.entries.get_key_value(path) {
            Some((key, was)) => {
                seen.insert(key.as_str());
                // Counted before the size test below, because a path that is in
                // both and did not move is the strongest evidence there is that
                // the record really is of this folder.
                report.matched += 1;
                // Directories are excluded from the net because a directory's
                // size is the sum of its children's; counting both would report
                // the same bytes several times over.
                if !is_dir {
                    report.net = report.net.saturating_add(signed_change(after, was.on_disk));
                }
                if after == was.on_disk {
                    return;
                }
                Change {
                    kind: if after > was.on_disk {
                        Kind::Grew
                    } else {
                        Kind::Shrank
                    },
                    is_dir,
                    before: was.on_disk,
                    after,
                    path: path.to_owned(),
                }
            }
            None => {
                if !is_dir {
                    report.net = report.net.saturating_add(signed_change(after, 0));
                }
                // Before the floor below drops it: this counts what the
                // record is missing, not what the report shows.
                report.added += 1;
                report.largest_added = report.largest_added.max(after);
                Change {
                    kind: Kind::Added,
                    is_dir,
                    before: 0,
                    after,
                    path: path.to_owned(),
                }
            }
        };

        if change.magnitude() < opts.min_delta {
            report.below_floor += 1;
            return;
        }
        report.changes.push(change);
    });

    // Anything in the baseline the scan did not reach has gone.
    for (path, was) in &baseline.entries {
        if seen.contains(path.as_str()) {
            continue;
        }
        if (was.is_dir && !opts.directories) || (!was.is_dir && !opts.files) {
            continue;
        }
        if !was.is_dir {
            report.net = report.net.saturating_add(signed_change(0, was.on_disk));
        }
        let change = Change {
            path: path.clone(),
            kind: Kind::Removed,
            is_dir: was.is_dir,
            before: was.on_disk,
            after: 0,
        };
        if change.magnitude() < opts.min_delta {
            report.below_floor += 1;
            continue;
        }
        report.changes.push(change);
    }

    // Largest change first, and path to break ties so the order is total and
    // two runs over an unchanged pair cannot disagree.
    report.changes.sort_unstable_by(|a, b| {
        b.magnitude()
            .cmp(&a.magnitude())
            .then_with(|| a.path.cmp(&b.path))
    });
    report
}

/// A node's path relative to the scan root, in the export's spelling.
fn for_each_relative_path(tree: &SizeTree, mut visit: impl FnMut(NodeId, &str)) {
    let mut path = String::new();
    // One frame per ancestor, not one allocation or root walk per entry.
    let mut stack = vec![(tree.node(tree.root()).child_range(), 0usize)];
    while let Some((children, parent_len)) = stack.last_mut() {
        let Some(index) = children.next() else {
            stack.pop();
            continue;
        };
        path.truncate(*parent_len);
        if !path.is_empty() {
            path.push('/');
        }
        let id = NodeId::from_index(index);
        let name = tree.name_lossy(id);
        path.push_str(&crate::path::slashed(&name));
        visit(id, &path);
        let children = tree.node(id).child_range();
        if !children.is_empty() {
            stack.push((children, path.len()));
        }
    }
}

#[cfg(test)]
fn relative_path(tree: &SizeTree, id: NodeId) -> String {
    let full = tree.path_of(id);
    let relative = full.strip_prefix(tree.root_path()).unwrap_or(&full);
    crate::path::slashed(&relative.to_string_lossy()).into_owned()
}

/// The two ways to build a baseline have to key entries identically, or a
/// comparison against a snapshot and one against an export of the same tree
/// would disagree about what changed. They are separate code, so this is what
/// holds them together.
#[cfg(test)]
mod baseline_agreement {
    use super::*;

    #[test]
    fn a_tree_and_its_own_export_make_the_same_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/deep.bin"), vec![0u8; 4096]).unwrap();
        std::fs::write(root.join("a/side, with comma.txt"), b"x").unwrap();
        std::fs::write(root.join("top.txt"), b"xy").unwrap();
        let tree = crate::Scanner::new(crate::ScanOptions::default())
            .scan(root)
            .unwrap();

        let mut csv = Vec::new();
        crate::export::write_tree(
            &tree,
            &crate::export::Options {
                format: crate::export::Format::Csv,
                min_size: 0,
                ..Default::default()
            },
            &mut csv,
        )
        .unwrap();
        let from_csv = read_csv(csv.as_slice()).unwrap();
        let from_tree = Baseline::of_tree(&tree);

        assert_eq!(
            from_csv.len(),
            from_tree.len(),
            "the two baselines do not even hold the same number of entries"
        );
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            let path = relative_path(&tree, id);
            assert_eq!(
                from_csv.get(&path),
                from_tree.get(&path),
                "{path} differs between a CSV baseline and a tree baseline"
            );
        }

        // Neither holds the root. Its relative path is the empty string and
        // `compare` skips it on the scan side, so a baseline that kept it
        // would report the root as gone on every comparison.
        assert_eq!(from_tree.get(""), None, "a tree baseline kept the root");
        assert_eq!(from_csv.get(""), None, "a CSV baseline kept the root");
    }
}

// --------------------------------------------------------------- CSV reading

/// An RFC 4180 reader.
///
/// Rows rather than lines, because a quoted field may contain a line break —
/// which a filename legally may, on every supported platform. Splitting on
/// newlines would silently cut such a row in half and shift every column after
/// it.
struct Rows<R> {
    reader: R,
    buffer: Vec<u8>,
    at: usize,
    done: bool,
}

impl<R: BufRead> Rows<R> {
    fn new(reader: R) -> Self {
        Rows {
            reader,
            buffer: Vec::new(),
            at: 0,
            done: false,
        }
    }

    /// Read the whole input once, then hand back one row at a time.
    ///
    /// A baseline is a file somebody chose to compare against, not a stream, so
    /// the simple thing is the right thing: a row-at-a-time reader over a
    /// `BufRead` has to handle a quoted newline spanning a buffer boundary, and
    /// that is the case nobody ever tests.
    fn fill(&mut self) -> Result<(), ReadError> {
        if self.done {
            return Ok(());
        }
        self.reader
            .read_to_end(&mut self.buffer)
            .map_err(|e| ReadError::Io(e.to_string()))?;
        self.done = true;
        Ok(())
    }

    fn next_row(&mut self) -> Result<Option<Vec<String>>, ReadError> {
        self.fill()?;
        if self.at >= self.buffer.len() {
            return Ok(None);
        }

        let mut fields = Vec::with_capacity(EXPECTED_FIELDS);
        let mut field = Vec::new();
        let mut quoted = false;

        while self.at < self.buffer.len() {
            let byte = self.buffer[self.at];
            self.at += 1;

            if quoted {
                if byte == b'"' {
                    // A doubled quote is one literal quote; a single one ends
                    // the quoted section.
                    if self.buffer.get(self.at) == Some(&b'"') {
                        field.push(b'"');
                        self.at += 1;
                    } else {
                        quoted = false;
                    }
                } else {
                    field.push(byte);
                }
                continue;
            }

            match byte {
                b'"' if field.is_empty() => quoted = true,
                b',' => fields.push(take(&mut field)),
                b'\n' => {
                    fields.push(take(&mut field));
                    return Ok(Some(fields));
                }
                // A bare CR before the newline is what a CRLF file has.
                b'\r' if self.buffer.get(self.at) == Some(&b'\n') => {}
                other => field.push(other),
            }
        }

        fields.push(take(&mut field));
        Ok(Some(fields))
    }
}

fn take(field: &mut Vec<u8>) -> String {
    // Lossy, because a baseline may contain a path that was already lossy when
    // it was written; the export marks those, and refusing to read the file
    // would be a worse answer than comparing what is there.
    let text = String::from_utf8_lossy(field).into_owned();
    field.clear();
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScanOptions, Scanner, export};

    fn baseline_from(csv: &str) -> Baseline {
        read_csv(std::io::Cursor::new(csv)).unwrap()
    }

    fn export_of(tree: &SizeTree) -> String {
        let mut out = Vec::new();
        export::write_tree(tree, &export::Options::default(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn scan(root: &std::path::Path) -> SizeTree {
        Scanner::new(ScanOptions::default()).scan(root).unwrap()
    }

    /// A tree of `big` large files and `small` small ones.
    fn sized(big: usize, small: usize) -> (tempfile::TempDir, SizeTree) {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..big {
            std::fs::write(dir.path().join(format!("big{n}.bin")), vec![0u8; 3 << 20]).unwrap();
        }
        for n in 0..small {
            std::fs::write(dir.path().join(format!("small{n}.bin")), vec![0u8; 512]).unwrap();
        }
        let tree = scan(dir.path());
        (dir, tree)
    }

    /// Everything the record leaves out reads as created.
    ///
    /// **A CSV export taken with `--export-min` is a record that did not see
    /// everything, and the file cannot say so.** A snapshot carries
    /// `Meta::partial` for this and the comparison prints it; a CSV has no
    /// header to carry a flag. Measured end to end on a tree nobody had
    /// touched, against an export of itself taken with a floor: eighteen
    /// files reported `new`, and `Net +9.00 MiB across files`.
    #[test]
    fn a_record_written_with_a_size_floor_is_noticed() {
        let (_dir, tree) = sized(6, 18);

        let mut out = Vec::new();
        export::write_tree(
            &tree,
            &export::Options {
                format: export::Format::Csv,
                min_size: 1 << 20,
                ..Default::default()
            },
            &mut out,
        )
        .unwrap();
        let baseline = baseline_from(&String::from_utf8(out).unwrap());

        let report = compare(&baseline, &tree, &Options::default());
        assert!(
            report.added >= 18,
            "the floor left {} entries out",
            report.added
        );
        assert!(
            report.looks_filtered_by_size(),
            "a record with a floor went unremarked: {report:?}"
        );
    }

    /// And an ordinary week of small new files does not set it off.
    ///
    /// The line states a true observation and hedges the cause, but a line
    /// that appears whenever somebody saves a few notes is one people learn
    /// to skip. A floor takes out a class of entries; this takes out none.
    #[test]
    fn a_few_new_small_files_are_not_a_floor() {
        let (dir, before) = sized(6, 18);
        let baseline = Baseline::of_tree(&before);

        // Empty, so that it really is smaller than everything in the record
        // and the size half of the test is satisfied. What must keep this
        // quiet is the other half: one addition against two dozen matches is
        // a week, not a floor. A one-byte file would not do -- allocation
        // rounds it up to the same block as the small files already there,
        // and the test would pass without ever reaching the share.
        std::fs::write(dir.path().join("newcomer.bin"), b"").unwrap();
        let after = scan(dir.path());

        let report = compare(&baseline, &after, &Options::default());
        assert_eq!(report.added, 1);
        assert!(
            !report.looks_filtered_by_size(),
            "one new file was called a size floor: {report:?}"
        );

        // Four of them clear the count and still fail the share: four
        // additions against two dozen matches is not a class of entries
        // going missing. This is the half the count alone does not cover.
        for n in 0..4 {
            std::fs::write(dir.path().join(format!("also{n}.bin")), b"").unwrap();
        }
        let after = scan(dir.path());
        let report = compare(&baseline, &after, &Options::default());
        assert_eq!(report.added, 5);
        assert!(
            report.added.saturating_mul(3) < report.matched,
            "the fixture no longer exercises the share: {report:?}"
        );
        assert!(
            !report.looks_filtered_by_size(),
            "five new files in a tree of two dozen were called a floor: {report:?}"
        );

        // And the small comparison the count is there for. One entry
        // recorded, one empty file appearing beside it: the share says yes
        // -- every addition is below everything recorded, and there is one
        // of each -- and a sentence about size floors over a record of one
        // file would be nonsense.
        let tiny = tempfile::tempdir().unwrap();
        std::fs::write(tiny.path().join("only.bin"), vec![0u8; 3 << 20]).unwrap();
        let recorded = Baseline::of_tree(&scan(tiny.path()));
        std::fs::write(tiny.path().join("nothing.bin"), b"").unwrap();
        let report = compare(&recorded, &scan(tiny.path()), &Options::default());
        assert_eq!((report.matched, report.added), (1, 1));
        assert!(
            report.added.saturating_mul(3) >= report.matched,
            "the fixture no longer exercises the count: {report:?}"
        );
        assert!(
            !report.looks_filtered_by_size(),
            "a record of one file was called a floor: {report:?}"
        );
    }

    /// How much of a record is the folder it is held against.
    ///
    /// Two roots are comparable — that is the export contract, and a directory
    /// against the backup that mirrors it is the case it was written for. What
    /// the report rests on is the overlap, and without that figure a record of
    /// somewhere else reads as a list of deletions: both of one folder's files
    /// `gone`, both of another's `new`, while every one of them sits where it
    /// always was.
    #[test]
    fn a_record_of_somewhere_else_is_visible_in_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("here");
        let mirror = dir.path().join("mirror");
        let other = dir.path().join("other");
        for root in [&here, &mirror] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("one.bin"), vec![0u8; 8192]).unwrap();
            std::fs::write(root.join("two.bin"), vec![0u8; 4096]).unwrap();
        }
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("unrelated.bin"), vec![0u8; 8192]).unwrap();

        let baseline = Baseline::of_tree(&scan(&here));
        let opts = Options {
            min_delta: 1,
            ..Options::default()
        };

        // The folder itself: everything in the record is here.
        let same = compare(&baseline, &scan(&here), &opts);
        assert_eq!(same.matched, same.baseline_entries);
        assert!(!same.mostly_missing());

        // A copy of it somewhere else: the comparison the contract is for, and
        // the overlap is total even though the roots are not the same.
        let copy = compare(&baseline, &scan(&mirror), &opts);
        assert_eq!(copy.matched, copy.baseline_entries);
        assert!(!copy.mostly_missing());
        assert!(
            copy.changes.is_empty(),
            "a faithful mirror reports changes: {:?}",
            copy.changes
        );

        // And a folder that has nothing to do with it. The report is still
        // produced — every path gone, one path new — but it says what it is.
        let elsewhere = compare(&baseline, &scan(&other), &opts);
        assert_eq!(elsewhere.matched, 0);
        assert!(elsewhere.mostly_missing());
        assert_eq!(
            elsewhere.changes.len(),
            3,
            "two gone and one new: {:?}",
            elsewhere.changes
        );
    }

    #[test]
    fn what_the_exporter_writes_is_what_the_reader_reads() {
        // The pair has to round-trip or every comparison is against a
        // misreading, and the awkward names are the whole reason to check.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("b,tricky")).unwrap();
        std::fs::write(dir.path().join("b,tricky/one.bin"), vec![0u8; 8192]).unwrap();
        std::fs::write(dir.path().join("plain.txt"), b"hi").unwrap();

        let tree = scan(dir.path());
        let baseline = baseline_from(&export_of(&tree));

        assert!(
            baseline.get("b,tricky").is_some(),
            "a comma broke the reader"
        );
        let one = baseline.get("b,tricky/one.bin").expect("missing file");
        assert_eq!(one.on_disk, tree_size(&tree, "b,tricky/one.bin"));
        assert!(!one.is_dir);
        assert!(baseline.get("b,tricky").unwrap().is_dir);
    }

    /// The exporter and the comparer each spell a path themselves, and if the
    /// two ever disagree `--since` reports every file as gone and every file
    /// as new — a diff of a tree against itself. They had their own copies of
    /// the separator substitution until one of them was wrong.
    ///
    /// A backslash in a name is where they would part company first, so that
    /// is what this compares them on. Windows cannot create the name, which is
    /// the reason the substitution is correct there.
    #[cfg(unix)]
    #[test]
    fn a_tree_compared_against_its_own_export_reports_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/back\\slash.txt"), vec![0u8; 4096]).unwrap();
        std::fs::write(dir.path().join("ordinary.bin"), vec![0u8; 4096]).unwrap();

        let tree = scan(dir.path());
        let baseline = baseline_from(&export_of(&tree));

        let report = compare(&baseline, &tree, &Options::default());
        assert!(
            report.changes.is_empty(),
            "a tree differs from its own export: {:?}",
            report.changes
        );
    }

    fn tree_size(tree: &SizeTree, relative: &str) -> u64 {
        for index in 0..tree.len() {
            let id = NodeId::from_index(index);
            if relative_path(tree, id) == relative {
                return tree.node(id).allocated;
            }
        }
        panic!("{relative} is not in the tree");
    }

    #[test]
    fn quoted_fields_survive_commas_quotes_and_newlines() {
        let csv = concat!(
            "path,name,kind,on_disk,apparent,items,modified,flags,lossy\n",
            "\"a,b\",\"a,b\",dir,100,100,1,1970-01-01T00:00:00Z,,\n",
            "\"say \"\"hi\"\"\",x,file,200,200,,1970-01-01T00:00:00Z,,\n",
            "\"two\nlines\",x,file,300,300,,1970-01-01T00:00:00Z,,\n",
        );
        let baseline = baseline_from(csv);

        assert_eq!(baseline.len(), 3);
        assert_eq!(baseline.get("a,b").unwrap().on_disk, 100);
        assert_eq!(baseline.get("say \"hi\"").unwrap().on_disk, 200);
        assert_eq!(baseline.get("two\nlines").unwrap().on_disk, 300);
    }

    #[test]
    fn crlf_is_read_the_same_as_lf() {
        let unix = concat!(
            "path,name,kind,on_disk,apparent,items,modified,flags,lossy\n",
            "a,a,file,100,100,,1970-01-01T00:00:00Z,,\n",
        );
        let windows = unix.replace('\n', "\r\n");
        assert_eq!(
            baseline_from(unix).get("a"),
            baseline_from(&windows).get("a"),
        );
    }

    #[test]
    fn the_wrong_file_is_refused_rather_than_compared() {
        // Skipping the header instead of checking it would report every path as
        // both added and removed, which reads as a catastrophic month rather
        // than as the mistake it is.
        let err = read_csv(std::io::Cursor::new("name,size\nfoo,1\n")).unwrap_err();
        assert!(matches!(err, ReadError::NotAnExport { .. }), "{err:?}");
        assert!(err.to_string().contains("does not look like"));

        let empty = read_csv(std::io::Cursor::new("")).unwrap_err();
        assert!(matches!(empty, ReadError::NotAnExport { .. }));
    }

    #[test]
    fn a_malformed_row_stops_the_read() {
        let csv = concat!(
            "path,name,kind,on_disk,apparent,items,modified,flags,lossy\n",
            "a,a,file,100,100,,1970-01-01T00:00:00Z,,\n",
            "b,b,file\n",
        );
        let err = read_csv(std::io::Cursor::new(csv)).unwrap_err();
        assert!(matches!(err, ReadError::BadRow { line: 3, .. }), "{err:?}");

        let bad_size = concat!(
            "path,name,kind,on_disk,apparent,items,modified,flags,lossy\n",
            "a,a,file,huge,100,,1970-01-01T00:00:00Z,,\n",
        );
        let err = read_csv(std::io::Cursor::new(bad_size)).unwrap_err();
        assert!(
            matches!(err, ReadError::BadNumber { line: 2, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_trailing_newline_is_not_a_row() {
        let csv = concat!(
            "path,name,kind,on_disk,apparent,items,modified,flags,lossy\n",
            "a,a,file,100,100,,1970-01-01T00:00:00Z,,\n",
            "\n",
        );
        assert_eq!(baseline_from(csv).len(), 1);
    }

    /// A tree, its export, then the tree changed — the actual use case.
    fn changed_fixture() -> (tempfile::TempDir, String, SizeTree) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::write(root.join("logs/app.log"), vec![0u8; 4 * 1024 * 1024]).unwrap();
        std::fs::write(root.join("keep.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(root.join("gone.bin"), vec![0u8; 8 * 1024 * 1024]).unwrap();

        let before = export_of(&scan(root));

        // The log grew, something new appeared, something was deleted, and one
        // file did not move at all.
        std::fs::write(root.join("logs/app.log"), vec![0u8; 20 * 1024 * 1024]).unwrap();
        std::fs::write(root.join("new.bin"), vec![0u8; 6 * 1024 * 1024]).unwrap();
        std::fs::remove_file(root.join("gone.bin")).unwrap();

        let after = scan(root);
        (dir, before, after)
    }

    fn find<'a>(report: &'a Report, path: &str) -> &'a Change {
        report
            .changes
            .iter()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("{path} is not in the report: {:?}", report.changes))
    }

    #[test]
    fn it_finds_what_grew_what_went_and_what_arrived() {
        let (_dir, before, after) = changed_fixture();
        let report = compare(&baseline_from(&before), &after, &Options::default());

        let grew = find(&report, "logs/app.log");
        assert_eq!(grew.kind, Kind::Grew);
        assert_eq!(grew.delta(), 16 * 1024 * 1024);

        assert_eq!(find(&report, "new.bin").kind, Kind::Added);
        assert_eq!(find(&report, "gone.bin").kind, Kind::Removed);
        assert_eq!(find(&report, "gone.bin").delta(), -8 * 1024 * 1024);

        // Unchanged files are not changes.
        assert!(
            !report.changes.iter().any(|c| c.path == "keep.bin"),
            "an unchanged file was reported as a change"
        );
    }

    #[test]
    fn the_net_counts_files_once_and_directories_never() {
        let (_dir, before, after) = changed_fixture();
        let report = compare(&baseline_from(&before), &after, &Options::default());

        // +16 grew, +6 added, -8 removed.
        assert_eq!(report.net, (16 + 6 - 8) * 1024 * 1024);
    }

    #[test]
    fn the_largest_change_comes_first() {
        let (_dir, before, after) = changed_fixture();
        let report = compare(&baseline_from(&before), &after, &Options::default());

        let magnitudes: Vec<u64> = report.changes.iter().map(Change::magnitude).collect();
        assert!(
            magnitudes.windows(2).all(|w| w[0] >= w[1]),
            "{magnitudes:?}"
        );
    }

    #[test]
    fn the_floor_hides_noise_and_says_how_much() {
        let (_dir, before, after) = changed_fixture();
        let high = Options {
            min_delta: 10 * 1024 * 1024,
            ..Options::default()
        };
        let report = compare(&baseline_from(&before), &after, &high);

        assert!(
            report
                .changes
                .iter()
                .all(|c| c.magnitude() >= high.min_delta)
        );
        assert!(report.below_floor > 0, "nothing was reported as hidden");
        // The 16 MiB growth survives a 10 MiB floor; the 6 MiB addition does not.
        assert!(report.changes.iter().any(|c| c.path == "logs/app.log"));
        assert!(!report.changes.iter().any(|c| c.path == "new.bin"));
    }

    #[test]
    fn directories_and_files_can_be_asked_for_separately() {
        let (_dir, before, after) = changed_fixture();
        let baseline = baseline_from(&before);

        let files = compare(
            &baseline,
            &after,
            &Options {
                directories: false,
                ..Options::default()
            },
        );
        assert!(files.changes.iter().all(|c| !c.is_dir));

        let dirs = compare(
            &baseline,
            &after,
            &Options {
                files: false,
                ..Options::default()
            },
        );
        assert!(dirs.changes.iter().all(|c| c.is_dir));
        assert_eq!(dirs.net, 0, "directories must not contribute to the net");
    }

    #[test]
    fn a_tree_compared_with_itself_has_nothing_to_say() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 4 * 1024 * 1024]).unwrap();
        let tree = scan(dir.path());

        let report = compare(
            &baseline_from(&export_of(&tree)),
            &tree,
            &Options::default(),
        );
        assert!(report.changes.is_empty(), "{:?}", report.changes);
        assert_eq!(report.net, 0);
    }
}

/// A baseline is the one input here that kosha did not write the bytes of.
///
/// The four binary formats are swept bit by bit; this was the surface that was
/// not, and it is the one somebody is most likely to hand a damaged file to —
/// a CSV is text, so it gets opened in a spreadsheet, saved by an editor,
/// truncated by a copy that ran out of disk, or edited by hand on purpose.
#[cfg(test)]
mod damaged_baselines {
    use super::*;
    use crate::{ScanOptions, Scanner, export};

    /// A real export, plus the rows a real export cannot produce here.
    ///
    /// The escaping is the interesting half of this reader, and mutating bytes
    /// that never exercise it would be a sweep of the easy path. Windows
    /// cannot create a file whose name holds a newline or a quote, so those
    /// rows are written by hand — which is legitimate, because the question is
    /// what the *reader* does with bytes, and a baseline may have come from any
    /// machine or any editor.
    fn fixture() -> (tempfile::TempDir, SizeTree, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a,b")).unwrap();
        std::fs::write(dir.path().join("a,b/one.bin"), vec![0u8; 8192]).unwrap();
        std::fs::write(dir.path().join("plain.txt"), b"hello").unwrap();
        let tree = Scanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();

        let mut out = Vec::new();
        export::write_tree(
            &tree,
            &export::Options {
                format: export::Format::Csv,
                min_size: 0,
                ..Default::default()
            },
            &mut out,
        )
        .unwrap();
        let mut csv = String::from_utf8(out).unwrap();

        // A quoted field holding a comma and a doubled quote; a quoted field
        // holding a line break, which is the case the row reader exists for;
        // a directory row; and a size wide enough that a flipped digit is
        // still a number.
        csv.push_str("\"odd,name \"\"quoted\"\"\",odd,file,4096,4096,0,,,\n");
        csv.push_str("\"two\nlines\",two,file,1048576,1048576,0,,,\n");
        csv.push_str("deep/dir,dir,dir,4096,0,3,,,\n");
        csv.push_str("big.bin,big.bin,file,9007199254740992,9007199254740992,0,,,\n");
        csv.push_str("lossy.bin,lossy.bin,file,512,512,0,,,yes\n");

        (dir, tree, csv)
    }

    /// The fixture itself has to be a baseline, or the sweep is over rubbish
    /// and every mutation of it is an error for the wrong reason.
    #[test]
    fn the_fixture_is_a_baseline_before_anything_is_done_to_it() {
        let (_dir, _tree, csv) = fixture();
        let baseline = read_csv(csv.as_bytes()).expect("the fixture must parse");
        assert!(baseline.get("odd,name \"quoted\"").is_some(), "quoting");
        assert!(
            baseline.get("two\nlines").is_some(),
            "a line break in a name"
        );
        assert!(
            baseline.get("deep/dir").is_some_and(|row| row.is_dir),
            "a directory row"
        );
        assert_eq!(
            baseline.get("big.bin").map(|row| row.on_disk),
            Some(9_007_199_254_740_992),
            "a wide size"
        );
    }

    /// Reading a baseline is only half of it. A misread one goes straight into
    /// the comparison, so every mutation that *parses* is also held against a
    /// real tree — which is where the arithmetic lives and where the overflow
    /// this found actually fired.
    fn read_and_compare(bytes: &[u8], tree: &SizeTree) {
        if let Ok(baseline) = read_csv(bytes) {
            let report = compare(&baseline, tree, &Options::default());
            // Touch every number the interface would.
            let _ = report.net;
            let _ = report.below_floor;
            let _ = report.baseline_entries;
            for change in &report.changes {
                let _ = change.delta();
                let _ = change.magnitude();
            }
        }
    }

    #[test]
    fn every_flipped_bit_produces_an_answer_or_an_error() {
        let (_dir, tree, csv) = fixture();
        let bytes = csv.as_bytes().to_vec();
        assert!(
            bytes.len() > 100,
            "the fixture stopped producing a baseline worth sweeping"
        );

        let mut swept = 0usize;
        for at in 0..bytes.len() {
            for bit in 0..8 {
                let mut mutated = bytes.clone();
                mutated[at] ^= 1 << bit;
                read_and_compare(&mutated, &tree);
                swept += 1;
            }
        }

        // Computed from what was covered rather than written down, so the
        // number cannot drift, and asserted against a floor so a sweep that
        // quietly covered nothing fails rather than passes.
        assert_eq!(swept, bytes.len() * 8);
        assert!(swept >= 3000, "only {swept} mutations");
    }

    #[test]
    fn every_truncation_produces_an_answer_or_an_error() {
        let (_dir, tree, csv) = fixture();
        let bytes = csv.as_bytes();
        for keep in 0..=bytes.len() {
            read_and_compare(&bytes[..keep], &tree);
        }
    }

    /// The bug the sweep found, kept as the case that names it.
    ///
    /// `after as i64 - before as i64` overflowed on a baseline whose recorded
    /// sizes are near the top of the range and whose paths are gone — a panic
    /// in a debug build, and a wrapped total in the release build people run,
    /// which is the worse half because nothing about it looks like a failure.
    #[test]
    fn enormous_recorded_sizes_do_not_overflow_the_total() {
        let mut csv = COLUMNS.join(",") + "\n";
        for n in 0..4 {
            csv.push_str(&format!(
                "gone{n},gone{n},file,9223372036854775807,0,0,,,\n"
            ));
        }
        let baseline = read_csv(std::io::Cursor::new(csv)).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let tree = Scanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let report = compare(&baseline, &tree, &Options::default());

        assert_eq!(report.changes.len(), 4, "every path in it has gone");
        assert_eq!(
            report.net,
            i64::MIN,
            "a total too large to express must pin to the end of the range"
        );
    }

    /// A size above `i64::MAX` cast to a negative number, so a single nonsense
    /// row reported as *freed* what the file claims was used. The sign is the
    /// whole point of the report.
    #[test]
    fn a_size_above_the_signed_range_does_not_flip_its_own_sign() {
        let mut csv = COLUMNS.join(",") + "\n";
        csv.push_str("gone,gone,file,18446744073709551615,0,0,,,\n");
        let baseline = read_csv(std::io::Cursor::new(csv)).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let tree = Scanner::new(ScanOptions::default())
            .scan(dir.path())
            .unwrap();
        let report = compare(&baseline, &tree, &Options::default());

        let gone = &report.changes[0];
        assert_eq!(gone.kind, Kind::Removed);
        assert!(
            gone.delta() < 0,
            "a file that has gone cannot have grown: {}",
            gone.delta()
        );
        assert_eq!(
            gone.magnitude(),
            u64::MAX,
            "the size it recorded is what went"
        );
    }
}
