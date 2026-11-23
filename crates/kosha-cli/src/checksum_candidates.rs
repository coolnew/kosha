// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use kosha_core::{format, say, sayln};
use kosha_import::checksums::{Algorithm, ChecksumRows};

const USAGE: &str = "kosha duplicate-candidates --algorithm ALGORITHM LISTING [LISTING]

Report candidate duplicates from one or two rclone lsjson --hash listings.
Select md5, sha1, sha256, quickxor or dropbox. Use - for one input from stdin.
Only equal recorded sizes and checksums match. With two inputs, each group
must occur on both sides. The report reads the listings without opening the
files they name.
";

const GROUPS_SHOWN: usize = 100;
const MEMBERS_SHOWN_PER_INPUT: usize = 10;

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        say!("{USAGE}");
        return Ok(());
    }
    let [flag, algorithm, inputs @ ..] = args else {
        return Err(USAGE.into());
    };
    if flag != "--algorithm" || !(1..=2).contains(&inputs.len()) {
        return Err(USAGE.into());
    }
    if inputs.iter().filter(|input| *input == "-").count() > 1 {
        return Err("standard input can be used for only one listing".into());
    }
    let algorithm: Algorithm = algorithm.parse()?;
    let mut report = ChecksumRows::new(algorithm);
    for name in inputs {
        let reader: Box<dyn Read> =
            if name == "-" {
                Box::new(io::stdin().lock())
            } else {
                Box::new(File::open(Path::new(name)).map_err(|error| {
                    format!("could not open {}: {error}", format::printable(name))
                })?)
            };
        report
            .read(reader)
            .map_err(|error| format!("{}: {error}", format::printable(name)))?;
    }
    report.finish();
    sayln!("Candidate duplicates from listing checksums ({algorithm})");
    sayln!("Checksums and sizes are reported by the inputs. File contents were not checked.");
    for (index, (name, coverage)) in inputs.iter().zip(report.coverage()).enumerate() {
        let label = if name == "-" { "standard input" } else { name };
        sayln!(
            "Input {} ({}): {} comparable of {} rows; {} directories, {} unknown sizes, {} missing checksums, {} invalid checksums",
            index + 1,
            format::printable(label),
            coverage.comparable,
            coverage.rows,
            coverage.directories,
            coverage.unknown_size,
            coverage.missing,
            coverage.invalid
        );
    }
    if report
        .coverage()
        .iter()
        .any(|source| source.comparable == 0)
    {
        if inputs.len() == 2 {
            sayln!("No shared coverage for {algorithm}.");
        } else {
            sayln!("No usable {algorithm} checksums.");
        }
        return Ok(());
    }
    let mut total_groups = 0;
    for (index, group) in report.groups().enumerate() {
        total_groups += 1;
        if index >= GROUPS_SHOWN {
            continue;
        }
        sayln!("");
        sayln!(
            "Group {} ({} apparent bytes per file)",
            index + 1,
            group.size()
        );
        for source in 0..inputs.len() {
            let mut members = group.members(source);
            for member in members.by_ref().take(MEMBERS_SHOWN_PER_INPUT) {
                sayln!(
                    "  input {} row {}  {}",
                    member.source + 1,
                    member.ordinal,
                    format::printable(member.path)
                );
            }
            let omitted = members.count();
            if omitted > 0 {
                sayln!("  input {}: {omitted} more members", source + 1);
            }
        }
    }
    sayln!("");
    sayln!("{}", format::plural(total_groups, "candidate group"));
    if total_groups > GROUPS_SHOWN {
        sayln!("Showing the first {GROUPS_SHOWN} groups by apparent size.");
    }
    Ok(())
}
