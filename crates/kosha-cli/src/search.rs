// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Explicit natural-language search. Describe resolves intent before any source
//! is opened; execution uses that exact compiled query and captured clock.

use kosha_core::{ScanOptions, Scanner, export, format, sayln, snapshot};
use kosha_query::{Expr, Field, Op, Value};
use kosha_search::{RecipeBook, SearchContext, SearchIndex, SearchMode, Translation};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

pub const USAGE: &str = "\
kosha search — natural-language search over an explicit source

USAGE:
    kosha search --describe \"PHRASE\"
    kosha search --load SAVED.kosha \"PHRASE\" [OPTIONS]
    kosha search --path DIRECTORY \"PHRASE\" [OPTIONS]

    --describe       Show the canonical DSL, entities, and date boundaries;
                     opens no source and scans no path
    --load FILE      Search a snapshot path or exact saved NAME.kosha; local
                     paths take precedence; never rescans its original root
    --path DIRECTORY Scan this directory once, then search the resulting tree
    --cache          With --path, read its last completed GUI record instead;
                     missing/corrupt records error without a fallback scan
    --cache-dir DIR  Read cache records here instead; requires --cache
    --recipes FILE   Load custom phrase-to-DSL translations from this JSON file
    --json           With --describe, emit a machine-readable translation object
    --list-recipes   List built-in recipes for this platform without opening a source
                     Add --recipes FILE and optional --filter TEXT to inspect a pack
    --top N          Show the N largest matches (default 15); totals are complete
    --stats          Print scan and search timing/memory statistics
    --decimal        Display decimal GB instead of binary GiB
    --export FORMAT  Export all matches as txt, csv, jsonl, or json (never --top)
    --absolute-paths Use full paths in CSV/JSON/JSONL; requires --export
    --out FILE       Write the export here instead of standard output
    -h, --help       Show this message

Give exactly one quoted phrase and one execution source. Use --describe to
inspect a phrase without a source. Natural size defaults to bytes on disk;
use apparent size for logical file length. Searching changes no files;
--export and --out explicitly write exports. Unknown creation dates are never
replaced by modification dates.

AUTOMATION / LLM WORKFLOW:
    1. Learn the language with --describe --json before opening any source.
    2. Use --path for a fresh full-depth scan; it never consults or updates a
       cache. Use --load for a specific historical snapshot, or --path with
       --cache to explicitly read the GUI cache for that exact root. Cache
       misses fail; they never trigger a scan. Cache age goes to stderr.
    3. Use --export json for root/completeness metadata and entries, or jsonl
       for streaming rows. Use --absolute-paths when acting on paths. TXT is
       always one full path per line. CSV/JSON/JSONL default to relative paths.
       Exports contain ALL matches; --top affects only the human report.
    4. Read stderr for partial/incomplete data and unknown date warnings.
       Exit 0 includes zero matches; exit 1 indicates argument/source/I/O or
       cancellation errors. Searching itself never deletes or modifies files.

Custom recipes default to search-recipes.json in the shared configuration
directory when present; --recipes selects another file. A JSON array maps
exact phrases to DSL: [{\"phrase\":\"my builds\",\"dsl\":\"file and ext in (obj,pdb)\"}].
Phrase matching ignores case and repeated whitespace. Invalid recipe files
fail before source I/O. Built-in natural language handles unmatched phrases.
Versioned recipe packs wrap the same entries in a recipes array with metadata.
Dates use the captured local clock; unknown timestamps remain unknown even
under negation. Inspect --describe output before relying on a date translation.

EXAMPLES:
    kosha search --describe --json \"files modified more than 30 days ago\"
    kosha search --path . --export jsonl --absolute-paths \"documents\"
    kosha search --path C:\\ --cache --export txt \"files over 1 GB\"
    kosha search --load saved.kosha --export csv --out results.csv \"images\"
    kosha search --recipes recipes.json --path . \"my builds\"
";

#[derive(Debug)]
enum Source {
    Snapshot(PathBuf),
    Path(PathBuf),
}
#[derive(Debug)]
pub struct Args {
    phrase: String,
    describe: bool,
    source: Option<Source>,
    top: usize,
    stats: bool,
    units: format::Units,
    export: Option<export::Format>,
    out: Option<PathBuf>,
    cache: super::cache::Cache,
    json: bool,
    recipes: Option<PathBuf>,
    absolute_paths: bool,
}
#[derive(Debug)]
pub enum Parsed {
    Run(Box<Args>),
    Help,
    ListRecipes(Option<PathBuf>, Option<String>),
}

pub fn parse(argv: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = Args {
        phrase: String::new(),
        describe: false,
        source: None,
        top: 15,
        stats: false,
        units: format::Units::Binary,
        export: None,
        out: None,
        cache: super::cache::Cache::default(),
        json: false,
        recipes: None,
        absolute_paths: false,
    };
    let mut iter = argv.into_iter().peekable();
    if iter.peek().is_some_and(|arg| arg == "--list-recipes") {
        iter.next();
        let Some(flag) = iter.next() else {
            return Ok(Parsed::ListRecipes(None, None));
        };
        if flag != "--recipes" {
            return Err(
                "--list-recipes accepts only --recipes FILE and optional --filter TEXT".into(),
            );
        }
        let file = option_value(&mut iter, "--recipes")?.into();
        let filter = match iter.next().as_deref() {
            None => None,
            Some("--filter") => Some(option_value(&mut iter, "--filter")?),
            _ => return Err("expected --filter TEXT after the recipe file".into()),
        };
        if iter.next().is_some() {
            return Err("unexpected argument after --filter TEXT".into());
        }
        return Ok(Parsed::ListRecipes(Some(file), filter));
    }
    let mut phrase = None;
    let mut seen = HashSet::new();
    let mut positional = false;
    while let Some(arg) = iter.next() {
        if !positional && arg == "--" {
            positional = true;
            continue;
        }
        if !positional && matches!(arg.as_str(), "--help" | "-h") {
            return Ok(Parsed::Help);
        }
        if !positional && arg.starts_with('-') {
            if !seen.insert(arg.clone()) {
                return Err(format!("search option `{arg}` was given more than once"));
            }
            match arg.as_str() {
                "--describe"=>args.describe=true,
                "--stats"=>args.stats=true,
                "--decimal"=>args.units=format::Units::Decimal,
                "--cache"=>args.cache.enabled=true,
                "--cache-dir"=>args.cache.dir=Some(option_value(&mut iter,"--cache-dir")?.into()),
                "--recipes"=>args.recipes=Some(option_value(&mut iter,"--recipes")?.into()),
                "--json"=>args.json=true,
                "--absolute-paths"=>args.absolute_paths=true,
                "--load"|"--path"=>{
                    if args.source.is_some(){return Err("choose exactly one search source: --load FILE or --path DIRECTORY".into());}
                    let value=option_value(&mut iter,&arg)?;
                    args.source=Some(if arg=="--load"{Source::Snapshot(value.into())}else{Source::Path(value.into())});
                }
                "--top"=>{
                    let value=option_value(&mut iter,"--top")?;
                    args.top=value.parse::<u32>().map_err(|_|format!("--top needs a whole number up to {}; got `{value}`",u32::MAX))? as usize;
                }
                "--export"=>{
                    let value=option_value(&mut iter,"--export")?;
                    args.export=Some(export::Format::parse(&value).ok_or_else(||format!("unknown export format `{value}` — txt, csv, jsonl, or json"))?);
                }
                "--out"=>args.out=Some(option_value(&mut iter,"--out")?.into()),
                "--query"=>return Err("search takes one natural-language phrase; use kosha PATH --query Q for explicit DSL".into()),
                _=>return Err(format!("unknown search option `{arg}`; see kosha search --help")),
            }
        } else if phrase.replace(arg).is_some() {
            return Err("search accepts exactly one phrase; quote the complete phrase".into());
        }
    }
    args.phrase = phrase
        .filter(|p| !p.trim().is_empty())
        .ok_or("search needs a nonempty quoted phrase")?;
    args.cache.validate()?;
    if args.cache.enabled && !matches!(args.source, Some(Source::Path(_))) {
        return Err(
            "--cache requires --path DIRECTORY and cannot be combined with --load or --describe"
                .into(),
        );
    }
    if args.json && !args.describe {
        return Err(
            "--json requires --describe; use --export json or jsonl for search results".into(),
        );
    }
    if args.absolute_paths && args.export.is_none() {
        return Err("--absolute-paths requires --export".into());
    }
    if args.describe {
        if args.source.is_some() {
            return Err("--describe does not open a source; omit --load and --path".into());
        }
        if seen.iter().any(|flag| {
            matches!(
                flag.as_str(),
                "--top" | "--stats" | "--decimal" | "--export" | "--out"
            )
        }) {
            return Err(
                "--describe cannot be combined with result, statistics, or export options".into(),
            );
        }
    } else if args.source.is_none() {
        return Err("search requires --load FILE or --path DIRECTORY; use --describe to inspect without a source".into());
    }
    if args.out.is_some() && args.export.is_none() {
        return Err("--out requires --export txt, csv, jsonl, or json".into());
    }
    Ok(Parsed::Run(Box::new(args)))
}

fn option_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    let value = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
    if value.starts_with("--") {
        return Err(format!(
            "{flag} needs a value, not another option `{value}`"
        ));
    }
    Ok(value)
}

fn translate(phrase: &str, now: u32, recipes: &RecipeBook) -> Translation {
    let local_date = format::local_date(now).unwrap_or_else(|| {
        let (y, m, d) = format::civil_from_days(i64::from(now) / 86400);
        [y as i32, m as i32, d as i32]
    });
    recipes.translate_with_resolver(
        phrase,
        SearchContext::new(now, local_date),
        SearchMode::Natural,
        |[y, m, d]| {
            format::local_midnight(i64::from(y), m as u32, d as u32).or_else(|| {
                u64::try_from(format::days_from_civil(i64::from(y), m as u32, d as u32) * 86400)
                    .ok()
            })
        },
    )
}

pub fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let now = format::now_unix();
    let recipes = load_recipes(args.recipes.as_deref())?;
    let translation = translate(&args.phrase, now, &recipes);
    if !translation.is_valid() {
        for diagnostic in &translation.diagnostics {
            eprintln!(
                "  bytes {}..{}: {}\n    {}",
                diagnostic.span.start,
                diagnostic.span.end,
                format::printable(&diagnostic.message),
                format::printable(&diagnostic.hint)
            );
        }
        return Err("the search phrase is incomplete or unsupported; no source was opened".into());
    }
    let query = translation.query.as_ref().expect("validated translation");
    if args.describe {
        if args.json {
            describe_json(&args.phrase, &translation, now);
        } else {
            describe(&args.phrase, &translation, now);
        }
        return Ok(());
    }
    // Refuse bad output destinations after translation, before loading/scanning.
    if let Some(out) = &args.out {
        let parent = out.parent().unwrap_or_else(|| std::path::Path::new(""));
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(format!(
                "could not write {}: parent directory does not exist",
                out.display()
            )
            .into());
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed));
    let document_stdout = args.export.is_some() && args.out.is_none();
    let mut apparent_only = false;
    let mut unknown_sizes = false;
    let tree = match args.source.as_ref().expect("validated source") {
        Source::Snapshot(path) => {
            let path = super::snapshots::resolve(path)?;
            let (meta, tree) = snapshot::store::open(&path)
                .map_err(|e| format!("could not load {}: {e}", path.display()))?;
            if let Some(source) = &meta.import {
                apparent_only = !source.allocated_known;
                unknown_sizes = source.unknown_size > 0;
            }
            super::snapshots::report_source(&path, &meta);
            super::snapshots::report_replaced_names(&tree);
            if !document_stdout {
                super::print_snapshot_header(&meta);
            }
            tree
        }
        Source::Path(path) => {
            if args.cache.enabled {
                let (meta, tree) = args.cache.open(path)?;
                if !document_stdout {
                    super::print_snapshot_header(&meta);
                }
                tree
            } else {
                let tree = Scanner::new(ScanOptions::default())
                    .with_cancel(cancel.clone())
                    .scan(path)?;
                if tree.root_unreadable() {
                    return Err(format!("{} could not be read", path.display()).into());
                }
                tree
            }
        }
    };
    if tree.is_incomplete() {
        eprintln!(
            "warning: this source is incomplete because its scan was stopped or depth-limited; results and allocated bytes are lower bounds"
        );
    } else if tree.is_partial() {
        eprintln!(
            "warning: this source is partial due to inaccessible or unreadable entries; results and allocated bytes are lower bounds"
        );
    }
    let started = Instant::now();
    let index = SearchIndex::new_cancellable(Arc::new(tree), &cancel)
        .ok_or(kosha_core::ScanError::Cancelled)?;
    let index_elapsed = started.elapsed();
    if mentions_created(query.expression()) {
        let coverage = index.creation_coverage();
        if coverage.known == 0 && coverage.total > 0 {
            eprintln!(
                "warning: creation timestamps are unavailable (0 of {} entries recorded); unknown creation dates cannot satisfy creation-date conditions, including under negation. Modification times are never substituted. Use --path DIRECTORY for an explicit fresh scan.",
                coverage.total
            );
        } else if coverage.known < coverage.total {
            eprintln!(
                "warning: creation timestamps are known for {} of {} entries; unknown creation dates cannot satisfy creation-date conditions, including under negation. Modification times are never substituted.",
                coverage.known, coverage.total
            );
        }
    }
    let started = Instant::now();
    let limit = if args.export.is_some() {
        usize::MAX
    } else {
        args.top
    };
    let results = index
        .run(query, now, limit, &cancel)
        .ok_or(kosha_core::ScanError::Cancelled)?;
    let elapsed = started.elapsed();
    let tree = index.tree();
    if !document_stdout {
        sayln!("SEARCH  {}", format::printable(&args.phrase));
        sayln!("  DSL: {}", format::printable(query.source()));
        sayln!(
            "  Source: {} ({})",
            format::printable(&tree.root_path().to_string_lossy()),
            tree.stats().provenance_in_a_sentence()
        );
        sayln!(
            "  {} matched, {} in total; searched {} in {}",
            format::count(results.total_matched as u64),
            format::bytes_in(results.total_allocated, args.units),
            format::plural(tree.len().saturating_sub(1), "entry"),
            format::duration(elapsed)
        );
        for id in results.matches.iter().take(args.top) {
            let node = tree.node(*id);
            sayln!(
                "  {:>10}  {}{}",
                format::bytes_in(node.allocated, args.units),
                format::printable(&tree.path_of(*id).to_string_lossy()),
                if node.is_dir() { "/" } else { "" }
            );
        }
        if results.total_matched > args.top {
            sayln!(
                "  showing {} of {} matches",
                args.top,
                results.total_matched
            );
        }
        if args.stats {
            super::print_stats(tree);
            sayln!(
                "\nSEARCH STATS\n  index build        {}\n  index memory       {}\n  search             {}",
                format::duration(index_elapsed),
                format::bytes(index.memory_bytes() as u64),
                format::duration(elapsed)
            );
        }
    } else if args.stats {
        eprintln!(
            "search stats: {} entries; tree {}; index {} built in {}; search {}",
            tree.len(),
            format::bytes(tree.memory_bytes() as u64),
            format::bytes(index.memory_bytes() as u64),
            format::duration(index_elapsed),
            format::duration(elapsed)
        );
    }
    if let Some(kind) = args.export {
        let options = export::Options {
            format: kind,
            absolute_paths: args.absolute_paths,
            apparent_only,
            unknown_sizes,
            ..Default::default()
        };
        let written = if let Some(path) = &args.out {
            export::to_path(tree, Some(&results.matches), &options, path)?
        } else {
            let stdout = std::io::stdout();
            export::write_rows(
                tree,
                &results.matches,
                &options,
                std::io::BufWriter::new(stdout.lock()),
            )?
        };
        eprintln!("exported {}", format::plural(written.rows, "entry"));
        if written.lossy > 0 {
            eprintln!("warning: {} names were exported lossily", written.lossy);
        }
    }
    Ok(())
}

fn load_recipes(explicit: Option<&std::path::Path>) -> Result<RecipeBook, String> {
    match explicit {
        Some(path) => kosha_marketplace::read_recipe_file(path),
        None => kosha_marketplace::load(),
    }
}

pub fn list_pack(path: &std::path::Path, filter: Option<&str>) -> Result<(), String> {
    let book = load_recipes(Some(path))?;
    if let Some(pack) = book.metadata() {
        sayln!("{} ({}, version {})", pack.title, pack.id, pack.version);
        sayln!("{}", pack.description);
        if !pack.tags.is_empty() {
            sayln!("Tags: {}", pack.tags.join(", "));
        }
    }
    let mut matches = book.matching_recipes(filter.unwrap_or_default()).peekable();
    if matches.peek().is_none() {
        sayln!("No matching recipes for {}.", std::env::consts::OS);
    }
    for recipe in matches {
        sayln!("  {}", recipe.phrase);
    }
    Ok(())
}

/// Stable, versioned envelope for agents inspecting a phrase before scanning.
fn describe_json(phrase: &str, translation: &Translation, now: u32) {
    use kosha_core::json::escaped;
    let mut out = format!(
        "{{\"schema\":\"kosha.search.describe.v1\",\"phrase\":{},\"clock_unix\":{now},\"local_clock\":{},\"dsl\":{},\"default_size\":\"allocated\",\"source_opened\":false,\"entities\":[",
        escaped(phrase),
        escaped(&format::local_timestamp(u64::from(now))),
        escaped(translation.dsl.as_deref().expect("validated translation"))
    );
    for (i, entity) in translation.entities.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"kind\":{},\"start_byte\":{},\"end_byte\":{},\"text\":{},\"value\":{}}}",
            escaped(&format!("{:?}", entity.kind)),
            entity.span.start,
            entity.span.end,
            escaped(&phrase[entity.span.clone()]),
            escaped(&entity.value)
        ));
    }
    out.push_str("]}");
    sayln!("{out}");
}

fn describe(phrase: &str, translation: &Translation, now: u32) {
    sayln!("SEARCH  {}", format::printable(phrase));
    sayln!("Clock: {}", format::local_timestamp(u64::from(now)));
    sayln!(
        "DSL: {}",
        format::printable(translation.dsl.as_deref().expect("validated translation"))
    );
    sayln!("ENTITIES");
    for entity in &translation.entities {
        sayln!(
            "  {:?} [{}..{}] {} -> {}",
            entity.kind,
            entity.span.start,
            entity.span.end,
            format::printable(&phrase[entity.span.clone()]),
            format::printable(&entity.value)
        );
    }
    date_boundaries(
        translation
            .query
            .as_ref()
            .expect("validated translation")
            .expression(),
        false,
    );
}
fn date_boundaries(expr: &Expr, inside_not: bool) {
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => {
            date_boundaries(a, inside_not);
            date_boundaries(b, inside_not)
        }
        Expr::Not(inner) => date_boundaries(inner, true),
        Expr::Compare {
            field: field @ (Field::Created | Field::Modified),
            op,
            value: Value::Number(value),
        } => {
            let op = match op {
                Op::Gt => ">",
                Op::Ge => ">=",
                Op::Lt => "<",
                Op::Le => "<=",
                Op::Eq => "=",
                Op::Ne => "!=",
                _ => return,
            };
            sayln!(
                "  boundary{}: {} {op} {}",
                if inside_not { " (inside NOT)" } else { "" },
                field.name(),
                format::local_timestamp(*value)
            );
        }
        _ => {}
    }
}
fn mentions_created(expr: &Expr) -> bool {
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => mentions_created(a) || mentions_created(b),
        Expr::Not(inner) => mentions_created(inner),
        Expr::Compare { field, .. } | Expr::In { field, .. } => *field == Field::Created,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sources_and_describe_are_explicit_and_mutually_exclusive() {
        for args in [
            vec!["files"],
            vec!["--describe", "--path", "missing", "files"],
            vec!["--load", "a", "--path", "b", "files"],
            vec!["--describe", "--export", "csv", "files"],
            vec!["--path", "a", "--query", "file"],
            vec!["--path", "a", "files", "folders"],
            vec!["--path", "a", "--top", "1", "--top", "2", "files"],
        ] {
            assert!(parse(args.into_iter().map(str::to_string)).is_err());
        }
        assert!(matches!(parse(["--help".into()]), Ok(Parsed::Help)));
        assert!(matches!(
            parse(["--describe".into(), "files greater than 10 MB".into()]),
            Ok(Parsed::Run(_))
        ));
    }
}
