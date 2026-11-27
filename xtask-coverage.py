#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Render measured LLVM coverage totals, provenance, an offline landing page and badge."""

import argparse
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


def scenario_report(root, lcov, test_log, output, commit, repository):
    summary = json.loads((output / "summary.json").read_text(encoding="utf-8"))
    if (summary["platform"], summary["commit"], summary["repository"]) != ("windows", commit, repository) or (
            summary["schema"] != "kosha.coverage.v1" or not summary["scope"].startswith("Rust workspace;")):
        raise ValueError("Scenario evidence needs the matching Windows measurement")
    passed = set(re.findall(r"^test ([\w:]+) [.]{3} ok\s*$", test_log, re.MULTILINE))
    if not passed:
        raise ValueError("Expected successful named tests from Rust's test harness")
    files = lcov_lines(lcov)
    rows = []
    for case, label, source, anchor, test in fast_ntfs_cases():
        lines = (root / source).read_text(encoding="utf-8").splitlines()
        locations = [n for n, line in enumerate(lines, 1) if line.strip() == anchor]
        if len(locations) != 1:
            raise ValueError(f"Review the source anchor for {case}: expected one match in {source}")
        line = locations[0]
        rows.append({"id": case, "scenario": label, "source": source, "line": line,
                     "line_hits": files.get(source, {}).get(line), "test": test,
                     "status": ("Component test passed" if test in passed else
                                "Mapped test did not pass in this run" if test else
                                "Needs end-to-end evidence")})
    data = {"schema": "kosha.scenarios.v1", "platform": "windows", "commit": commit,
            "repository": repository,
            "workflow": "Fast NTFS drive scan", "scenarios": rows}
    (output / "scenarios.json").write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    entries = []
    for row in rows:
        source = f"https://github.com/{repository}/blob/{commit}/{row['source']}#L{row['line']}"
        hits = row["line_hits"] if row["line_hits"] is not None else "Unmapped"
        entries.append(f"<tr><th scope='row'>{html.escape(row['scenario'])}</th>"
                       f"<td>{row['status']}<br><code>{html.escape(row['test'] or '')}</code></td>"
                       f"<td><a href='{source}'>{hits}</a></td></tr>")
    page = """<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Fast NTFS scenario coverage</title>
<style>body{font:16px/1.55 system-ui;max-width:70rem;margin:2rem auto;padding:0 1rem;color:#182c2b}
table{border-collapse:collapse;width:100%}td,th{padding:.65rem;text-align:left;border-bottom:1px solid #cedbd6}
code{overflow-wrap:anywhere}a{color:#176553}@media(max-width:40rem){td,th{padding:.35rem}}</style>
<nav><a href="../">All platforms</a> &middot; <a href="./">Windows source coverage</a></nav>
<h1>Fast NTFS scenario coverage</h1>
<p>Each row describes an expected Windows behavior. A passing component test verifies the named
assertion. LLVM line hits show execution across the full test run and cannot attribute it to that
test or show that a desktop action and its result were verified.</p>
<p>Hosted coverage uses Rust tests. Real UAC prompts, drive menu interactions and snapshot
preservation require separate desktop acceptance evidence.</p>
<table><thead><tr><th scope="col">Scenario</th><th scope="col">Test evidence</th>
<th scope="col">Source line hits</th></tr></thead><tbody>"""
    (output / "scenarios.html").write_text(page + "".join(entries) + '</tbody></table>'
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
    parser.add_argument("--scenarios", action="store_true", help="Audit Fast NTFS scenarios from LCOV and Rust test results")
    parser.add_argument("--test-log", type=Path, help="Captured output from the same instrumented Rust test run")
    args = parser.parse_args()
    if args.scenarios:
        if args.platform != "windows" or args.scope != "workspace" or args.test_log is None:
            parser.error("Scenario coverage requires Windows workspace LCOV and --test-log")
        try:
            scenario_report(Path(__file__).resolve().parent, args.input.read_text(encoding="utf-8"),
                            args.test_log.read_text(encoding="utf-8"), args.output, args.commit, args.repository)
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
