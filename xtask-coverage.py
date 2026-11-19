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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--commit", default=os.environ.get("GITHUB_SHA"), required=not bool(os.environ.get("GITHUB_SHA")))
    parser.add_argument("--repository", default="coolnew/kosha")
    parser.add_argument("--platform", choices=["linux", "windows", "macos"], required=True)
    parser.add_argument("--scope", choices=["workspace", "smoke"], required=True)
    args = parser.parse_args()
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
