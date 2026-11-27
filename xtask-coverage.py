#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Render measured LLVM coverage totals, provenance, an offline landing page and badge."""

import argparse
from collections import Counter
from datetime import datetime, timezone
import html
import json
import os
from pathlib import Path
import re


def metrics(report):
    if report.get("type") != "llvm.coverage.json.export" or len(report.get("data", [])) != 1:
        raise ValueError("Expected one LLVM coverage export")
    totals = report["data"][0]["totals"]
    result = {}
    for kind in ("lines", "functions", "regions"):
        count, covered = totals[kind]["count"], totals[kind]["covered"]
        if (type(count) is not int or type(covered) is not int
                or not 0 <= covered <= count):
            raise ValueError("Invalid coverage counters")
        result[kind] = {"count": count, "covered": covered,
                        "percent": round(covered * 100 / count, 2) if count else None}
    if not result["lines"]["count"]:
        raise ValueError("No measured production lines; refusing a misleading badge")
    return result


def render(report, output, commit, repository, platform, scope="workspace"):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Expected full source commit SHA")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("Expected owner/repository")
    if platform not in {"linux", "windows", "macos"}:
        raise ValueError("Unknown coverage platform")
    scopes = {"workspace": "Rust workspace; cargo-llvm-cov default exclusions",
              "smoke": "Focused tooling smoke test; not project coverage"}
    if scope not in scopes:
        raise ValueError("Unknown measurement scope")
    values = metrics(report)
    if not (output / "coverage/html/index.html").is_file():
        raise ValueError("Generate the LLVM HTML report before its landing page")
    data = {"schema": "kosha.coverage.v1", "commit": commit, "repository": repository,
            "platform": platform, "scope": scopes[scope],
            "generated_at": datetime.now(timezone.utc).isoformat(), "metrics": values,
            "limitations": "Platform-specific line execution; not GUI usability, branch coverage or doctest coverage."}
    percent = values["lines"]["percent"]
    color = "#4c1" if percent >= 90 else "#dfb317" if percent >= 75 else "#fe7d37" if percent >= 50 else "#e05d44"
    badge = f'''<svg xmlns="http://www.w3.org/2000/svg" width="210" height="20" role="img" aria-label="{platform} line coverage: {percent:.1f}%">
<title>{platform} line coverage: {percent:.1f}%</title>
<rect width="150" height="20" fill="#555"/><rect x="150" width="60" height="20" fill="{color}"/>
<g fill="#fff" font-family="Verdana,sans-serif" font-size="11" text-anchor="middle"><text x="75" y="14">{platform} line coverage</text><text x="180" y="14">{percent:.1f}%</text></g></svg>'''
    rows = "".join(f"<tr><td>{kind.title()}</td><td>{v['covered']:,} / {v['count']:,}</td><td>{str(v['percent']) + '%' if v['percent'] is not None else 'unmeasured'}</td></tr>" for kind, v in values.items())
    page = f'''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Kosha test coverage</title><style>body{{font:17px/1.6 system-ui;max-width:900px;margin:3rem auto;padding:0 1rem;color:#172033;background:#f8fafc}}a{{color:#174caa}}table{{border-collapse:collapse}}td,th{{padding:.5rem 1.2rem;border-bottom:1px solid #ccd}}code{{overflow-wrap:anywhere}}</style>
<h1>Kosha test coverage</h1><p><img src="coverage.svg" alt="{platform} line coverage"></p>
<p>Measured on <strong>{platform}</strong> from commit <a href="https://github.com/{repository}/tree/{commit}"><code>{commit}</code></a>.</p>
<p>Generated {html.escape(data['generated_at'])}. This is a development report, not a release approval.</p>
<table><tr><th>Metric</th><th>Covered / measured</th><th>Percent</th></tr>{rows}</table>
<p><a href="coverage/html/index.html">Browse annotated source coverage</a> · <a href="summary.json">Download provenance and totals</a></p>
<p>{html.escape(data['scope'])}. Standalone test files and dependencies are excluded by the tool defaults; inline test modules may contribute.</p>
<p>{html.escape(data['limitations'])} In particular, a Linux report does not measure Windows-only filesystem code.</p>
<p>The report and badge update only after a successful instrumented test run. Check the commit and date when assessing freshness.</p></html>'''
    (output / "summary.json").write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8", newline="\n")
    (output / "coverage.svg").write_text(badge + "\n", encoding="utf-8", newline="\n")
    (output / "index.html").write_text(page + "\n", encoding="utf-8", newline="\n")
    summary = f"## {platform.title()} Rust coverage\n\nCommit: `{commit}`\n\n| Metric | Covered / measured | Percent |\n|---|---:|---:|\n"
    summary += "".join(f"| {k} | {v['covered']} / {v['count']} | {v['percent']} |\n" for k, v in values.items())
    summary += "\nDownload the coverage artifact for offline HTML, LCOV and provenance. Pages publication is opt-in through KOSHA_COVERAGE_PAGES.\n\n" + data["limitations"] + "\n"
    return data, summary


GUI = "crates/kosha-gui/src/app.rs"
WORKER = "crates/kosha-core/src/ntfs_worker"


def fast_ntfs_cases():
    return [
        ("offer-action", "Offer Fast NTFS for an eligible drive", GUI,
         'PopupMenuItem::new("Fast NTFS scan…")', None),
        ("accept", "Approve UAC and show the MFT result", GUI,
         "Ok(tree) => (Ok(tree), None),", None),
        ("decline", "Decline UAC and fall back to a standard scan", GUI,
         '"Fast NTFS unavailable ({error}). Used a standard scan."', None),
        ("cancel", "Cancel the scan and preserve the last snapshot", GUI,
         "Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => {", None),
        ("ineligible", "Reject a scope other than a whole lettered drive", WORKER + "/launch.rs",
         "fn drive_root_letter(root: &Path) -> Option<u8> {",
         "ntfs_worker::launch::worker_preflight_requires_a_whole_lettered_drive"),
        ("malformed", "Reject a malformed worker request before scanning", WORKER + ".rs",
         "fn invalid_request() -> io::Error {",
         "ntfs_worker::tests::malformed_requests_are_rejected_before_scanning"),
        ("worker-unavailable", "Handle a missing worker", WORKER + "/launch.rs",
         '"the Fast NTFS worker is missing",', None),
        ("drive-changed", "Reject a drive whose identity changed during the scan",
         "crates/kosha-core/src/ntfs/strategy.rs",
         "if VolumeId::at_root(&root).as_ref() != Some(&identity) {", None),
        ("worker-timeout", "Fall back when the worker fails to connect", WORKER + "/launch.rs",
         "let pipe = listener.accept(Instant::now() + CONNECT_LIMIT, &stopped)?;", None),
    ]


def lcov_lines(text):
    """Index Rust source hits by repo-relative path, including Windows exports."""
    files = {}
    current = None
    for line in text.splitlines():
        if line.startswith("SF:"):
            path = line[3:].replace("\\", "/")
            match = re.search(r"(?:^|/)(crates/[^:]+[.]rs)$", path)
            current = files.setdefault(match[1], {}) if match else None
        elif current is not None and line.startswith("DA:"):
            number, hits, *_ = line[3:].split(",")
            number, hits = int(number), int(hits)
            if number < 1 or hits < 0:
                raise ValueError("Invalid LLVM LCOV line counter")
            current[number] = current.get(number, 0) + hits
        elif line == "end_of_record":
            current = None
    if not any(files.values()):
        raise ValueError("Expected LLVM LCOV with Rust source line counters")
    return files


PLATFORMS = {"linux": "Linux", "windows": "Windows", "macos": "macOS"}
# Each group names the Rust integration target that must report the test.
# Cases with environmental early exits need stronger evidence before mapping.
INTEGRATION_CASES = (
    ("Kosha CLI: scanning and comparison", "kosha-cli", "end_to_end", (
        ("cli-scan", "Scan a folder and report its contents", "a_scan_reports_the_tree_it_was_given"),
        ("cli-stats", "Report file and directory counts", "stats_count_what_is_there"),
        ("cli-missing-path", "Fail cleanly for a missing path", "a_path_that_is_not_there_says_so_and_fails"),
        ("cli-query", "Filter a scan with a query", "a_query_matches_the_files_it_should"),
        ("cli-export", "Export the same tree consistently", "the_export_is_the_same_twice"),
        ("cli-duplicates", "Find identical files across folders", "identical_files_are_found_in_different_folders"),
        ("cli-depth", "Report depth-limited totals as a lower bound", "a_depth_limited_scan_is_a_floor_and_says_so_everywhere"),
        ("cli-snapshot", "Save, list, browse and compare a snapshot", "a_snapshot_is_kept_listed_browsed_and_compared"),
        ("cli-compare", "Compare live folders and snapshots", "compares_live_folders_and_snapshots"),
        ("cli-content-compare", "Detect changed contents despite equal size and time", "content_verification_finds_same_size_and_time_edits"),
        ("cli-offline-compare", "Compare saved snapshots with their original roots absent", "compares_two_saved_snapshots_without_opening_their_source_roots"),
        ("cli-corrupt-snapshot", "Report an unreadable saved snapshot", "a_snapshot_that_cannot_be_read_is_reported_rather_than_skipped"),
        ("cli-refuse-copy", "Refuse to save a snapshot of a snapshot", "keeping_a_snapshot_of_a_snapshot_is_refused"),
        ("cli-refuse-scan-flag", "Reject live scan options on saved snapshots", "a_scan_flag_on_a_snapshot_is_refused_and_a_reporting_flag_is_not"),
        ("cli-fragmentation-validation", "Validate fragmentation limits before opening a path", "fragmentation_help_and_sample_limits_are_validated_before_opening_a_path"),
    )),
    ("Kosha CLI: search", "kosha-cli", "search", (
        ("search-describe", "Translate an intent without scanning", "describe_translates_entities_without_scanning_or_creating_files"),
        ("search-refuse-intent", "Reject invalid or conflicting search inputs before scanning", "invalid_intent_and_conflicting_options_are_refused_before_source_io"),
        ("search-scope", "Keep complete match counts when limiting displayed rows", "search_scans_only_the_explicit_path_and_top_keeps_complete_counts"),
        ("search-export", "Export all matches without the human report", "export_contains_all_matches_and_no_human_report_even_with_top_one"),
        ("search-offline", "Search saved creation dates with the live root absent", "load_searches_recorded_birth_dates_without_touching_the_missing_root"),
        ("search-unknown-dates", "Report unknown creation dates without substituting modification dates", "missing_birth_metadata_is_reported_and_never_replaced_by_mtime"),
        ("search-recipes", "List available recipes without opening a source", "recipe_discovery_tracks_the_platform_without_opening_a_source"),
    )),
    ("Kosha CLI: cache and automation", "kosha-cli", "cache_and_automation", (
        ("cache-inventory", "List saved records without reading the source", "cache_inventory_lists_shared_and_custom_records_without_reading_the_source"),
        ("cache-absent", "List an absent cache without creating it", "absent_cache_inventory_is_empty_and_does_not_create_directories"),
        ("cache-precedence", "Resolve saved snapshot names with local precedence", "saved_snapshot_names_work_for_browsing_search_and_comparison_with_local_precedence"),
        ("cache-explicit", "Use a fresh scan by default and cache only on request", "fresh_scan_ignores_shared_cache_and_explicit_cache_reads_the_old_record"),
        ("cache-miss", "Report a cache miss without falling back to a live scan", "custom_cache_miss_and_invalid_cache_combinations_never_fall_back_to_scan"),
        ("cache-exports", "Keep snapshot messages out of machine-readable stdout", "snapshot_headers_and_save_reports_do_not_contaminate_machine_stdout"),
    )),
    ("Kosha CLI: imports and shared views", "kosha-cli", "import", (
        ("import-inventory", "Keep and list an imported record", "an_import_is_kept_and_listed_as_imported"),
        ("import-analysis", "Search and compare imported records with source attribution", "search_and_comparison_work_on_an_imported_record_and_say_what_it_is"),
        ("import-stdin", "Import from standard input and reject invalid input", "standard_input_and_refusals"),
    )),
    ("Kosha CLI: imports and shared views", "kosha-cli", "views", (
        ("view-preview", "Preview publication and preserve an existing destination", "publication_preview_and_noclobber_preserve_source_and_existing_destination"),
        ("view-offline", "Query a fixed view after the artifact and source are removed", "fixed_session_queries_after_artifact_and_live_sources_are_removed_and_rejects_escape_requests"),
        ("view-confirmation", "Refuse publication when the artifact changes after preview", "confirmation_refuses_a_changed_artifact_before_creating_output"),
        ("view-pagination", "Keep response pages bounded and report only known totals", "pagination_has_bounded_rows_and_no_invented_totals"),
    )),
    ("Kosha CLI: pipes", "kosha-cli", "pipe", (
        ("cli-pipe-version", "Write the version through a pipe", "the_version_survives_a_pipe"),
        ("cli-pipe-close", "Handle a consumer closing the pipe early", "a_reader_that_walks_away_is_not_a_failure"),
    )),
    ("Filesystem scanning", "kosha-core", "scan", (
        ("scan-cancel", "Return a valid partial tree after cancellation", "cancellation_yields_a_valid_partial_tree"),
        ("scan-empty", "Scan an empty directory successfully", "empty_directory_scans_cleanly"),
        ("scan-hardlinks", "Count hard-linked bytes once", "hardlinked_bytes_are_counted_once"),
    )),
    ("Filesystem scanning", "kosha-core", "unreadable_root", (
        ("scan-denied", "Distinguish unreadable directories from empty directories",
         "a_root_that_could_not_be_read_says_so_rather_than_reading_as_empty"),
    )),
)
NATIVE_CASES = {
    "linux": (
        ("Kosha CLI: Linux", "kosha-cli", "cache_and_automation", (
            ("linux-rollback", "Roll back and switch both managed executables", "update_rollback_switches_the_managed_pair_from_the_cli"),
        )),
        ("Kosha CLI: Linux", "kosha-cli", "search", (
            ("linux-recipes", "Match Linux recipes within the selected root", "builtin_recipes_match_metadata_within_the_selected_root"),
        )),
    ),
    "windows": (
        ("Kosha CLI: Windows", "kosha-cli", "search", (
            ("windows-creation-dates", "Search live files by Windows creation date", "freshly_scanned_files_keep_their_recorded_creation_dates"),
        )),
        ("Filesystem scanning", "kosha-core", "scan", (
            ("windows-long-path", "Scan a tree beyond the Windows legacy path limit", "a_tree_deeper_than_max_path_is_scanned_whole"),
        )),
    ),
}
UNIX_CASES = (
    ("Filesystem scanning", "kosha-core", "scan", (
        ("unix-symlink", "Measure symbolic links without descending through them", "symlinks_are_measured_not_followed"),
    )),
)
def manual_capture(description, page, section, image, published_commit):
    return {
        "kind": "manual screenshot",
        "description": description,
        "url": f"https://coolnew.github.io/kosha/manual/{page}/#{section}",
        "published_commit": published_commit,
        "image": f"docs/manual/images/{image}",
        "manual": f"docs/manual/{page}.md",
    }


MENU_EVIDENCE = manual_capture(
    "Windows Sandbox screenshot showing the Fast NTFS action in the drive menu",
    "scanning", "fast-ntfs-scan-on-windows", "gui-windows-fast-ntfs-menu.png",
    "38b9db85fa14bc4ddae640934e38e9d9d53c3ebe",
)
COMPARISON_CAPTURE_COMMIT = "13079aef9ef0237de7e205011bb9c6d0f358a8f3"
DESKTOP_CAPTURES = {
    "linux": (
        ("desktop-compare", "Show a same-size edit in folder comparison", manual_capture(
            "Linux folder comparison showing a same-size edit", "snapshots", "compare-observations",
            "gui-linux-folder-comparison.png", COMPARISON_CAPTURE_COMMIT)),
        ("desktop-quick-look", "Show sizes and extension breakdown for a selected folder", manual_capture(
            "Linux Quick Look for a selected Media folder", "keyboard", "file-list-and-search-results",
            "gui-linux-quick-look.png", "6807a85fbb8f17dd03410abbcee877a7594f575b")),
        ("desktop-volume-space", "Show reserved free space and unexplained usage", manual_capture(
            "Linux Volume space for a synthetic ext4 volume", "snapshots", "review-volume-space",
            "gui-linux-volume-space.png", "183616a4480f68adff81d7bcb49704a31358f676")),
    ),
    "macos": (
        ("desktop-compare", "Show a same-size edit when verifying folder contents", manual_capture(
            "macOS folder comparison showing File contents differ", "snapshots", "compare-observations",
            "gui-macos-folder-verification.png", COMPARISON_CAPTURE_COMMIT)),
        ("desktop-monitoring", "Show a monitored folder growth alert", manual_capture(
            "macOS monitoring showing a synthetic growth alert", "scanning", "monitor-local-storage-over-time",
            "gui-macos-monitoring.png", "38b9db85fa14bc4ddae640934e38e9d9d53c3ebe")),
    ),
    "windows": (
        ("desktop-compare", "Show a verified content match and items unique to each side", manual_capture(
            "Windows folder comparison showing one match and one unique item per side",
            "snapshots", "compare-observations", "gui-windows-folder-comparison.png", COMPARISON_CAPTURE_COMMIT)),
    ),
}


def scenario_report(root, lcov, test_log, output, commit, repository, platform="windows"):
    summary = json.loads((output / "summary.json").read_text(encoding="utf-8"))
    if (summary["platform"], summary["commit"], summary["repository"]) != (platform, commit, repository) or (
            summary["schema"] != "kosha.coverage.v1" or not summary["scope"].startswith("Rust workspace;")):
        raise ValueError(f"Scenario evidence needs the matching {PLATFORMS[platform]} measurement")
    passed = set(re.findall(r"^test ([\w:]+) [.]{3} ok[^\S\r\n]*$", test_log, re.MULTILINE))
    if not passed:
        raise ValueError("Expected successful named tests from Rust's test harness")
    # Cargo identifies the integration target on stderr. Where Cargo omits
    # the crate prefix, the target and exact test name identify the assertion.
    integration_passed = set()
    target = None
    for line in test_log.replace("\\", "/").splitlines():
        if re.match(r"^\s*Running ", line):
            match = re.match(r"^\s*Running (?:(?:crates/)?([^/ ]+)/)?tests/([\w_]+)[.]rs \(", line)
            target = match.groups() if match else None
        if target and (match := re.fullmatch(r"test ([\w:]+) [.]{3} ok\s*", line)):
            integration_passed.add((*target, match[1]))
    if (None, "end_to_end", "a_scan_reports_the_tree_it_was_given") not in integration_passed and (
            "kosha-cli", "end_to_end", "a_scan_reports_the_tree_it_was_given") not in integration_passed:
        raise ValueError("Expected passing Kosha CLI end-to-end tests in Cargo output")
    files = lcov_lines(lcov)
    workflows = {}
    groups = INTEGRATION_CASES + NATIVE_CASES.get(platform, ())
    if platform in {"linux", "macos"}:
        groups += UNIX_CASES
    for workflow, crate, harness, cases in groups:
        source = f"crates/{crate}/tests/{harness}.rs"
        for case, label, test in cases:
            workflows.setdefault(workflow, []).append(
                (case, label, source, f"fn {test}() {{", test, None, (crate, harness)))
    if platform == "macos":
        workflows["Filesystem scanning"].append((
            "macos-volume-uuid", "Identify the macOS system volume by a stable UUID",
            "crates/kosha-core/src/volume_history.rs",
            "fn macos_system_volume_has_a_persistent_uuid() {",
            "volume_history::tests::macos_system_volume_has_a_persistent_uuid", None, None))
    if platform == "windows":
        workflows["Fast NTFS drive scan"] = [
            (*case, MENU_EVIDENCE if case[0] == "offer-action" else None, None)
            for case in fast_ntfs_cases()
        ]

    workflows["Desktop observations"] = [
        (case, label, evidence["manual"], Path(evidence["image"]).name, None, evidence, None)
        for case, label, evidence in DESKTOP_CAPTURES[platform]
    ]
    rows = []
    for workflow, cases in workflows.items():
        for case, label, source, anchor, test, manual, harness in cases:
            lines = (root / source).read_text(encoding="utf-8").splitlines()
            if source.endswith(".md"):
                locations = [n for n, line in enumerate(lines, 1) if f"images/{anchor}" in line]
            else:
                locations = [n for n, line in enumerate(lines, 1) if line.strip() == anchor]
            if manual and not (root / manual["image"]).is_file():
                raise ValueError(f"Missing manual capture for {case}: {manual['image']}")
            if len(locations) != 1:
                raise ValueError(f"Review the source anchor for {case}: expected one match in {source}")
            line = locations[0]
            if harness and ((*harness, test) in integration_passed or
                            (None, harness[1], test) in integration_passed):
                status = "CLI integration test passed" if harness[0] == "kosha-cli" else "Scanner integration test passed"
            elif not harness and test in passed:
                status = "Component test passed"
            elif test:
                status = "Mapped test did not pass in this run"
            elif manual:
                status = "Documented manual observation"
            else:
                status = "Needs end-to-end evidence"
            rows.append({"id": case, "workflow": workflow, "scenario": label, "source": source,
                         "line": line, "line_hits": files.get(source, {}).get(line),
                         "test": test, "status": status, "manual_evidence": manual})
    status_counts = dict(Counter(row["status"] for row in rows))
    data = {"schema": "kosha.scenarios.v2", "platform": platform, "commit": commit,
            "repository": repository, "status_counts": status_counts, "scenarios": rows}
    (output / "scenarios.json").write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    sections = []
    for workflow in workflows:
        entries = []
        for row in (row for row in rows if row["workflow"] == workflow):
            source = f"https://github.com/{repository}/blob/{commit}/{row['source']}#L{row['line']}"
            hits = row["line_hits"]
            if hits is None:
                hits = ("Manual reference" if row["source"].endswith(".md") else
                        "Excluded test source" if "/tests/" in row["source"] else "Unmapped")
            evidence = html.escape(row["status"])
            if row["test"]:
                evidence += f"<br><code>{html.escape(row['test'])}</code>"
            if manual := row["manual_evidence"]:
                evidence += f"<br><a href='{manual['url']}'>{html.escape(manual['description'])}</a>"
                evidence += (f"<br><a href='https://github.com/{repository}/blob/"
                             f"{manual['published_commit']}/{manual['image']}'>Original capture</a>")
            entries.append(f"<tr><th scope='row'>{html.escape(row['scenario'])}</th>"
                           f"<td>{evidence}</td><td>{hits} &middot; <a href='{source}'>Source</a></td></tr>")
        sections.append(f'<h2>{html.escape(workflow)}</h2><table><thead><tr><th scope="col">Scenario</th>'
                        '<th scope="col">Test evidence</th><th scope="col">Line hits and source</th></tr>'
                        '<tbody>' + "".join(entries) + '</tbody></table>')
    title = f"{PLATFORMS[platform]} scenario coverage"
    counts = "".join(f"<li>{html.escape(status)}: {count}</li>" for status, count in status_counts.items())
    page = f"""<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>{PLATFORMS[platform]} source coverage | Scenarios</title>
<style>body{{font:16px/1.55 system-ui;max-width:70rem;margin:2rem auto;padding:0 1rem;color:#182c2b}}
table{{border-collapse:collapse;width:100%}}td,th{{padding:.65rem;text-align:left;border-bottom:1px solid #cedbd6}}
code{{overflow-wrap:anywhere}}a{{color:#176553}}@media(max-width:40rem){{td,th{{padding:.35rem}}}}</style>
<nav><a href="../">All platforms</a> &middot; <a href="./">{PLATFORMS[platform]} source coverage</a></nav>
<h1>{title}</h1>
<p>CLI integration tests invoke Kosha and assert its output or exit status. Component tests verify narrower behaviors.
LLVM line hits count execution across the run. Manual observations describe the linked capture and may predate this commit.</p>
<p>{len(rows)} tracked scenarios</p><ul>{counts}</ul>
"""
    (output / "scenarios.html").write_text(page + "".join(sections) +
        '<p><a href="scenarios.json">Download scenario evidence and source commit</a></p></html>',
        encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--commit", default=os.environ.get("GITHUB_SHA"), required=not bool(os.environ.get("GITHUB_SHA")))
    parser.add_argument("--repository", default="coolnew/kosha")
    parser.add_argument("--platform", choices=["linux", "windows", "macos"], required=True)
    parser.add_argument("--scope", choices=["workspace", "smoke"], required=True)
    parser.add_argument("--scenarios", action="store_true", help="Audit platform scenarios from LCOV and Rust test results")
    parser.add_argument("--test-log", type=Path, help="Captured output from the same instrumented Rust test run")
    args = parser.parse_args()
    if args.scenarios:
        if args.scope != "workspace" or args.test_log is None:
            parser.error("Scenario coverage requires workspace LCOV and --test-log")
        try:
            scenario_report(Path(__file__).resolve().parent, args.input.read_text(encoding="utf-8"),
                            args.test_log.read_text(encoding="utf-8"), args.output, args.commit, args.repository, args.platform)
        except (KeyError, TypeError, ValueError, OSError) as error:
            parser.exit(1, f"Scenario coverage refused: {error}\n")
        return
    if args.input.stat().st_size > 32 * 1024 * 1024:
        parser.error("Use an LLVM --summary-only JSON export smaller than 32 MiB")
    try:
        data, summary = render(json.loads(args.input.read_text(encoding="utf-8")), args.output,
                               args.commit, args.repository, args.platform, args.scope)
    except (KeyError, TypeError, ValueError, OSError) as error:
        parser.exit(1, f"Coverage report refused: {error}\n")
    if path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(path, "a", encoding="utf-8", newline="\n") as out:
            out.write(summary)
    print(json.dumps(data))


if __name__ == "__main__":
    main()
