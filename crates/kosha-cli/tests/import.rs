// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `kosha import`, end to end: a synthetic ncdu export in, an imported record
//! in the snapshot store out, and every way of looking at that record saying
//! it was imported and not measured by kosha.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A synthetic export of a machine that is not this one. The root names a
/// path that does not exist here; nothing may ever try to open it.
const EXPORT: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9","timestamp":1763647320},
[{"name":"/kosha-import-test-nonexistent/srv","asize":4096,"dsize":4096,"dev":2049},
{"name":"notes.txt","asize":1200,"dsize":4096},
[{"name":"project","dsize":4096},
{"name":"Cargo.toml","asize":300,"dsize":4096},
[{"name":"target","dsize":4096},
{"name":"big.o","asize":5242880,"dsize":5242880},
{"name":"twin.o","asize":5242880,"dsize":5242880,"ino":7,"nlink":2},
{"name":"twin-link.o","asize":5242880,"dsize":5242880,"ino":7,"nlink":2}
]],
[{"name":"locked","read_error":true}]
]]
"#;

fn run_in(args: &[&str], home: &Path, stdin: Option<&[u8]>) -> Output {
    let mut command = Command::new(common::kosha());
    command
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("roaming"))
        .env("LOCALAPPDATA", home.join("local"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("kosha runs");
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn run(args: &[&str], home: &Path) -> Output {
    run_in(args, home, None)
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exited {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn refused(out: &Output) -> String {
    assert!(
        !out.status.success(),
        "accepted:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const SENTENCE: &str =
    "Imported from ncdu 2.9 (export 2025-11-20 14:02 UTC). Not measured by Kosha.";

/// Import the export and return the saved record's path.
fn imported(home: &Path) -> PathBuf {
    let input = home.join("srv.json");
    std::fs::write(&input, EXPORT).unwrap();
    let out = ok(&run(
        &[
            "import",
            "--from",
            "ncdu",
            &input.to_string_lossy(),
            "--label",
            "build-box",
        ],
        home,
    ));
    assert!(out.contains(SENTENCE), "{out}");
    assert!(!out.to_lowercase().contains("scanned"), "{out}");
    let line = out
        .lines()
        .find(|line| line.trim_start().starts_with("imported to"))
        .unwrap_or_else(|| panic!("no destination in:\n{out}"));
    let path = PathBuf::from(line.trim_start().trim_start_matches("imported to").trim());
    assert!(path.is_file(), "{}", path.display());
    assert_eq!(path.extension().unwrap(), "kosha");
    path
}

#[test]
fn an_import_is_kept_and_listed_as_imported() {
    let home = tempfile::tempdir().unwrap();
    let record = imported(home.path());
    let bytes = std::fs::read(&record).unwrap();
    assert_eq!(&bytes[..8], b"KOSHASNP");
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);

    let listed = ok(&run(&["--snapshots"], home.path()));
    assert!(listed.contains(SENTENCE), "{listed}");
    assert!(listed.contains("build-box"), "{listed}");
    assert!(listed.contains("1 snapshot"), "{listed}");
}

#[test]
fn browsing_an_imported_record_says_so_everywhere() {
    let home = tempfile::tempdir().unwrap();
    let record = imported(home.path());
    let record = record.to_string_lossy();

    let out = run(
        &[&record, "--stats", "--insights", "--cleanup-guide"],
        home.path(),
    );
    let text = ok(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(text.starts_with("IMPORTED"), "{text}");
    assert!(text.contains(SENTENCE), "{text}");
    assert!(text.contains("imported, not measured by Kosha"), "{text}");
    assert!(
        text.contains("LISTING  imported, not measured by Kosha"),
        "{text}"
    );
    assert!(!text.contains("\nSCAN\n"), "{text}");
    assert!(text.contains("hardlinks deduped"), "{text}");
    assert!(text.contains("advice is about that machine"), "{text}");
    assert!(
        stderr.contains("source: imported; measured_by_kosha=false"),
        "{stderr}"
    );
    assert!(stderr.contains("records no modification times"), "{stderr}");
    // Nothing offers to act, and nothing about this machine's allocation.
    assert!(!text.contains("Trash"), "{text}");
    assert!(!text.contains("cluster slack"), "{text}");
    assert!(
        text.contains("Nothing here can be reclaimed by acting")
            || text.contains("nothing worth reporting"),
        "{text}"
    );
}

#[test]
fn search_and_comparison_work_on_an_imported_record_and_say_what_it_is() {
    let home = tempfile::tempdir().unwrap();
    let record = imported(home.path());
    let record = record.to_string_lossy();

    let out = run(
        &["search", "--load", &record, "files over 1 MB"],
        home.path(),
    );
    let text = ok(&out);
    assert!(text.contains(SENTENCE), "{text}");
    assert!(text.contains("big.o"), "{text}");
    assert!(text.contains("(imported)"), "{text}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("source: imported"));

    let out = run(
        &[&record, "--since", &record, "--since-min", "0"],
        home.path(),
    );
    let text = ok(&out);
    assert!(text.contains("the record is an imported listing"), "{text}");
    assert!(text.contains("this tree is an imported listing"), "{text}");
    assert!(text.contains("Nothing changed"), "{text}");

    let json = ok(&run(&[&record, "--export", "json"], home.path()));
    assert!(json.contains("\"imported\": true"), "{json}");
    assert!(json.contains("\"measured_by_kosha\": false"), "{json}");
}

#[test]
fn nothing_can_be_done_to_an_imported_record() {
    let home = tempfile::tempdir().unwrap();
    let record = imported(home.path());
    let record = record.to_string_lossy();
    for (flags, said) in [
        (&["--duplicates"][..], "duplicate hunt"),
        (&["--snapshot"][..], "already a snapshot"),
        (&["--mft"][..], "says how to scan a disk"),
        (&["--depth", "1"][..], "says how to scan a disk"),
    ] {
        let mut args = vec![&*record];
        args.extend_from_slice(flags);
        let stderr = refused(&run(&args, home.path()));
        assert!(stderr.contains(said), "{flags:?}: {stderr}");
    }
}

#[test]
fn standard_input_and_refusals() {
    let home = tempfile::tempdir().unwrap();
    let out = run_in(
        &["import", "--from", "ncdu", "-"],
        home.path(),
        Some(EXPORT.as_bytes()),
    );
    let text = ok(&out);
    assert!(text.contains("standard input"), "{text}");
    assert!(text.contains(SENTENCE), "{text}");

    let binary = refused(&run_in(
        &["import", "--from", "ncdu", "-"],
        home.path(),
        Some(b"\xbfncduEX1\x00\x01"),
    ));
    assert!(binary.contains("ncdu -o"), "{binary}");

    let cut = refused(&run_in(
        &["import", "--from", "ncdu", "-"],
        home.path(),
        Some(&EXPORT.as_bytes()[..EXPORT.len() / 2]),
    ));
    assert!(
        cut.contains("ends early") || cut.contains("not well-formed"),
        "{cut}"
    );

    let small = refused(&run_in(
        &["import", "--from", "ncdu", "-", "--max-input-bytes", "100"],
        home.path(),
        Some(EXPORT.as_bytes()),
    ));
    assert!(small.contains("--max-input-bytes"), "{small}");

    assert!(refused(&run(&["import", "srv.json"], home.path())).contains("--from ncdu"));
    let missing = refused(&run(
        &[
            "import",
            "--from",
            "ncdu",
            &home.path().join("missing.json").to_string_lossy(),
        ],
        home.path(),
    ));
    assert!(missing.contains("could not read"), "{missing}");
    let folder = refused(&run(
        &["import", "--from", "ncdu", &home.path().to_string_lossy()],
        home.path(),
    ));
    assert!(folder.contains("is a directory"), "{folder}");

    // Nothing was kept by any refusal: only the one good import is listed.
    let listed = ok(&run(&["--snapshots"], home.path()));
    assert!(listed.contains("1 snapshot"), "{listed}");

    let help = ok(&run(&["import", "--help"], home.path()));
    assert!(help.contains("--from FORMAT"), "{help}");
}
