// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Headless front-end. Exists for three reasons: scripting, benchmarking, and
//! having somewhere to reproduce a bug report without launching a GPU.
//!
//! Argument parsing is hand-rolled rather than pulling in a parser crate — the
//! surface is a path and eight flags, and this keeps the binary that people
//! benchmark almost free of dependencies that are not being measured.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kosha_analyze::Context;
use kosha_analyze::duplicates::{self, DuplicateOptions};
use kosha_core::{ScanOptions, Scanner, SizeTree, compare, export, format, snapshot};
// `say!` and `sayln!` rather than `print!` and `println!`: a reader is allowed
// to close the pipe, and `println!` panics when one does. See `kosha_core::say`.
use kosha_core::{say, sayln};

mod cache;
mod marketplace;
mod search;
mod snapshots;
mod views;

const USAGE: &str = "\
kosha — scan a directory tree and report what is in it

USAGE:
    kosha <PATH> [OPTIONS]
    kosha <SNAPSHOT.kosha> [OPTIONS]   full/relative path, or exact saved filename
    kosha --snapshots
    kosha --caches [--cache-dir DIR] [--export json|jsonl]
    kosha search --describe \"files larger than ten megabytes\"
    kosha search --load SAVED.kosha \"folders created last week\"
    kosha search --path PATH \"files ending with md\"
    kosha search --help             natural-language search options
    kosha marketplace --help       install and manage marketplace packs
    kosha view --help              publish and query scoped metadata views

OPTIONS:
    --top N          Show the N largest entries at the root (default 15)
    --stats          Print scan timing and memory statistics
    --insights       Run the analysis rules and print the findings
    --cleanup-guide  Show recorded workflow locations and owning-tool cleanup
                     instructions; never executes cleanup or sends data to AI
    --owners         Report how much disk each account is using
    --duplicates     Hunt for duplicate file contents (reads file data)
    --dupe-min N     Smallest file the hunt considers, in bytes (default 1 MiB);
                     asks for the hunt, so --duplicates need not be given too
    --query Q        Show entries matching a query, e.g. \"size > 1gb\"
    --query-help     Explain the query language
    --export FORMAT  Write every entry as txt, csv, jsonl, or json
    --absolute-paths Use full paths in CSV/JSON/JSONL; requires --export
    --out FILE       Write the export here instead of standard output; the
                     export flag is what says which format to write
    --export-min N   Leave entries smaller than N bytes out of the export
    --snapshot       Keep this scan, so a later one can be compared with it
    --label TEXT     Name the snapshot (optional; the date and path already
                     describe it)
    --snapshots      List the snapshots kept, and what they occupy
    --caches         List cached drives (implicit snapshots), age and record paths;
                     reads headers only; no scan, refresh or tree decompression
    --since FILE     Compare against a snapshot or a CSV export and report
                     what changed
    --since-min N    Ignore changes smaller than N bytes (default 1 MiB)
    --depth N        Stop descending below depth N
    --cache          Read the GUI's last completed record for this exact root;
                     missing/corrupt cache is an error, never a fallback scan
    --cache-dir DIR  Use this cache directory instead; requires --cache or --caches
    --cross-fs       Descend into mounted filesystems (never into links)
    --exact          Query exact on-disk sizes, even where it costs a syscall
    --mft            Read NTFS' Master File Table instead of walking directories
                     (Windows, NTFS, and an elevated process; falls back and
                     says so where any of those is missing)
    --image          Read a WSL distribution's ext4.vhdx instead of going
                     through the wsl.localhost redirector, or read a disk
                     image named as the path: with or without a VHDX around
                     it, partitioned or not (ext2, ext3, ext4, NTFS, XFS,
                     FAT12/16/32, exFAT, ISO 9660 and UDF; falls back and says
                     so where the image cannot be read)
    --hardlinks      Count hardlinked bytes once even during a folder walk on
                     Windows, where learning that a file is hardlinked costs an
                     open per file. Done anyway, on both platforms, by an MFT
                     scan and by every disk-image reader, which have the answer
                     already
    --no-hardlinks   Count every name's bytes separately, like du -l
    --decimal        Use GB (10^9) instead of GiB (2^30)
    -V, --version    Show the version, commit and target
    -h, --help       Show this message

AUTOMATION / LLM USE:
    A directory PATH always gets a fresh, full-depth scan by default. Only
    --depth limits depth; filesystem boundaries and links are not followed
    unless described above. No automatic cache is read or updated by a scan.
    --cache explicitly opts into historical data, shared with the GUI by
    default. Its original timestamp and age are printed to stderr. It does not
    rescan, refresh, or silently fall back on a cache miss. Use an absolute
    drive root (for example C:\\) for a GUI drive record; folders need a record
    saved for that exact root. --cache rejects scan options and duplicate hunts.

    For machine output, use --export json (metadata + entries), jsonl (one
    entry per line), csv, or txt (one full path per line). --top limits only
    the human report, never exports. Sizes are integer bytes. CSV/JSON paths
    and JSONL paths are relative to the root unless --absolute-paths is given;
    TXT always uses full paths. Diagnostics go to stderr
    when exporting to stdout. Exit 0 means success, including no matches;
    exit 1 means invalid arguments, unreadable source, cancellation, or I/O
    failure. Partial permission-limited scans can succeed: inspect partial
    metadata/diagnostics. Output file writes are explicit; no files are deleted.

EXAMPLES:
    kosha . --snapshot --label \"Before cleanup\"
    kosha --snapshots
    kosha saved-scan.kosha --stats
    kosha . --since saved-scan.kosha --since-min 0
    kosha newer.kosha --since older.kosha
    kosha . --duplicates --dupe-min 1048576
    kosha . --hardlinks --stats
    kosha --caches
    kosha --caches --export json
    kosha --caches --cache-dir ./drive-cache --export jsonl
    kosha . --query \"file and size > 10mb\" --export jsonl
    kosha . --export txt --out paths.txt
    kosha C:\\ --cache --export json
    kosha search --describe --json \"files created last week\"
    kosha search --path . --export csv \"documents except pdf\"
    kosha search --help

SNAPSHOT INPUT:
    Existing local files/directories take precedence. A missing bare NAME.kosha
    is looked up by exact filename in the shared saved-snapshot directory.
    Full paths and ./NAME.kosha always select that path. The same lookup works
    for --since and search --load. Labels are descriptive, not lookup names.
    --duplicates reports identical contents; it does not delete or deduplicate
    files. --hardlinks counts shared hardlinked storage once during a scan.
";

#[derive(Debug)]
struct Args {
    path: PathBuf,
    query: Option<String>,
    top: usize,
    stats: bool,
    insights: bool,
    cleanup_guide: bool,
    /// Report disk use per account. Turns on `ScanOptions::collect_owners`,
    /// because there is no point asking afterwards for something the scan had
    /// to record as it went.
    owners: bool,
    duplicates: bool,
    /// Smallest file `--duplicates` will consider.
    dupe_min: u64,
    /// Set by `--export`. Combines with `--query`, which then decides the rows.
    export: Option<export::Options>,
    /// Where the export goes. `None` is standard output.
    out: Option<PathBuf>,
    /// A snapshot or a CSV export to compare this scan against.
    since: Option<PathBuf>,
    /// Keep this scan as a snapshot.
    snapshot: bool,
    /// What to call it. Empty is the usual case.
    label: String,
    compare: compare::Options,
    scan: ScanOptions,
    units: format::Units,
    cache: cache::Cache,
    absolute_paths: bool,
}

impl Args {
    /// True when standard output is carrying an export rather than a report.
    /// The first flag given that only means something to a scan, if any.
    ///
    /// Named rather than counted so the refusal can say which one, and ordered
    /// as the help screen lists them so two given together complain about the
    /// first a reader would look for.
    ///
    /// `--owners` is deliberately not here: a snapshot carries the owners it was
    /// scanned with, so asking for that table is asking for something the record
    /// can answer. Neither is `--query` or `--insights`, for the same reason —
    /// they work on a tree and do not care where it came from.
    fn scan_only_flag(&self) -> Option<&'static str> {
        if self.scan.max_depth.is_some() {
            return Some("--depth");
        }
        if self.scan.cross_filesystems {
            return Some("--cross-fs");
        }
        if self.scan.exact_allocated {
            return Some("--exact");
        }
        if self.scan.use_mft {
            return Some("--mft");
        }
        if self.scan.use_image {
            return Some("--image");
        }
        match self.scan.dedup_hardlinks {
            Some(true) => Some("--hardlinks"),
            Some(false) => Some("--no-hardlinks"),
            None => None,
        }
    }

    fn document_on_stdout(&self) -> bool {
        self.export.is_some() && self.out.is_none()
    }
}

fn main() -> ExitCode {
    let result = match parse() {
        Ok(Parsed::Run(args)) => run(*args),
        Ok(Parsed::View(args)) => return views::run(&args),
        Ok(Parsed::Caches(args)) => cache::list(&args),
        Ok(Parsed::Search(args)) => search::run(&args),
        Ok(Parsed::Marketplace(args)) => marketplace::run(&args).map_err(Into::into),
        Ok(Parsed::SearchRecipes(Some(path), filter)) => {
            search::list_pack(&path, filter.as_deref()).map_err(Into::into)
        }
        Ok(Parsed::SearchRecipes(None, _)) => {
            if kosha_search::BUILTIN_RECIPE_GROUPS.is_empty() {
                sayln!(
                    "No platform-specific recipes for {}. Use kosha search --help for general examples.",
                    std::env::consts::OS
                );
            }
            for group in kosha_search::BUILTIN_RECIPE_GROUPS {
                sayln!("{}", group.label);
                for (phrase, _) in group.recipes {
                    sayln!("  {phrase}");
                }
            }
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::SearchHelp) => {
            say!("{}", search::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Usage) => {
            say!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::QueryHelp) => {
            say!("{}", query_help());
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            sayln!("{}", kosha_core::version::long());
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Snapshots) => {
            print_snapshots();
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        // Ctrl-C is an answer, not a fault. The walk stops at a level boundary
        // and still prints what it found, which is the whole reason the handler
        // exists; the MFT strategy has nothing to print until the table is
        // read, so an interrupt during one arrives here. Still a non-zero exit,
        // because the scan did not finish — but not under a heading that says
        // something went wrong.
        Err(e)
            if matches!(
                e.downcast_ref::<kosha_core::ScanError>(),
                Some(kosha_core::ScanError::Cancelled)
            ) =>
        {
            eprintln!("cancelled");
            ExitCode::FAILURE
        }
        // Whoever was reading stopped: `kosha … --export csv | head`. The
        // export writes straight to standard output rather than through
        // `sayln!`, so it arrives here as an ordinary error, and it is not
        // one — the pipeline ended, which is the reader's prerogative. Same
        // rule and same exit status as every other line kosha prints; see
        // `kosha_core::say`.
        Err(e) if kosha_core::say::reader_is_gone_boxed(&*e) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            // The source chain carries the real cause — usually a permission
            // error naming the exact path.
            let mut source = std::error::Error::source(&*e);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            // The one error whose fix is a flag this program has. The core
            // cannot say it — the window reads an image handed to it and has
            // no flags to offer — so the suggestion belongs here, where
            // `--image` is a real thing to type. (The window's half of that
            // sentence was not true until `state::opening`: a path given to
            // it was scanned with the settings' options, so an image handed
            // to the window hit this same refusal.)
            if matches!(
                e.downcast_ref::<kosha_core::ScanError>(),
                Some(kosha_core::ScanError::ImageNotRead(_))
            ) {
                eprintln!("  try: kosha --image <path>");
            }
            ExitCode::FAILURE
        }
    }
}

/// Everything that can be refused without reading a disk, refused first.
///
/// **A three-letter typo used to cost a whole scan.** Each of these was
/// discovered by the code that needed it, and every one of those lives after
/// the tree is built, so the scan ran to completion and *then* the mistake
/// was reported. Measured against `C:\Users\…\AppData\Local`, 1.28 million
/// entries, with a plain scan of the same tree for comparison:
///
/// ```text
/// plain scan        3.5 s
/// missing baseline  3.8 s   error: could not read J:\nope\baseline.csv
/// bad query         4.5 s   error: `sze` is not a field (did you mean `size`?)
/// bad --out         4.6 s   error: could not write J:\nope\deeper\out.csv
/// ```
///
/// Cold, that scan is 32 seconds. None of the three answers needs a single
/// byte off the disk, and the query error even knows what the person meant.
///
/// What each check is, and is not:
///
/// - The query is *parsed*, which is the whole of it — one that parses here
///   cannot fail later.
/// - The baseline is *opened*, which catches the path being wrong and not
///   the contents being wrong. Reading it here instead would hold a second
///   tree in memory for the length of the scan, and a mistyped path is the
///   commoner mistake by far.
/// - The export's destination is checked for a directory to write into.
///   Permission is not, because finding that out means creating a file, and
///   leaving litter behind to prove a scan could have written something is a
///   worse trade than a late error.
fn refuse_before_scanning(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(source) = &args.query {
        kosha_query::Query::parse(source)?;
    }

    if let Some(baseline) = &args.since {
        std::fs::File::open(baseline)
            .map_err(|e| format!("could not read {}: {e}", baseline.display()))?;
    }

    if let Some(out) = &args.out {
        // An empty parent is a bare filename, which means the directory this
        // was run from, and that exists by definition.
        let parent = out.parent().unwrap_or_else(|| std::path::Path::new(""));
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(format!(
                "could not write {}: there is no directory {}",
                out.display(),
                parent.display()
            )
            .into());
        }
    }

    Ok(())
}

fn run(mut args: Args) -> Result<(), Box<dyn std::error::Error>> {
    if !args.cache.enabled {
        args.path = snapshots::resolve(&args.path)?;
    }
    if let Some(path) = &args.since {
        args.since = Some(snapshots::resolve(path)?);
    }
    let args = &args;
    refuse_before_scanning(args)?;

    // Ctrl-C stops the scan and still prints what was found, rather than
    // discarding minutes of work on a large volume.
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed));

    // A snapshot is browsed exactly like a disk: everything below this line —
    // the treemap numbers, the queries, the insights, the duplicate hunt's
    // candidate list — works on a tree and does not care where it came from.
    // The one thing that does care is the duplicate hunt itself, which reads
    // file contents, and it says so.
    let tree = if args.cache.enabled {
        let (meta, tree) = args.cache.open(&args.path)?;
        if !args.document_on_stdout() {
            print_snapshot_header(&meta);
        }
        tree
    } else if snapshot::is_snapshot(&args.path) {
        // **How to scan a disk says nothing about a record of one.** Six flags
        // were accepted here and did nothing at all: `--depth`, `--cross-fs`,
        // `--exact`, `--mft`, `--image`, and either of the hardlink flags. The
        // worst of them reads as a promise — `kosha snapshot.kosha --mft` looks
        // like it is about to read a Master File Table, and opens a file. The
        // rule is the one above `--label`: a flag taken and ignored is worse than
        // one refused, and it was applied to `--since` here and to nothing else.
        //
        // Checked here rather than in the parser because *this* is where the
        // answer is known: a snapshot is recognised by its first eight bytes and
        // not by its name, which is a decision `is_snapshot` documents.
        if let Some(flag) = args.scan_only_flag() {
            return Err(format!(
                "`{flag}` says how to scan a disk, and `{}` is a snapshot — it was \
                 scanned when it was taken",
                args.path.display()
            )
            .into());
        }
        // **A copy of a record is not a record of today.** `--snapshot` here
        // wrote a second file holding the same tree, stamped with this minute,
        // and the listing then showed a scan that never happened:
        //
        //     2026-09-27 19:46:38Z  2 entries  …\twosnap  (first)
        //     2026-09-27 20:00:47Z  2 entries  …\twosnap  (a copy with a new
        //                                                  date)
        //
        // Worse than a wrong date on a row: a comparison against the copy
        // takes 20:00 as "then", so everything that happened between the two
        // stamps is invisible. The window has refused it from the start, in
        // its own words — "Snapshotting a snapshot is a copy of a file with a
        // new date on it, which would be a lie about when it was taken" — and
        // the command line did it anyway.
        if args.snapshot {
            return Err(format!(
                "`{}` is already a snapshot — keeping it again would write a copy \
                 of it stamped with now, which is a scan that never happened",
                args.path.display()
            )
            .into());
        }
        // **A hunt for duplicates is a question about bytes, and a record
        // holds none.** A snapshot says how big every file was and where it
        // sat; deciding whether two are identical means reading them off the
        // disk *as it is now*, so the answer arrives under a header saying
        // this is the past. Measured on one record, three times over: with
        // the disk unchanged it reported the pair; with one file rewritten it
        // reported none; with one deleted it reported "1 could not be read".
        // Three answers from one record, none of them about the record.
        //
        // The window has refused it from the start and says why in
        // `find_duplicates`. This is the same refusal on the other front end.
        if args.duplicates {
            return Err(format!(
                "`{}` is a snapshot, and a duplicate hunt has to read the files \
                 themselves — a record holds their sizes, not their bytes, so the \
                 answer would be about the disk as it is now",
                args.path.display()
            )
            .into());
        }
        let (meta, tree) = snapshot::store::open(&args.path)
            .map_err(|e| format!("{}: {e}", args.path.display()))?;
        snapshots::report_source(&args.path, &meta);
        if !args.document_on_stdout() {
            print_snapshot_header(&meta);
        }
        tree
    } else {
        let tree = Scanner::new(args.scan)
            .with_cancel(Arc::clone(&cancel))
            .scan(&args.path)?;
        // **A folder nothing could be read from is not a folder with nothing
        // in it.** Measured on `C:\System Volume Information`, which an
        // ordinary account cannot open: the scan reported `~0 B on disk`, one
        // line of "1 entry could not be read", and **exit 0** — a number a
        // script would take, about a folder kosha never looked into. A path
        // that is a file, or is not there at all, already ends in a sentence
        // and a non-zero exit; this is the third way a root can be unusable
        // and it was the one that answered.
        if tree.root_unreadable() {
            return Err(format!(
                "{}: it could not be read{}",
                format::printable(&args.path.to_string_lossy()),
                if cfg!(windows) {
                    " — an administrator can, and `kosha --mft` reads a whole volume that way"
                } else {
                    " — check the permissions on it, or try it as its owner"
                }
            )
            .into());
        }
        tree
    };

    // Exporting to standard output means standard output is the document, and
    // nothing else may go there — a CSV with a treemap glued to the front of it
    // is a CSV that fails to parse somewhere else, later, for reasons nobody
    // can see. The report is not lost: `run_export` writes its summary to
    // stderr, and `--out` keeps the human report on stdout where it belongs.
    if !args.document_on_stdout() {
        print_header(&tree, args);
        print_largest(&tree, args);

        if args.stats {
            print_stats(&tree);
        }
        if args.insights {
            print_insights(&tree, args);
        }
        if args.cleanup_guide {
            sayln!("\nCLEANUP GUIDE — recorded data, not guaranteed savings");
            let guides = kosha_analyze::cleanup::guides(&tree, &Context::detect());
            if guides.is_empty() {
                sayln!("  No supported locations recorded. Custom locations may not be detected.");
            }
            for guide in guides {
                sayln!(
                    "  {} · {} · {} recorded",
                    guide.workflow.label(),
                    guide.title,
                    format::bytes_in(tree.node(guide.node).allocated, args.units)
                );
                sayln!("    {}\n    {}", guide.steps, guide.documentation);
            }
            sayln!(
                "  Use the owning application's controls and rescan afterwards. No commands were executed."
            );
        }
        if args.owners {
            print_owners(&tree, args);
        }
        if args.duplicates {
            print_duplicates(&tree, args);
        }
        if let Some(query) = &args.query {
            print_query(&tree, query, args)?;
        }
        if let Some(baseline) = &args.since {
            print_changes(&tree, baseline, args)?;
        }
    }
    if args.snapshot {
        keep_snapshot(&tree, args)?;
    }
    if args.export.is_some() {
        warn_export_coverage(&tree);
    }
    if let Some(options) = &args.export {
        run_export(&tree, options, args)?;
    }
    if tree.stats().cancelled {
        return Err(kosha_core::ScanError::Cancelled.into());
    }
    Ok(())
}

/// A rows-only stream cannot carry scan coverage, and human headers are
/// intentionally suppressed on machine stdout. Keep lower bounds visible on
/// stderr for every export format, including exports loaded from records.
fn warn_export_coverage(tree: &SizeTree) {
    let stats = tree.stats();
    if tree.is_incomplete() {
        eprintln!(
            "warning: exported source is incomplete (cancelled={}, depth_limited={}, unreadable_entries={}); results and bytes are lower bounds",
            stats.cancelled, stats.depth_limited, stats.unreadable
        );
    } else if tree.is_partial() || stats.unreadable > 0 {
        eprintln!(
            "warning: exported source is partial due to inaccessible or unreadable entries (unreadable_entries={}); results and bytes are lower bounds",
            stats.unreadable
        );
    }
}

/// Keep this scan.
fn keep_snapshot(tree: &SizeTree, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let taken = std::time::SystemTime::now();
    let path = snapshot::store::save(tree, &args.label, taken)?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    if args.document_on_stdout() {
        eprintln!(
            "snapshot kept at {} ({} bytes)",
            format::printable(&path.to_string_lossy()),
            bytes
        );
        return Ok(());
    }

    sayln!("\nSNAPSHOT");
    sayln!("  kept at            {}", path.display());
    sayln!("  {:<18} {}", "size", format::bytes_in(bytes, args.units));
    // The ratio, because it is the number somebody deciding whether to keep
    // ten of these actually wants, and because it is the claim this format
    // makes for itself.
    let arena = (tree.len() * 48) as u64;
    if arena > 0 {
        sayln!(
            "  {:<18} {} of arena, {}%",
            "against",
            format::bytes_in(arena, args.units),
            bytes.saturating_mul(100) / arena.max(1)
        );
    }
    sayln!(
        "  {:<18} kosha --since \"{}\"",
        "compare with",
        path.display()
    );
    Ok(())
}

/// What a snapshot says about itself, before its tree is reported like any
/// other.
///
/// **Its label and its root are text out of a file**, and this is the one place
/// they become lines on a terminal. A snapshot is a thing people hand to each
/// other — that is what `--since` is for — so the label went through
/// [`format::printable`] for the same reason every name in the table below it
/// already does. Measured with `--label` and one escape: `red\x1b[31m\n` plus a
/// forged row put a second snapshot in the listing that does not exist.
fn print_snapshot_header(meta: &snapshot::Meta) {
    sayln!("SNAPSHOT  {}", format_when(meta.taken));
    if !meta.label.is_empty() {
        sayln!("  {}", format::printable(&meta.label));
    }
    sayln!(
        "  taken of  {}",
        format::printable(&meta.root.to_string_lossy())
    );
    if meta.partial || meta.cancelled || meta.depth_limited {
        sayln!(
            "  {} — {}, so totals are a floor",
            if meta.cancelled || meta.depth_limited {
                "incomplete"
            } else {
                "partial"
            },
            if meta.cancelled {
                "the scan was stopped"
            } else if meta.depth_limited {
                "the scan stopped at a depth ceiling"
            } else {
                "the scan could not read everything"
            }
        );
    }
}

/// List what has been kept.
fn print_snapshots() {
    let listing = snapshot::store::list_all();
    let kept = &listing.kept;
    if kept.is_empty() {
        match snapshot::store::dir() {
            Some(dir) => sayln!("No snapshots kept yet. They would go in {}", dir.display()),
            None => sayln!("No snapshots, and nowhere on this platform to keep them."),
        }
        // "No snapshots kept yet" over a directory holding three damaged ones
        // is the same wrong answer as "1 snapshot" over a directory holding
        // four — so the note goes here too, and before the early return that
        // used to be the end of it.
        print_unreadable_snapshots(&listing);
        return;
    }

    sayln!("{:<23}  {:>10}  {:>12}  OF", "TAKEN", "ENTRIES", "SIZE");
    for one in kept {
        sayln!(
            "{:<23}  {:>10}  {:>12}  {}{}",
            format_when(one.meta.taken),
            format::count(one.meta.entries),
            format::bytes_in(one.bytes, format::Units::Binary),
            format::printable(&one.meta.root.to_string_lossy()),
            if one.meta.label.is_empty() {
                String::new()
            } else {
                format!("  ({})", format::printable(&one.meta.label))
            },
        );
    }
    sayln!(
        "\n{} occupying {}",
        format::plural(kept.len(), "snapshot"),
        format::bytes_in(snapshot::store::total_bytes(kept), format::Units::Binary)
    );
    for one in kept {
        sayln!("  {}", one.path.display());
    }
    print_unreadable_snapshots(&listing);
}

/// The files in the store that are not snapshots anybody can open.
///
/// **A damaged snapshot used to disappear.** The listing skips what it cannot
/// read — rightly, so one bad file cannot hide the nine good ones — and then
/// nothing said it had. `kosha --snapshots` reported "1 snapshot" over a
/// directory holding four, while the other three sat there occupying their
/// megabytes, which is exactly the reading somebody takes as "my snapshot was
/// never kept".
///
/// Named with the reason, because the reasons differ and one of them is not
/// damage at all: a file somebody dropped in the directory that was never a
/// snapshot says so.
fn print_unreadable_snapshots(listing: &snapshot::store::Listing) {
    if listing.unreadable.is_empty() {
        return;
    }
    sayln!(
        "\n{} here could not be read:",
        format::plural(listing.unreadable.len(), "file")
    );
    for one in &listing.unreadable {
        sayln!(
            "  {}  —  {}",
            format::printable(&one.path.to_string_lossy()),
            one.why
        );
    }
}

/// Unix seconds as something a person can read.
///
/// Local time would be friendlier and is not worth a dependency or a
/// hand-rolled timezone database; UTC is unambiguous, which for a stamp that
/// exists to order two records is the property that matters.
fn format_when(secs: u64) -> String {
    let days = secs / 86_400;
    let time = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    // To the second, because the minute is not enough to tell two apart: a
    // folder that scans in a moment is one somebody snapshots twice in a row,
    // and a listing where both rows read `13:18Z` is a listing where the two
    // are indistinguishable — which was half of what made losing one of them
    // invisible.
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Days since the epoch to a calendar date.
///
/// Howard Hinnant's `civil_from_days`, which is the standard way to do this
/// without a calendar library: shift the year to start in March so the leap day
/// lands at the end of it, and the month lengths become a linear formula.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Report what has changed since a baseline export.
///
/// "What is big" is dominated by things that have always been big and are
/// supposed to be. "What grew" is almost always the actual cause of a disk
/// filling up, and it is the question this answers.
fn print_changes(
    tree: &SizeTree,
    baseline_path: &std::path::Path,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    // Either kind of record, told apart by what is in the file rather than by
    // its name: a snapshot is recognised by its magic, and anything else is
    // read as the CSV export this flag has always taken.
    let mut taken_at = None;
    let mut floor = None;
    let mut elsewhere = None;
    let mut counted_differently = None;
    let mut crossed_differently = None;
    let mut measured_differently = None;
    let baseline = if snapshot::is_snapshot(baseline_path) {
        let (meta, past) = snapshot::store::open(baseline_path)
            .map_err(|e| format!("{}: {e}", baseline_path.display()))?;
        snapshots::report_source(baseline_path, &meta);
        taken_at = Some(meta.taken);
        // A snapshot knows which folder it is of, so a comparison against
        // another folder can be *said* rather than merely performed. Comparing
        // two roots is the documented case — see `compare`, which was written
        // for a directory against the backup that mirrors it — so this is a
        // fact stated, not a refusal.
        if meta.root != tree.root_path() {
            elsewhere = Some(meta.root.clone());
        }
        // **Two counting rules are two different questions.** A hardlinked
        // file's bytes belong to one of its names under one rule and to all of
        // them under the other, so a record taken under one and a scan under
        // the other disagree about a tree nothing has touched. Measured: five
        // names for one 8 MiB file, snapshotted with folding on and scanned
        // with it off, came back as `+32.0 MiB` of growth across four files.
        //
        // Not hypothetical on Windows, where the rule belongs to the strategy:
        // an elevated `--mft` snapshot folds hardlinks and the ordinary walk
        // does not, so the two commands somebody is most likely to run in that
        // order are exactly the pair that disagree.
        if meta.dedup_hardlinks != tree.stats().dedup_hardlinks {
            counted_differently = Some(meta.dedup_hardlinks);
        }
        // And the other rule that decides which bytes are in the answer at
        // all. Measured on `/run`, which has a handful of mounts inside it: a
        // record taken with `--cross-fs`, held against an ordinary scan,
        // reported every file on every one of them as *gone*.
        if meta.cross_filesystems != tree.stats().cross_filesystems {
            crossed_differently = Some(meta.cross_filesystems);
        }
        // **And the third rule of the same kind.** On Windows an ordinary
        // file's on-disk size is estimated from the cluster size unless
        // `--exact` was given, and for a file small enough to live inside the
        // Master File Table the estimate is not close — 4096 against 8. So a
        // record taken one way and a scan taken the other disagree about
        // every small file. Measured: a snapshot with `--exact`, held against
        // an ordinary scan of the same untouched directory, reported four
        // files as having grown and `Net +15.0 KiB across files`.
        //
        // `None` is a record written before this was carried. It did not say,
        // so nothing is said about it.
        if let Some(then) = meta.exact_allocated
            && then != tree.stats().exact_allocated
        {
            measured_differently = Some(then);
        }
        // **A record that did not see everything invents changes.** Every path
        // the earlier scan never reached comes back as *new*, and its bytes as
        // growth — which is what happened with a `--depth`-limited snapshot: a
        // whole subtree reported as created, 880 KiB of it, none of it true. The
        // snapshot says it is a floor; this is where saying so matters, because
        // this is the reading it changes.
        if meta.partial {
            floor = Some(if meta.depth_limited {
                "it stopped at a depth ceiling"
            } else {
                "it could not read everything"
            });
        }
        compare::Baseline::of_tree(&past)
    } else {
        let file = std::fs::File::open(baseline_path)
            .map_err(|e| format!("could not read {}: {e}", baseline_path.display()))?;
        compare::read_csv(std::io::BufReader::new(file))
            .map_err(|e| format!("{}: {e}", baseline_path.display()))?
    };

    let report = compare::compare(&baseline, tree, &args.compare);

    // A snapshot knows when it was taken; a CSV export does not, and the
    // file is all there is to name it by. So the line says whichever is the
    // more useful thing about the record in hand.
    match taken_at {
        Some(when) => sayln!("\nSINCE  {}", format_when(when)),
        None => sayln!("\nSINCE  {}", baseline_path.display()),
    }
    sayln!(
        "  {} then, {} now",
        format::plural(report.baseline_entries, "entry"),
        format::plural(tree.len().saturating_sub(1), "entry"),
    );
    if let Some(why) = floor {
        sayln!(
            "  the record is incomplete — {why} — so anything below that reads \
             as new here",
        );
    }
    // **And the same thing the other way round, which nothing said.** A
    // record that did not see everything reports what it missed as created;
    // a *scan* that does not see everything reports what it missed as gone,
    // and that half was silent. Measured on a complete record held against a
    // scan of the same untouched tree stopped at depth 1: every file below
    // the ceiling listed as `gone`.
    //
    // The scan knows, so this needs no guessing — `partial` is set by the
    // same two causes on both sides, and the front ends say which.
    if tree.stats().partial {
        sayln!(
            "  this scan is incomplete — {} — so anything it did not reach \
             reads as gone here",
            if tree.stats().depth_limited {
                "it stopped at a depth ceiling"
            } else {
                "it could not read everything"
            }
        );
    }
    // **A record of another folder reports files as gone while they sit where
    // they always were.** Measured: a snapshot of one folder held against
    // another listed both of the first folder's files as `gone` and the second
    // folder's as `new`, and said nothing about the two being unrelated — while
    // the window refused the same pair outright. Neither is right. The
    // comparison is the one `compare` documents, and what it rests on is how
    // much of the record is here, so that is what is printed.
    if let Some(root) = &elsewhere {
        sayln!(
            "  the record is of {}",
            format::printable(&root.to_string_lossy())
        );
    }
    if let Some(then) = counted_differently {
        sayln!(
            "  the record counted a hardlinked file's bytes {}, and this scan \
             counts them {} — anything hardlinked differs for that reason \
             alone",
            if then { "once" } else { "under every name" },
            if then { "under every name" } else { "once" },
        );
    }
    if let Some(then) = measured_differently {
        sayln!(
            "  the record {} every on-disk size and this scan {} \u{2014} a file \
             small enough to sit inside the Master File Table differs for that \
             reason alone",
            if then { "measured" } else { "estimated" },
            if then { "estimates" } else { "measures" },
        );
    }
    if let Some(then) = crossed_differently {
        sayln!(
            "  the record {} into other filesystems and this scan {} \u{2014} what \
             is mounted inside this folder differs for that reason alone",
            if then { "descended" } else { "did not descend" },
            if then { "does not" } else { "does" },
        );
    }
    if report.mostly_missing() {
        sayln!(
            "  {} of {} in it {} here, so most of what follows is the difference \
             between two folders rather than a change",
            format::count(report.matched as u64),
            format::plural(report.baseline_entries, "path"),
            if report.matched == 1 { "is" } else { "are" },
        );
    }
    // **And the same hole, through the other kind of record.** A snapshot
    // says `partial` and the line above prints it. A CSV export has no
    // header to say anything in, so an export taken with `--export-min` is
    // an incomplete record that looks exactly like a complete one — and a
    // comparison against it reports everything it left out as created.
    // Measured on a tree nobody had touched, against an export of itself
    // taken with a floor: eighteen files `new` and `Net +9.00 MiB across
    // files`.
    //
    // Read off the numbers, since the file will not say: everything the scan
    // has and the record does not is smaller than the smallest thing in the
    // record. Stated as the observation it is rather than as a conclusion,
    // because somebody really can have added nothing but small files — and
    // worth stating either way, because the other reading is silent.
    if report.looks_filtered_by_size() {
        sayln!(
            "  {} here {} in the record, and {} smaller than anything in it — \
             which is what a record exported with a size floor looks like",
            format::plural(report.added, "entry"),
            if report.added == 1 {
                "is not"
            } else {
                "are not"
            },
            if report.added == 1 {
                "it is"
            } else {
                "they are"
            },
        );
    }

    if report.changes.is_empty() {
        sayln!(
            "\n  Nothing changed by more than {}.",
            format::bytes_in(args.compare.min_delta, args.units)
        );
        return Ok(());
    }

    // Directories say where, files say what. Kept apart because a directory's
    // growth is the sum of its children's, so one list would show the same
    // bytes several times over.
    for (heading, want_dir) in [("DIRECTORIES", true), ("FILES", false)] {
        let rows: Vec<&compare::Change> = report
            .changes
            .iter()
            .filter(|c| c.is_dir == want_dir)
            .take(args.top)
            .collect();
        if rows.is_empty() {
            continue;
        }
        sayln!("\n  {heading}");
        for change in rows {
            let sign = if change.delta() >= 0 { "+" } else { "-" };
            sayln!(
                "  {sign}{:>9}  {:<8}  {}",
                format::bytes_in(change.magnitude(), args.units),
                match change.kind {
                    compare::Kind::Grew => "grew",
                    compare::Kind::Shrank => "shrank",
                    compare::Kind::Added => "new",
                    compare::Kind::Removed => "gone",
                },
                // The baseline is a file somebody else's tool may have
                // written, and a path in it can carry anything a filename can.
                format::printable(&change.path),
            );
        }
    }

    let sign = if report.net >= 0 { "+" } else { "-" };
    sayln!(
        "\n  Net {sign}{} across files",
        format::bytes_in(report.net.unsigned_abs(), args.units)
    );
    if report.below_floor > 0 {
        sayln!(
            "  {} below {} not shown",
            format::plural(report.below_floor, "change"),
            format::bytes_in(args.compare.min_delta, args.units),
        );
    }
    Ok(())
}

/// Write the export, to a file or to standard output.
///
/// With `--query` it exports exactly the rows that matched, so "show me this"
/// and "give me this as a file" ask the same question. Without one it exports
/// the whole tree.
///
/// The report goes to stderr, always. When the document is going to stdout,
/// anything else on that stream would end up inside the file the user is
/// redirecting into — and a CSV with a summary line glued to the end of it is
/// a CSV that fails to parse somewhere else, later, for reasons nobody can see.
fn run_export(
    tree: &SizeTree,
    options: &export::Options,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = match &args.query {
        Some(source) => {
            let query = kosha_query::Query::parse(source)?;
            // No cap: `--top` is about what fits on a screen, and an export
            // truncated to fifteen rows would be a quietly wrong file.
            Some(query.run(tree, format::now_unix(), usize::MAX).matches)
        }
        None => None,
    };

    let written = match &args.out {
        // Through `to_path`, which writes beside the destination and renames:
        // an export that did not finish must not replace the baseline that was
        // there with a shorter one that still parses.
        Some(path) => export::to_path(tree, rows.as_deref(), options, path)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?,
        None => {
            let stdout = std::io::stdout();
            let out = std::io::BufWriter::new(stdout.lock());
            match &rows {
                Some(rows) => export::write_rows(tree, rows, options, out)?,
                None => export::write_tree(tree, options, out)?,
            }
        }
    };

    let where_to = args.out.as_ref().map_or_else(
        || "standard output".to_string(),
        |p| p.display().to_string(),
    );
    eprintln!(
        "exported {} to {where_to}",
        format::plural(written.rows, "entry"),
    );
    if written.lossy > 0 {
        eprintln!(
            "  {} had names that are not valid text; those rows are marked lossy",
            format::plural(written.lossy, "entry")
        );
    }
    Ok(())
}

fn print_header(tree: &SizeTree, args: &Args) {
    let stats = tree.stats();
    let approx = if tree.is_partial() { "~" } else { "" };

    // Sanitised like every name under it, and for a reason the other lines do
    // not have: when the tree came out of a snapshot this root came out of that
    // file's header, so it is not necessarily a path this person typed.
    sayln!(
        "\n{}",
        format::printable(&tree.root_path().to_string_lossy())
    );
    sayln!(
        "{approx}{} on disk · {approx}{} apparent · {} in {}",
        format::bytes_in(tree.total_allocated(), args.units),
        format::bytes_in(tree.total_logical(), args.units),
        format::plural(stats.entries as usize, "entry"),
        format::duration(stats.duration),
    );
    if stats.cancelled {
        sayln!("cancelled — the numbers above cover only what was scanned");
    }
    if stats.unreadable > 0 {
        sayln!(
            "{} could not be read, so totals are a lower bound \u{2014} \
             `--query unreadable` lists them",
            format::plural(stats.unreadable as usize, "entry")
        );
    }
    if stats.depth_limited {
        // The other way to be a floor, and the one somebody asked for — which is
        // no reason not to say it. `--depth 1` reported a twentieth of a tree
        // with no mark at all, and `du --max-depth` counts everything and shows
        // one level, so the number people expect here is the whole tree's.
        sayln!(
            "stopped at depth {}, so totals cover only what is above it",
            stats.max_depth
        );
    }
    // Said only when it is news. The walk is what everybody gets, and a line
    // repeating that on every run is a line nobody reads — which is exactly
    // how the line that matters would get missed.
    if stats.strategy != kosha_core::Strategy::DirectoryWalk || stats.declined.is_some() {
        sayln!("read from the {}", stats.provenance_in_a_sentence());
    }
    sayln!();
}

fn print_largest(tree: &SizeTree, args: &Args) {
    let children = tree.children_by_size(tree.root());
    if children.is_empty() {
        return;
    }
    let total = tree.total_allocated();
    let width = children
        .iter()
        .take(args.top)
        .map(|&id| format::printable(&tree.name_lossy(id)).chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 48);

    for &id in children.iter().take(args.top) {
        let node = tree.node(id);
        // `printable`, because a filename may legally contain a newline on
        // Unix and printing one raw splits this row in half — every column
        // after it then lands somewhere the header does not describe.
        let name = format::printable(&tree.name_lossy(id)).into_owned();
        let marker = if node.is_dir() { "/" } else { " " };
        sayln!(
            "  {:>10}  {:>5}  {}  {:<width$}{}",
            format::bytes_in(node.allocated, args.units),
            format::percent(node.allocated, total),
            bar(node.allocated, total),
            truncate(&name, width),
            marker,
        );
    }

    if children.len() > args.top {
        sayln!("  … and {} more", children.len() - args.top);
    }
}

/// A twelve-cell proportion bar. Enough to see shape, small enough to fit.
fn bar(part: u64, whole: u64) -> String {
    const CELLS: usize = 12;
    if whole == 0 {
        return " ".repeat(CELLS);
    }
    let filled = ((part as f64 / whole as f64) * CELLS as f64).round() as usize;
    let filled = filled.min(CELLS);
    format!("{}{}", "█".repeat(filled), "·".repeat(CELLS - filled))
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    format!("{}…", s.chars().take(keep).collect::<String>())
}

fn print_stats(tree: &SizeTree) {
    let stats = tree.stats();
    let per_sec = if stats.duration.as_secs_f64() > 0.0 {
        stats.entries as f64 / stats.duration.as_secs_f64()
    } else {
        0.0
    };

    sayln!("\nSCAN");
    sayln!("  directories        {:>14}", format::count(stats.dirs));
    sayln!("  files              {:>14}", format::count(stats.files));
    sayln!("  max depth          {:>14}", stats.max_depth);
    sayln!(
        "  unreadable         {:>14}",
        format::count(stats.unreadable)
    );
    sayln!(
        "  other devices      {:>14}",
        format::count(stats.skipped_other_device)
    );
    sayln!(
        "  hardlinks deduped  {:>14}",
        format::count(stats.hardlinks_deduped)
    );
    sayln!(
        "  elapsed            {:>14}",
        format::duration(stats.duration)
    );
    sayln!("  entries/sec        {:>14}", format::count(per_sec as u64));
    sayln!(
        "  tree memory        {:>14}  ({} bytes/entry)",
        format::bytes(tree.memory_bytes() as u64),
        tree.memory_bytes() / tree.len().max(1),
    );
}

/// Who is using the disk.
///
/// Sorted by what each account occupies, largest first, with the operating
/// system's own accounts set apart at the bottom rather than dropped. On a
/// Windows volume `TrustedInstaller` and `SYSTEM` are usually the first two
/// rows by a wide margin, and a table that buried the people under them would
/// answer a question nobody asked.
fn print_owners(tree: &SizeTree, args: &Args) {
    let shares = kosha_core::owner::shares(tree);
    if shares.is_empty() {
        // Either the scan did not collect owners — in which case `--owners`
        // switched it on and something went wrong — or the tree is empty.
        sayln!("\nOWNERS\n  nothing was attributed");
        return;
    }

    let total = tree.total_allocated();
    let width = shares
        .iter()
        .map(|share| share.label().chars().count())
        .max()
        .unwrap_or(8)
        // Wide enough for a whole SID. An account name fits in far less, but an
        // account the machine cannot name is shown *as* its SID, and two SIDs
        // from one machine differ only in the last few digits — truncated to
        // forty characters, `…-329466538-1001` and `…-329466538-1005` are the
        // same string on the screen, which is the one thing showing a SID at
        // all is meant to avoid.
        .clamp(8, 64);

    sayln!("\nOWNERS");
    let (people, machine): (Vec<_>, Vec<_>) = shares.into_iter().partition(|share| {
        share
            .owner
            .as_ref()
            .is_none_or(|owner| owner.kind.is_somebody())
    });

    let row = |share: &kosha_core::Share| {
        // An account this machine could not name is shown by its identifier and
        // said to be unknown, never folded into anything: on a shared machine
        // the unrecognised account is often the answer.
        let unnamed = if share.owner.as_ref().is_some_and(|owner| !owner.is_named()) {
            "  (this machine does not know this account)"
        } else {
            ""
        };
        // Directories as well as files. An account can own a directory and
        // nothing in it — two service accounts on this machine own exactly
        // that — and a row reading `0 B` and `0 files` with no third number
        // looks like a fault rather than like the truth.
        //
        // The counts are formatted with their nouns and then aligned as one
        // field, rather than aligned as a number with a fixed word after it:
        // that keeps the columns straight *and* spells `1 directory`.
        sayln!(
            "  {:>10}  {:>5}  {}  {:<width$}  {:>16}  {:>19}{}",
            format::bytes_in(share.allocated, args.units),
            format::percent(share.allocated, total),
            bar(share.allocated, total),
            truncate(&share.label(), width),
            format::plural(share.files as usize, "file"),
            format::plural(share.dirs as usize, "directory"),
            unnamed,
        );
    };

    for share in &people {
        row(share);
    }
    if !machine.is_empty() {
        sayln!("  — the operating system's own —");
        for share in &machine {
            row(share);
        }
    }
}

fn print_insights(tree: &SizeTree, args: &Args) {
    let context = Context {
        units: args.units,
        ..Context::detect()
    };
    // Loaded here rather than inside `findings` so that a rule which did not
    // load can be *said*. A pack author looking at a file they wrote and no
    // finding from it has nothing to go on otherwise.
    let packs = kosha_marketplace::load_analysis_rules();
    for problem in packs.problems() {
        eprintln!("rule pack: {problem}");
    }

    // Not `analyze`: that is the pure rule engine, and some findings have to
    // read the disk or come from the user's own rules. The window shows the
    // same set.
    let insights = kosha_analyze::findings_with(tree, &context, &packs.rules);
    sayln!("\nINSIGHTS");
    if insights.is_empty() {
        sayln!("  nothing worth reporting");
        return;
    }
    for insight in &insights {
        let amount = if insight.reclaimable > 0 {
            format::bytes_in(insight.reclaimable, args.units)
        } else {
            "—".into()
        };
        sayln!("\n  [{:>10}]  {}", amount, insight.title);
        sayln!(
            "               {:?} · {} · {}",
            insight.confidence,
            insight.action.label(),
            format::plural(insight.len(), &insight.subject),
        );
        sayln!("               {}", insight.detail);
        if insight.superseded > 0 {
            sayln!(
                "               ({} counted in a finding above)",
                format::count(insight.superseded as u64),
            );
        }
        for &node in insight.nodes.iter().take(5) {
            sayln!(
                "               · {}",
                format::printable(&tree.path_of(node).to_string_lossy())
            );
        }
        if insight.nodes.len() > 5 {
            sayln!("               · … and {} more", insight.nodes.len() - 5);
        }
    }
    // `0 B reclaimable` under a list of findings with large figures beside
    // them reads as a contradiction rather than a total. It is what a scan of
    // a disk image says, where every finding is for looking at, so it says
    // that instead.
    match kosha_analyze::total_reclaimable(&insights) {
        0 => sayln!("\n  Nothing here can be reclaimed by acting; the findings above are to read"),
        total => sayln!(
            "\n  {} reclaimable across confident findings",
            format::bytes_in(total, args.units)
        ),
    }
}

fn print_duplicates(tree: &SizeTree, args: &Args) {
    // **A hunt reads bytes, and the bytes here are inside a file.** The tree
    // came out of a disk image, so every path in it names something that does
    // not exist to this system — `dupes.iso\a\twin.bin` is not a file, it is
    // part of one. The hunt opened each of them, failed, and said so in the
    // one shape that looks like an answer:
    //
    //     examined 2 candidates, read 0 B
    //     2 files could not be read and were excluded
    //     no identical files found
    //
    // Measured on an image holding one identical pair and nothing else. The
    // last line is the false one, and it is the line a person reads.
    //
    // Refused rather than repaired, because repairing it means reading file
    // contents back out of every image format kosha opens, and the readers
    // map structure rather than data. The same refusal the other front end
    // has always given a snapshot, and for a reason of the same shape.
    if tree.stats().strategy == kosha_core::Strategy::DiskImage {
        sayln!("\nDUPLICATES");
        sayln!(
            "  This is a disk image, and a hunt has to read the files themselves \
             — their bytes are inside the image, which kosha reads for its shape \
             rather than its contents"
        );
        return;
    }
    let report = duplicates::find(
        tree,
        &DuplicateOptions {
            min_size: args.dupe_min,
            ..DuplicateOptions::default()
        },
    );
    sayln!("\nDUPLICATES");
    sayln!(
        "  examined {}, read {}",
        format::plural(report.candidates_examined, "candidate"),
        format::bytes_in(report.bytes_hashed, args.units),
    );
    if report.not_local > 0 {
        // Not a failure and not a gap in the results — a deliberate refusal.
        // Reading a cloud placeholder downloads it, so hashing a folder of
        // them would fill the disk this tool was asked to help empty.
        // Phrased so it agrees whether it is one file or many: "1 file ...
        // were skipped" was the first attempt.
        sayln!(
            "  {} stored in the cloud, skipped rather than downloaded",
            format::plural(report.not_local, "file"),
        );
    }
    if report.unreadable > 0 {
        // Excluded from the results, so say so rather than letting the user
        // assume the hunt covered everything it examined.
        sayln!(
            "  {} could not be read and {} excluded",
            format::plural(report.unreadable, "file"),
            if report.unreadable == 1 {
                "was"
            } else {
                "were"
            },
        );
    }
    if report.below_min > 0 {
        // The only exclusion that is a setting rather than a fact about the
        // file, so it names the threshold and the flag that moves it. A folder
        // of identical 200 KB photographs otherwise reports "no identical
        // files found", which is true of what was examined and reads as a
        // statement about the folder.
        sayln!(
            "  {} under {} not considered — lower it with --dupe-min",
            format::plural(report.below_min, "file"),
            format::bytes_in(report.min_size, args.units),
        );
    }
    if report.groups.is_empty() {
        sayln!("  no identical files found");
        return;
    }
    for group in report.groups.iter().take(10) {
        sayln!(
            "\n  {} × {} — {} recoverable",
            group.nodes.len(),
            format::bytes_in(group.size, args.units),
            format::bytes_in(group.reclaimable, args.units),
        );
        for &node in &group.nodes {
            sayln!(
                "    {}",
                format::printable(&tree.path_of(node).to_string_lossy())
            );
        }
    }
    sayln!(
        "\n  {} recoverable in total",
        format::bytes_in(report.total_reclaimable(), args.units)
    );
}

/// Run a query and print what it found, with the time it took.
///
/// The timing is printed because it is the claim: a query is a linear pass over
/// a flat array, and anyone should be able to check that for themselves rather
/// than take the documentation's word for it.
fn print_query(
    tree: &SizeTree,
    source: &str,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let query = kosha_query::Query::parse(source)?;
    let now = format::now_unix();

    // Said before the results rather than after them, because the results are
    // going to be empty and an empty list explains nothing. "0 matched" reads
    // the same whether nobody matched or the scan cannot tell.
    if query.needs_owners() && !tree.owners().were_collected() {
        eprintln!("warning: this query asks about owners; add --owners so the scan records them");
    }

    let started = std::time::Instant::now();
    let results = query.run(tree, now, args.top);
    let elapsed = started.elapsed();

    sayln!(
        "
QUERY  {source}"
    );
    sayln!(
        "  {} matched, {} in total, searched {} in {}",
        format::count(results.total_matched as u64),
        format::bytes_in(results.total_allocated, args.units),
        format::plural(tree.len(), "entry"),
        format::duration(elapsed),
    );
    sayln!();

    for id in &results.matches {
        let node = tree.node(*id);
        sayln!(
            "  {:>10}  {}{}",
            format::bytes_in(node.allocated, args.units),
            format::printable(&tree.path_of(*id).to_string_lossy()),
            if node.is_dir() { "/" } else { "" },
        );
    }
    Ok(())
}

fn query_help() -> String {
    let mut text = String::from(
        "
QUERY LANGUAGE

           Plain text searches names:      node_modules
           Compare a field:                size > 1gb
           Combine with and / or / not:    file and size > 100mb and not path ~ cache
           Group with brackets:            (ext = mp4 or ext = mkv) and age > 1y
           List alternatives:              ext in (mp4, mkv, mov)

         OPERATORS

           >  >=  <  <=  =  !=     compare
           ~   !~                  contains, does not contain
           and  or  not            also spelled &&  ||  !

         UNITS

           Sizes:      100, 10k, 1.5mb, 2gb, 500mib
           Ages:       30d, 6mo, 2y, 12h, 1w  (a bare number means days)
           Dates:      2024-01-01

         FIELDS

",
    );
    for (name, description) in kosha_query::reference() {
        text.push_str(&format!(
            "  {name:<20}  {description}
"
        ));
    }
    text
}

/// What the command line asked for.
///
/// The two help screens are outcomes rather than side effects. `--query-help`
/// used to print and then call `std::process::exit(0)` from inside the parser,
/// which is a fine shortcut right up until something other than `main` wants
/// to know what the arguments said. A test, for one — which is part of why
/// there were none: the test binary exited part-way through its own run, and
/// reported two passes and no failures.
#[derive(Debug)]
enum Parsed {
    View(Vec<String>),
    /// Boxed because `Args` is far larger than the other arms and this is
    /// returned by value.
    Run(Box<Args>),
    Search(Box<search::Args>),
    Marketplace(Vec<String>),
    SearchHelp,
    SearchRecipes(Option<PathBuf>, Option<String>),
    Usage,
    QueryHelp,
    Version,
    /// List the snapshots kept. Needs no path, which is why it is its own arm
    /// rather than a flag on `Args`.
    Snapshots,
    Caches(cache::ListArgs),
}

/// Read the process's own arguments.
fn parse() -> Result<Parsed, String> {
    parse_from(std::env::args().skip(1))
}

/// The argument parser proper, over any sequence of arguments.
///
/// Split from [`parse`] so it can be tested. This is the only front end whose
/// behaviour is decided entirely by a string of text the user typed, and it
/// had no tests at all — a flag could be accepted and then ignored, or two
/// flags could disagree about the same field, and nothing would notice.
fn parse_from<I>(argv: I) -> Result<Parsed, String>
where
    I: IntoIterator<Item = String>,
{
    let original: Vec<_> = argv.into_iter().collect();
    let mut argv = original.clone().into_iter().peekable();
    if argv.peek().is_some_and(|arg| arg == "view") {
        argv.next();
        return Ok(Parsed::View(argv.collect()));
    }
    if argv.peek().is_some_and(|arg| arg == "marketplace") {
        argv.next();
        return Ok(Parsed::Marketplace(argv.collect()));
    }
    if argv.peek().is_some_and(|arg| arg == "search") {
        argv.next();
        return match search::parse(argv)? {
            search::Parsed::Run(args) => Ok(Parsed::Search(args)),
            search::Parsed::Help => Ok(Parsed::SearchHelp),
            search::Parsed::ListRecipes(path, filter) => Ok(Parsed::SearchRecipes(path, filter)),
        };
    }
    let mut path = None;
    let mut args = Args {
        path: PathBuf::new(),
        query: None,
        top: 15,
        stats: false,
        insights: false,
        cleanup_guide: false,
        owners: false,
        duplicates: false,
        dupe_min: DuplicateOptions::default().min_size,
        export: None,
        out: None,
        since: None,
        snapshot: false,
        label: String::new(),
        compare: compare::Options::default(),
        scan: ScanOptions::default(),
        units: format::Units::Binary,
        cache: cache::Cache::default(),
        absolute_paths: false,
    };

    let mut iter = argv;
    let mut positional = false;
    let mut list_caches = false;
    while let Some(arg) = iter.next() {
        if !positional && arg == "--" {
            positional = true;
            continue;
        }
        if positional {
            if path.replace(PathBuf::from(arg)).is_some() {
                return Err("only one path may be given".into());
            }
            continue;
        }
        // The one place a numeric flag is read, and therefore the one place its
        // refusal is worded. "needs a number" was said about `--top 4294967296`,
        // which is a number — the complaint was about its size and named the
        // wrong thing, and it did not echo what had been read either, so a typo
        // in a long value was invisible. Digits alone mean too large; anything
        // else means not a number.
        let mut value = |name: &str| {
            iter.next()
                .ok_or_else(|| format!("{name} needs a value"))
                .and_then(|v| match v.parse::<u32>() {
                    Ok(n) => Ok(n),
                    Err(_) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => {
                        Err(format!(
                            "{name} takes a number up to {}, and `{v}` is larger",
                            u32::MAX
                        ))
                    }
                    Err(_) => Err(format!("{name} needs a number, and `{v}` is not one")),
                })
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Usage),
            "--query-help" => return Ok(Parsed::QueryHelp),
            // `-V` rather than `-v`, which every other tool reserves for
            // verbosity and which somebody will eventually want here.
            "-V" | "--version" => return Ok(Parsed::Version),
            "--stats" => args.stats = true,
            "--insights" => args.insights = true,
            "--cleanup-guide" => args.cleanup_guide = true,
            "--owners" => {
                args.owners = true;
                args.scan.collect_owners = true;
            }
            "--duplicates" => args.duplicates = true,
            "--dupe-min" => {
                args.dupe_min = u64::from(value("--dupe-min")?);
                // Setting the smallest file the hunt looks at is asking for the
                // hunt, the same way `--label` is asking for a snapshot. Before
                // this it set a threshold for something that was not going to
                // happen and said nothing about it.
                args.duplicates = true;
            }
            "--cross-fs" => args.scan.cross_filesystems = true,
            "--exact" => args.scan.exact_allocated = true,
            "--mft" => args.scan.use_mft = true,
            "--image" => args.scan.use_image = true,
            "--hardlinks" => args.scan.dedup_hardlinks = Some(true),
            "--no-hardlinks" => args.scan.dedup_hardlinks = Some(false),
            "--decimal" => args.units = format::Units::Decimal,
            "--top" => args.top = value("--top")? as usize,
            "--query" => {
                args.query = Some(
                    iter.next()
                        .ok_or("--query needs a query, e.g. --query \"size > 1gb\"")?,
                );
            }
            "--depth" => args.scan.max_depth = Some(value("--depth")?.min(255) as u8),
            "--cache" => args.cache.enabled = true,
            "--caches" => list_caches = true,
            "--absolute-paths" => args.absolute_paths = true,
            "--cache-dir" => {
                args.cache.dir = Some(
                    iter.next()
                        .filter(|s| !s.starts_with("--"))
                        .ok_or("--cache-dir needs a directory")?
                        .into(),
                );
            }
            "--export" => {
                let word = iter
                    .next()
                    .ok_or("--export needs a format: txt, csv, jsonl, or json")?;
                let format = export::Format::parse(&word).ok_or_else(|| {
                    format!("unknown export format `{word}` — txt, csv, jsonl, or json")
                })?;
                args.export = Some(export::Options {
                    format,
                    ..args.export.unwrap_or_default()
                });
            }
            "--since" => {
                args.since = Some(PathBuf::from(
                    iter.next()
                        .ok_or("--since needs a snapshot or a CSV export to compare against")?,
                ));
            }
            "--snapshot" => args.snapshot = true,
            "--label" => {
                args.label = iter.next().ok_or("--label needs a name")?;
                // Naming one is asking for one. The alternative is a flag that
                // parses, changes nothing, and says nothing about why.
                args.snapshot = true;
            }
            "--snapshots" => return Ok(Parsed::Snapshots),
            "--since-min" => args.compare.min_delta = u64::from(value("--since-min")?),
            "--out" => {
                args.out = Some(PathBuf::from(
                    iter.next().ok_or("--out needs a file to write to")?,
                ));
            }
            "--export-min" => {
                let min = value("--export-min")?;
                args.export = Some(export::Options {
                    min_size: u64::from(min),
                    ..args.export.unwrap_or_default()
                });
            }
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => {
                if path.is_some() {
                    return Err("only one path may be given".into());
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    if list_caches {
        return cache::parse_list(original.into_iter().filter(|arg| arg != "--caches"))
            .map(Parsed::Caches);
    }
    args.path = path.ok_or("a path is required — a directory, or a snapshot to browse")?;
    args.cache.validate()?;
    if args.absolute_paths {
        args.export
            .as_mut()
            .ok_or("--absolute-paths requires --export")?
            .absolute_paths = true;
    }
    if args.cache.enabled {
        if let Some(flag) = args.scan_only_flag() {
            return Err(format!(
                "{flag} changes a fresh scan and cannot be combined with --cache"
            ));
        }
        if args.snapshot || args.duplicates {
            return Err("--cache cannot be combined with --snapshot/--label or duplicate hunts; cached data is historical".into());
        }
    }

    // **A flag taken and ignored is worse than one refused**, which is this
    // file's own rule — it is why `--label` turns the snapshot on and why
    // `--export-min` turns the export on. Two flags cannot do that, because
    // neither names a thing to do:
    //
    // * `--out` is a destination and there are two formats, so implying one
    //   would be a guess. `kosha here --out report.csv` wrote no file at all and
    //   exited 0, which is the shape of a backup script that has been silently
    //   doing nothing.
    // * `--since-min` is a threshold for a comparison whose *other* half is a
    //   file only the user can name.
    //
    // Checked after the loop so the order they are given in does not matter.
    if args.out.is_some() && args.export.is_none() {
        return Err(
            "--out names a file to write; add --export csv or --export json to say \
             what to write into it"
                .into(),
        );
    }
    if args.compare.min_delta != compare::Options::default().min_delta && args.since.is_none() {
        return Err("--since-min sets a threshold for --since, which was not given".into());
    }

    Ok(Parsed::Run(Box::new(args)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Parsed, String> {
        parse_from(line.split_whitespace().map(str::to_string))
    }

    /// Parses, and asked to run rather than for a help screen.
    fn args(line: &str) -> Args {
        match parse(line) {
            Ok(Parsed::Run(args)) => *args,
            Ok(other) => panic!("`{line}` asked for {other:?}"),
            Err(e) => panic!("`{line}` did not parse: {e}"),
        }
    }

    fn error(line: &str) -> String {
        match parse(line) {
            Err(e) => e,
            Ok(_) => panic!("`{line}` parsed, but should not have"),
        }
    }

    #[test]
    fn cleanup_guide_operates_on_records_without_changing_scan_or_export_policy() {
        let parsed = args(". --cleanup-guide");
        assert!(parsed.cleanup_guide);
        assert!(parsed.scan.max_depth.is_none());
        assert!(parsed.scan_only_flag().is_none());
        assert!(!parsed.document_on_stdout());
        let parsed = args(". --cleanup-guide --export json");
        assert!(parsed.document_on_stdout());
    }

    // ------------------------------------------------------ the help screen

    /// Every long option the help screen lists must be one the parser accepts.
    ///
    /// The help text is the only place these are written down, so a flag
    /// printed there and rejected here is a promise the program breaks the
    /// moment somebody follows its own instructions. Read from `HELP` rather
    /// than restated, so adding a line to `USAGE` starts checking it.
    #[test]
    fn every_option_the_help_screen_lists_is_accepted() {
        // `--flag` at the start of a help line, which is how they are laid out.
        let listed: Vec<&str> = USAGE
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("--"))
            .filter_map(|rest| rest.split_whitespace().next())
            .collect();
        assert!(
            listed.len() >= 10,
            "the help screen changed shape: {listed:?}"
        );

        for flag in listed {
            let line = format!("--{flag} 1 .");
            let outcome = parse(&line);
            let refused_as_unknown = matches!(&outcome, Err(e) if e.contains("unknown option"));
            assert!(
                !refused_as_unknown,
                "`--{flag}` is on the help screen and the parser rejects it"
            );
        }
    }

    /// Asking for the report has to switch the collection on. There is nothing
    /// to report afterwards about something the scan did not record as it went,
    /// and a `--owners` that printed an empty table would look like a machine
    /// with no accounts on it.
    #[test]
    fn asking_for_owners_makes_the_scan_collect_them() {
        let asked = args(". --owners");
        assert!(asked.owners);
        assert!(asked.scan.collect_owners);

        let plain = args(".");
        assert!(!plain.owners);
        assert!(!plain.scan.collect_owners);
    }

    /// A flag that sets up a thing turns that thing on, and one that cannot is
    /// refused. Neither is silently kept and never used.
    ///
    /// Found by running the binary with each modifier and nothing else:
    /// `kosha here --out report.csv` exited 0, printed a scan, and wrote no file,
    /// which is what a backup script that has quietly been doing nothing looks
    /// like. `--dupe-min` set a threshold for a hunt that was not going to
    /// happen. `--label` and `--export-min` already did the right thing, and the
    /// comment above the first of them is where the rule is written down.
    #[test]
    fn a_modifier_either_asks_for_its_mode_or_is_refused() {
        // Asking for it, because there is only one thing it could mean.
        assert!(args(". --label before-cleanup").snapshot);
        assert!(args(". --export-min 1024").export.is_some());
        assert!(args(". --dupe-min 5").duplicates);

        // Refused, because neither names a thing to do. `--out` has two formats
        // to choose between and `--since-min` needs a file only the user knows.
        let out = error(". --out report.csv");
        assert!(out.contains("--export"), "{out}");
        let since = error(". --since-min 4096");
        assert!(since.contains("--since"), "{since}");

        // And they are accepted in either order once the other half is there.
        assert_eq!(
            args(". --out report.csv --export csv").out,
            Some(PathBuf::from("report.csv"))
        );
        assert_eq!(
            args(". --export csv --out report.csv").out,
            Some(PathBuf::from("report.csv"))
        );
        assert!(args(". --since-min 4096 --since old.kosha").since.is_some());
    }

    /// A refusal has to be about what is actually wrong with what was typed.
    #[test]
    fn a_number_that_is_too_large_is_refused_as_too_large() {
        let big = error(". --top 4294967296");
        assert!(big.contains("larger"), "{big}");
        assert!(big.contains("4294967296"), "the value is not echoed: {big}");

        let word = error(". --top plenty");
        assert!(word.contains("not one"), "{word}");
        assert!(word.contains("plenty"), "the value is not echoed: {word}");

        let missing = error(". --top");
        assert!(missing.contains("needs a value"), "{missing}");

        // And the value at the edge is still a value.
        assert_eq!(args(". --top 4294967295").top, 4_294_967_295);
    }

    #[test]
    fn an_unknown_option_is_refused_rather_than_treated_as_a_path() {
        assert!(error("--wat .").contains("unknown option"));
        // And the refusal names it, so the user can see the typo.
        assert!(error("--dupemin 5 .").contains("--dupemin"));
    }

    // ------------------------------------------------------------- the path

    #[test]
    fn a_path_is_required_and_only_one_is_allowed() {
        assert!(error("--stats").contains("path is required"));
        assert!(error("one two").contains("only one path"));
        assert_eq!(args("somewhere").path, PathBuf::from("somewhere"));
    }

    #[test]
    fn help_is_not_an_error_and_needs_no_path() {
        assert!(matches!(parse("--help"), Ok(Parsed::Usage)));
        assert!(matches!(parse("-h"), Ok(Parsed::Usage)));
        assert!(matches!(parse("--query-help"), Ok(Parsed::QueryHelp)));
    }

    /// Asked for before anything else works, and answerable without a path:
    /// somebody establishing which build they have has not got as far as
    /// choosing a directory.
    #[test]
    fn the_version_needs_no_path_and_is_not_an_error() {
        assert!(matches!(parse("--version"), Ok(Parsed::Version)));
        assert!(matches!(parse("-V"), Ok(Parsed::Version)));
    }

    /// `-v` is verbosity everywhere else, and quietly making it mean version
    /// would be a trap for the first person who wants a verbose scan.
    #[test]
    fn lowercase_v_is_left_free_for_verbosity() {
        assert!(error(". -v").contains("unknown option"));
    }

    // ------------------------------------------------------- scan options

    /// The pair added when hardlink accounting started working on Windows.
    /// Whichever comes last wins, which is what a reader expects and what
    /// makes them usable from a script that appends a flag.
    #[test]
    fn the_hardlink_flags_are_a_pair_and_the_last_one_wins() {
        assert_eq!(args(". --hardlinks").scan.dedup_hardlinks, Some(true));
        assert_eq!(args(". --no-hardlinks").scan.dedup_hardlinks, Some(false));
        assert_eq!(
            args(". --hardlinks --no-hardlinks").scan.dedup_hardlinks,
            Some(false)
        );
        assert_eq!(
            args(". --no-hardlinks --hardlinks").scan.dedup_hardlinks,
            Some(true)
        );
        // And with neither flag the scan has not been told: what happens is
        // then the strategy's business, and the same disk image answers the
        // same on both platforms.
        assert_eq!(args(".").scan.dedup_hardlinks, None);
    }

    #[test]
    fn the_scan_flags_reach_the_scanner() {
        assert!(args(". --cross-fs").scan.cross_filesystems);
        assert!(args(". --exact").scan.exact_allocated);
        assert_eq!(args(". --depth 3").scan.max_depth, Some(3));
        // A depth past what the arena can hold is clamped, not rejected: the
        // walk cannot go deeper than 255 anyway.
        assert_eq!(args(". --depth 9999").scan.max_depth, Some(255));
        assert_eq!(args(".").scan.max_depth, None);
    }

    // ------------------------------------------------------------ numbers

    #[test]
    fn a_number_flag_without_a_number_says_which_one() {
        assert!(error(". --top").contains("--top"));
        assert!(error(". --top nine").contains("number"));
        assert!(error(". --dupe-min").contains("--dupe-min"));
    }

    #[test]
    fn the_duplicate_floor_can_be_lowered() {
        assert_eq!(args(".").dupe_min, DuplicateOptions::default().min_size);
        assert_eq!(args(". --dupe-min 100").dupe_min, 100);
        assert_eq!(args(". --dupe-min 0").dupe_min, 0);
    }

    // ------------------------------------------------------------- export

    #[test]
    fn an_export_format_is_named_and_checked() {
        assert_eq!(
            args(". --export csv").export.unwrap().format,
            export::Format::Csv
        );
        assert_eq!(
            args(". --export json").export.unwrap().format,
            export::Format::Json
        );
        assert!(error(". --export yaml").contains("yaml"));
        assert!(args(".").export.is_none());
    }

    /// `--export-min` before `--export` must not be thrown away by the later
    /// flag, and neither ordering may lose the other's setting.
    #[test]
    fn the_export_flags_combine_in_either_order() {
        for line in [
            ". --export csv --export-min 4096",
            ". --export-min 4096 --export csv",
        ] {
            let export = args(line).export.expect("an export was asked for");
            assert_eq!(export.format, export::Format::Csv, "{line}");
            assert_eq!(export.min_size, 4096, "{line}");
        }
    }

    #[test]
    fn units_switch_between_powers_of_two_and_ten() {
        assert_eq!(args(".").units, format::Units::Binary);
        assert_eq!(args(". --decimal").units, format::Units::Decimal);
    }
}
