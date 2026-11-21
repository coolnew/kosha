// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Explicit publisher and fixed-view consumer. No normal search/marketplace
//! dispatch, source sniffing, config loading or source-path reopening here.

use kosha_core::{format, snapshot};
use kosha_query::Query;
use kosha_search::{RecipeBook, SearchContext, SearchMode};
use kosha_view::{Fields, Prepared, Selection};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

const HELP: &str = "kosha view — owner-approved historical metadata, not an agent sandbox

  kosha view create --snapshot FILE --root RELATIVE --alias LABEL [OPTIONS]
  kosha view inspect --input FILE
  kosha view query --input FILE --query DSL [--limit N] [--offset N]
  kosha view query --input FILE --search PHRASE [--limit N] [--offset N]
  kosha view session --input FILE

Create previews selection by default; no artifact is written.
  --exclude RELATIVE  Omit an exact recorded subtree beneath --root; repeatable
  --modified         Share modification times (default omitted)
  --created          Share creation times (default omitted)
  --out NEW-FILE --confirm --expect-sha256 DIGEST
                     Publish exactly the reviewed preview; never replace a file
Root/exclusions use / between portable UTF-8 names; . selects the snapshot root.
Alias: 1–64 ASCII letters/digits/._-, starting with a letter/digit.
Names/types/sizes are shared; owners, original roots/labels and hidden totals are not.
Directory totals include shared non-directory entries only. Folded hardlinks refuse.

Consumers never scan, refresh, load recipes, hash files or write output files.
Query output is JSON with scope, original observation time, coverage and pagination.
Use --limit 1..1000 (default 100), --offset N. Dates require shared fields.
Session reads JSONL requests: inspect, describe, query, close; one fixed view only.
Example: {\"op\":\"query\",\"query\":\"file and size > 10mb\",\"mode\":\"dsl\",\"limit\":20}
Session limits: 8 KiB/request, 1000 requests; new requests expire after ten minutes.
Matching checks a 30-second deadline. Idle/blocked pipes need a client-side timeout.
Requests cannot specify paths, outputs or other CLI flags. Unknown fields refuse.
Artifacts: kosha.view.v1 JSON, at most 64 MiB/250,000 entries/depth 128.
All shared bytes remain readable to the recipient. This is cooperative operation
scope; an unrestricted parent agent can still invoke other tools or normal Kosha.
";

const MAX_REQUEST: usize = 8192;
const MAX_QUERY: usize = 2048;
const MAX_ROWS: usize = 1000;

fn fail(message: &str) -> Value {
    let (code, detail) = message.split_once(": ").unwrap_or(("view_error", message));
    json!({"schema":"kosha.error.v1", "code":code, "message":detail, "retryable":false})
}

fn emit(value: &Value, out: &mut impl Write) -> io::Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    out.write_all(b"\n")?;
    out.flush()
}

pub fn run(argv: &[String]) -> ExitCode {
    match execute(argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = emit(&fail(&message), &mut io::stderr().lock());
            ExitCode::FAILURE
        }
    }
}

fn open_regular(path: &Path) -> Result<File, String> {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()) {
        return Err("view_io: input must be an explicit regular file, not a link".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|_| "view_io: cannot open the supplied artifact")?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err("view_io: input is not a regular file".into());
    }
    Ok(file)
}

#[derive(Default)]
struct Args {
    input: Option<PathBuf>,
    snapshot: Option<PathBuf>,
    root: Option<String>,
    alias: Option<String>,
    excludes: Vec<String>,
    fields: Fields,
    out: Option<PathBuf>,
    confirm: bool,
    expected: Option<String>,
    query: Option<String>,
    natural: bool,
    limit: Option<usize>,
    offset: Option<usize>,
}

fn execute(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() || matches!(argv[0].as_str(), "--help" | "-h") {
        kosha_core::say!("{HELP}");
        return Ok(());
    }
    let command = argv[0].as_str();
    if !matches!(command, "create" | "inspect" | "query" | "session") {
        return Err(
            "operation_not_granted: supported view operations are create, inspect, query, session"
                .into(),
        );
    }
    let mut args = Args::default();
    let mut seen = std::collections::HashSet::new();
    let mut iter = argv[1..].iter();
    while let Some(flag) = iter.next() {
        if flag != "--exclude" && !seen.insert(flag.as_str()) {
            return Err("invalid_request: duplicate option".into());
        }
        match flag.as_str() {
            "--modified" if command == "create" => args.fields.modified = true,
            "--created" if command == "create" => args.fields.created = true,
            "--confirm" if command == "create" => args.confirm = true,
            "--snapshot" | "--root" | "--alias" | "--exclude" | "--out" | "--expect-sha256"
                if command == "create" =>
            {
                let value = iter
                    .next()
                    .filter(|v| !v.starts_with("--"))
                    .ok_or("invalid_request: missing option value")?;
                match flag.as_str() {
                    "--snapshot" => args.snapshot = Some(value.into()),
                    "--root" => args.root = Some(value.clone()),
                    "--alias" => args.alias = Some(value.clone()),
                    "--exclude" => args.excludes.push(value.clone()),
                    "--expect-sha256" => args.expected = Some(value.clone()),
                    _ => args.out = Some(value.into()),
                }
            }
            "--input" if command != "create" => {
                args.input = Some(
                    iter.next()
                        .ok_or("invalid_request: --input needs a file")?
                        .into(),
                )
            }
            "--query" | "--search" if command == "query" && args.query.is_none() => {
                args.query = Some(iter.next().ok_or("invalid_request: missing query")?.clone());
                args.natural = flag == "--search";
            }
            "--limit" | "--offset" if command == "query" => {
                let value: usize = iter
                    .next()
                    .ok_or("invalid_request: missing integer")?
                    .parse()
                    .map_err(|_| "invalid_request: expected unsigned integer")?;
                if flag == "--limit" {
                    args.limit = Some(value);
                } else {
                    args.offset = Some(value);
                }
            }
            _ => {
                return Err(
                    "operation_not_granted: option is not available for this view operation".into(),
                );
            }
        }
    }
    if command == "create" {
        return create(args);
    }
    if command == "query" && args.query.is_none() {
        return Err("invalid_request: use --query or --search".into());
    }
    if args.limit.is_some_and(|n| n == 0 || n > MAX_ROWS) {
        return Err("view_limit: result limit must be 1..1000".into());
    }
    let file = open_regular(
        args.input
            .as_deref()
            .ok_or("invalid_request: --input is required")?,
    )?;
    let view = kosha_view::read(file)?;
    let now = format::now_unix();
    if command == "session" {
        return session(&view, now);
    }
    let response = if command == "inspect" {
        metadata(&view, now)
    } else {
        answer(
            &view,
            &Request {
                op: "query".into(),
                id: None,
                query: args.query,
                mode: Some(if args.natural {
                    Mode::Natural
                } else {
                    Mode::Dsl
                }),
                limit: args.limit,
                offset: args.offset,
            },
            now,
        )?
    };
    emit(&response, &mut io::stdout().lock()).map_err(|_| "view_io: response pipe closed".into())
}

fn create(args: Args) -> Result<(), String> {
    if args.confirm && args.out.is_none() {
        return Err("invalid_request: --confirm requires --out".into());
    }
    if args.confirm != args.expected.is_some() {
        return Err(
            "invalid_request: --confirm requires --expect-sha256 from a reviewed preview".into(),
        );
    }
    if args.expected.as_ref().is_some_and(|s| {
        s.len() != 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err("invalid_request: expected a lowercase SHA-256 digest".into());
    }
    let root = args
        .root
        .as_deref()
        .ok_or("invalid_request: --root is required")?;
    let alias = args
        .alias
        .as_deref()
        .ok_or("invalid_request: --alias is required")?;
    let source = open_regular(
        args.snapshot
            .as_deref()
            .ok_or("invalid_request: --snapshot is required")?,
    )?;
    let (meta, tree) = snapshot::read(source)
        .map_err(|_| "view_invalid: could not read the explicitly supplied snapshot")?;
    let view = kosha_view::publish(
        &tree,
        meta.taken,
        Selection {
            root,
            alias,
            excludes: &args.excludes,
            fields: args.fields,
        },
    )?;
    let bytes = kosha_view::encode(&view)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if args
        .expected
        .as_ref()
        .is_some_and(|expected| *expected != digest)
    {
        return Err(
            "view_changed: artifact differs from reviewed preview; nothing published".into(),
        );
    }
    let prepared = kosha_view::prepare(view)?;
    let mut response = metadata(&prepared, format::now_unix());
    response["operation"] = json!(if args.confirm { "published" } else { "preview" });
    response["artifact_bytes"] = json!(bytes.len());
    response["sha256"] = json!(digest);
    response["sample_paths"] = json!(
        (0..prepared.view().entries.len().min(12))
            .map(|i| prepared.path(i))
            .collect::<Vec<_>>()
    );
    response["file_written"] = json!(args.confirm);
    if args.confirm {
        let destination = args.out.expect("validated output");
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| "view_io: cannot stage the new output")?;
        temp.write_all(&bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|_| "view_io: cannot finish the new output")?;
        temp.persist_noclobber(&destination).map_err(
            |_| "view_output_refused: destination exists or cannot be created; no replacement",
        )?;
    }
    emit(&response, &mut io::stdout().lock()).map_err(|_| "view_io: response pipe closed".into())
}

fn metadata(view: &Prepared, now: u32) -> Value {
    let v = view.view();
    json!({"schema":"kosha.view.result.v1", "alias":v.alias,
        "source":"historical_metadata_view", "observed_unix":v.observed_unix,
        "age_seconds":u64::from(now).saturating_sub(v.observed_unix),
        "observed_in_future":v.observed_unix > u64::from(now), "query_clock_unix":now,
        "fields":v.fields, "coverage":v.coverage, "entries":v.entries.len(),
        "allocated":v.entries[0].allocated, "apparent":v.entries[0].apparent,
        "enforcement":"cooperative_view_operations_only", "is_agent_sandbox":false})
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Dsl,
    Natural,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    op: String,
    id: Option<String>,
    query: Option<String>,
    mode: Option<Mode>,
    limit: Option<usize>,
    offset: Option<usize>,
}

fn compile(text: &str, mode: Mode, now: u32) -> Result<Query, String> {
    if text.is_empty() || text.len() > MAX_QUERY {
        return Err("view_limit: query must have 1..2048 UTF-8 bytes".into());
    }
    match mode {
        Mode::Dsl => Query::parse(text).map_err(|_| "query_invalid: invalid DSL".into()),
        Mode::Natural => {
            let local = format::local_date(now).unwrap_or_else(|| {
                let (y, m, d) = format::civil_from_days(i64::from(now) / 86400);
                [y as i32, m as i32, d as i32]
            });
            let translation = RecipeBook::default().translate_with_resolver(
                text,
                SearchContext::new(now, local),
                SearchMode::Natural,
                |[y, m, d]| {
                    format::local_midnight(i64::from(y), m as u32, d as u32).or_else(|| {
                        u64::try_from(
                            format::days_from_civil(i64::from(y), m as u32, d as u32) * 86400,
                        )
                        .ok()
                    })
                },
            );
            if !translation.is_valid() {
                return Err(
                    "query_invalid: unsupported natural phrase; no broader query executed".into(),
                );
            }
            translation
                .query
                .ok_or_else(|| "query_invalid: no interpretation".into())
        }
    }
}

fn answer(view: &Prepared, request: &Request, now: u32) -> Result<Value, String> {
    if request.id.as_ref().is_some_and(|s| s.len() > 128) {
        return Err("view_limit: request id exceeds 128 bytes".into());
    }
    if request.op == "inspect"
        && request.query.is_none()
        && request.mode.is_none()
        && request.limit.is_none()
        && request.offset.is_none()
    {
        return Ok(metadata(view, now));
    }
    if !matches!(request.op.as_str(), "query" | "describe") {
        return Err(
            "operation_not_granted: session allows inspect, describe, query, close only".into(),
        );
    }
    let query = compile(
        request
            .query
            .as_deref()
            .ok_or("invalid_request: query is required")?,
        request.mode.unwrap_or(Mode::Dsl),
        now,
    )?;
    view.check_query(&query)?;
    if request.op == "describe" {
        if request.limit.is_some() || request.offset.is_some() {
            return Err("invalid_request: describe has no result pagination".into());
        }
        return Ok(
            json!({"schema":"kosha.view.description.v1","dsl":query.source(),"live_source_opened":false,"uses_loaded_view":true,"query_clock_unix":now,"recipes":"built_in_only"}),
        );
    }
    let limit = request.limit.unwrap_or(100);
    if limit == 0 || limit > MAX_ROWS {
        return Err("view_limit: result limit must be 1..1000".into());
    }
    let offset = request.offset.unwrap_or(0);
    let mut total = 0usize;
    let mut rows = Vec::new();
    let started = Instant::now();
    for i in 1..view.view().entries.len() {
        if i % 256 == 0 && started.elapsed() > Duration::from_secs(30) {
            return Err("view_timeout: query deadline reached".into());
        }
        if view.matches(&query, i, now) {
            if total >= offset && rows.len() < limit {
                rows.push(json!({"path":view.path(i),"entry":view.view().entries[i]}));
            }
            total += 1;
        }
    }
    let mut response = metadata(view, now);
    response["dsl"] = json!(query.source());
    response["offset"] = json!(offset);
    response["matches"] = json!(total);
    response["has_more"] = json!(total.saturating_sub(offset) > rows.len());
    response["rows"] = json!(rows);
    Ok(response)
}

fn session(view: &Prepared, now: u32) -> Result<(), String> {
    let started = Instant::now();
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut ready = metadata(view, now);
    ready["operation"] = json!("session_ready");
    emit(&ready, &mut output).map_err(|_| "view_io: response pipe closed")?;
    for _ in 0..1000 {
        let mut line = Vec::new();
        let n = Read::by_ref(&mut input)
            .take((MAX_REQUEST + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| "view_io: request pipe failed")?;
        if n == 0 {
            return Ok(());
        }
        if n > MAX_REQUEST {
            return Err("view_limit: request exceeds 8 KiB; session closed".into());
        }
        if started.elapsed() > Duration::from_secs(600) {
            return Err("view_expired: session expired".into());
        }
        let parsed: Result<Request, _> = serde_json::from_slice(&line);
        let (id, result) = match parsed {
            Ok(request) => {
                let id = request.id.as_ref().filter(|s| s.len() <= 128).cloned();
                if request.op == "close"
                    && request.query.is_none()
                    && request.mode.is_none()
                    && request.limit.is_none()
                    && request.offset.is_none()
                    && request.id.as_ref().is_none_or(|s| s.len() <= 128)
                {
                    emit(&json!({"operation":"closed","id":id}), &mut output)
                        .map_err(|_| "view_io: response pipe closed")?;
                    return Ok(());
                }
                (id, answer(view, &request, now))
            }
            Err(_) => (
                None,
                Err("invalid_request: malformed request or ungranted fields".into()),
            ),
        };
        let mut response = match result {
            Ok(value) => value,
            Err(error) => fail(&error),
        };
        response["id"] = json!(id);
        emit(&response, &mut output).map_err(|_| "view_io: response pipe closed")?;
    }
    Err("view_limit: request budget exhausted; session closed".into())
}
