#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Build the public manual website; compose measured coverage without rescanning/testing Rust."""

import argparse
import importlib.util
from html.parser import HTMLParser
import json
import posixpath
from pathlib import Path
import re
import shutil
import subprocess
import sys
from urllib.parse import quote, unquote, urlsplit

ROOT = Path(__file__).resolve().parent
REPOSITORY = "https://github.com/coolnew/kosha"


def destination(name):
    if name == "PRIVACY.md":
        return "privacy.md"
    if name == "website/index.md":
        return "index.md"
    if name.startswith("docs/manual/"):
        return "manual/" + ("index.md" if name.endswith("/README.md") else name.removeprefix("docs/manual/"))
    return None


def rewrite_links(text, source, files):
    """Keep source Markdown portable; translate only rendered inline links outside fences."""
    def replace(match):
        target = match[2].strip("<>")
        url = urlsplit(target)
        if url.scheme or url.netloc or not url.path:
            return match[0]
        resolved = posixpath.normpath(posixpath.join(posixpath.dirname(source), unquote(url.path)))
        if resolved not in files:
            raise ValueError(f"Unreviewed website link: {source} -> {target}")
        staged = destination(resolved)
        path = (posixpath.relpath(staged, posixpath.dirname(destination(source)) or ".") if staged
                else REPOSITORY + "/blob/main/" + quote(resolved, safe="/"))
        path += ("?" + url.query if url.query else "") + ("#" + url.fragment if url.fragment else "")
        return match[1] + path + match[3]
    parts = re.split(r"(?m)(^```[^\n]*\n.*?^```[^\n]*$)", text, flags=re.S)
    return "".join(part if i % 2 else re.sub(
        r'(!?\[[^\]]*\]\(\s*)(<[^>]+>|[^\s)]+)(\s*(?:"[^"]*")?\s*\))', replace, part)
        for i, part in enumerate(parts))


def stage(files, docs):
    for name, data in files.items():
        dest = destination(name)
        if dest is None:
            continue
        # Only manual Markdown/reviewed PNGs, the homepage and public privacy policy.
        if not name.endswith((".md", ".png")):
            raise ValueError(f"Unexpected website payload: {name}")
        path = docs / dest
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(rewrite_links(data.decode("utf-8"), name, files).encode("utf-8")
                         if name.endswith(".md") else data)
    (docs / "styles").mkdir(parents=True, exist_ok=True)
    (docs / "styles/site.css").write_bytes(files["website/site.css"])
    (docs / "llms.txt").write_bytes(files["llms.txt"])


def platform_coverage(directory, platform, expected_sha):
    data = json.loads((directory / "summary.json").read_text(encoding="utf-8"))
    if (data.get("schema") != "kosha.coverage.v1" or data.get("repository") != "coolnew/kosha"
            or data.get("platform") != platform or not re.fullmatch(r"[0-9a-f]{40}", data.get("commit", ""))
            or not data.get("scope", "").startswith("Rust workspace;")
            or (expected_sha and data["commit"] != expected_sha)):
        raise ValueError(f"Coverage provenance does not match the selected {platform} workspace artifact")
    if not (directory / "coverage/html/index.html").is_file():
        raise ValueError("Coverage artifact is missing annotated HTML")
    return data


def compose_coverage(source, site, expected_sha=None):
    target = site / "coverage"
    target.mkdir(parents=True, exist_ok=True)
    nav = '<nav style="padding:1rem"><a href="../">Kosha home</a> · <a href="../manual/quality-and-coverage/">Tests and coverage scope</a></nav>'
    if source is None:
        (target / "index.html").write_text('<!doctype html><html lang="en"><meta charset="utf-8"><title>Kosha coverage</title>'
            + nav + '<h1>Coverage report unavailable</h1><p>No retained successful coverage artifact was found. '
            'This does not mean zero coverage. See <a href="https://github.com/coolnew/kosha/actions/workflows/coverage.yml">Actions reports</a>.</p></html>', encoding="utf-8")
        return
    # Never follow links out of downloaded artifacts or copy executable files.
    paths = list(source.rglob("*"))
    for path in paths:
        if path.is_symlink() or getattr(path, "is_junction", lambda: False)() or not path.resolve().is_relative_to(source.resolve()):
            raise ValueError("Unsafe coverage artifact path")
        if path.is_file() and path.suffix not in {".html", ".css", ".js", ".json", ".svg", ".png"}:
            raise ValueError(f"Unexpected coverage artifact file: {path.name}")
    if sum(p.stat().st_size for p in paths if p.is_file()) > 512 * 1024 * 1024:
        raise ValueError("Coverage artifact exceeds website size budget")
    platforms = {"linux": "Linux", "windows": "Windows", "macos": "macOS"}
    matrix = not (source / "summary.json").exists()
    if matrix:
        reports = {key: platform_coverage(source / key, key, expected_sha) for key in platforms}
        commits = {report["commit"] for report in reports.values()}
        if len(commits) != 1:
            raise ValueError("Coverage must use the same source commit across platforms")
        commit = commits.pop()
        scenarios = json.loads((source / "windows/scenarios.json").read_text(encoding="utf-8"))
        if (scenarios.get("schema"), scenarios.get("commit"), scenarios.get("platform"),
                scenarios.get("repository")) != ("kosha.scenarios.v1", commit, "windows", "coolnew/kosha") or not (source / "windows/scenarios.html").is_file():
            raise ValueError("Scenario evidence does not match the Windows measurement")
        rows = []
        for key, label in platforms.items():
            metrics = reports[key]["metrics"]["lines"]
            covered, count = metrics["covered"], metrics["count"]
            if type(count) is not int or type(covered) is not int or not 0 <= covered <= count or not count:
                raise ValueError("Invalid platform coverage counters")
            rows.append(f'<tr><th scope="row"><a href="{key}/">{label}</a></th>'
                        f'<td>{covered:,} / {count:,}</td><td>{100 * covered / count:.2f}%</td></tr>')
        shutil.copytree(source, target, dirs_exist_ok=True)
        for key in platforms:
            page = target / key / "index.html"
            page.write_text(page.read_text(encoding="utf-8").replace("<h1>",
                '<nav><a href="../">All platforms</a></nav><h1>', 1), encoding="utf-8")
        page = f"""<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Kosha test coverage</title><style>body{{font:17px/1.6 system-ui;color:#182c2b;background:#f8f9f5;max-width:60rem;margin:2rem auto;padding:0 1rem}}
a{{color:#176553}}table{{width:100%;border-collapse:collapse}}td,th{{padding:.65rem;text-align:left;border-bottom:1px solid #cedbd6}}
code{{overflow-wrap:anywhere}}</style>
{nav}<main><h1>Kosha test coverage</h1>
<p>Rust workspace tests measured on three native runners using <a href="{REPOSITORY}/tree/{commit}"><code>{commit}</code></a>.</p>
<table><thead><tr><th scope="col">Platform</th><th scope="col">Lines executed / measured</th><th scope="col">Line coverage</th></tr></thead>
<tbody>{"".join(rows)}</tbody></table>
<p>Each platform has its own annotated source, measured totals and timestamp.</p>
<h2>Scenario coverage</h2><p><a href="windows/scenarios.html">Fast NTFS drive scan</a>
tracks the drive menu, scan result, fallback and cancellation against available test evidence.</p></main></html>"""
        (target / "index.html").write_text(page, encoding="utf-8")
        badge_source = target / "linux"
        annotated = "linux/coverage/html/index.html"
        redirect_coverage(target / "coverage/html/index.html", "../../" + annotated)
    else:
        platform_coverage(source, "linux", expected_sha)
        shutil.copytree(source, target, dirs_exist_ok=True)
        page = target / "index.html"
        page.write_text(page.read_text(encoding="utf-8").replace("<h1>", nav + "<h1>", 1), encoding="utf-8")
        badge_source = target
        annotated = "coverage/html/index.html"
    # Keep existing Linux badge, JSON and source entry points working.
    for name in ("coverage.svg", "summary.json"):
        if matrix:
            shutil.copyfile(badge_source / name, target / name)
        shutil.copyfile(badge_source / name, site / name)
    redirect_coverage(target / "html/index.html", "../" + annotated)


def redirect_coverage(path, url):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f'<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="0;url={url}">'
                    f'<a href="{url}">Open source coverage</a>', encoding="utf-8")


def validate_site(site):
    """Check generated local links/assets, including the relocated coverage tree."""
    class Links(HTMLParser):
        def __init__(self):
            super().__init__()
            self.targets = []

        def handle_starttag(self, tag, attrs):
            self.targets.extend(value for name, value in attrs if name in {"href", "src"} and value)

    root = site.resolve()
    broken = []
    for page in site.rglob("*.html"):
        parser = Links()
        parser.feed(page.read_text(encoding="utf-8"))
        for target in parser.targets:
            url = urlsplit(target)
            if url.scheme or url.netloc or not url.path:
                continue
            decoded = unquote(url.path)
            if decoded.startswith('/kosha/'):
                path = (site / decoded.removeprefix('/kosha/')).resolve()
            else:
                path = (page.parent / decoded).resolve()
            if not path.is_relative_to(root) or not path.exists():
                broken.append(f"{page.relative_to(site)}: {target}")
    if broken:
        raise ValueError("Broken generated website links:\n" + "\n".join(broken[:25]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--coverage", type=Path, help="Downloaded coverage artifact's site directory")
    parser.add_argument("--coverage-sha", help="Verify artifact provenance against its workflow source SHA")
    args = parser.parse_args()
    work = ROOT / "target/website"
    # Fixed scratch directory, checked before recursive cleanup on Windows.
    if work.is_symlink() or work.resolve() != ROOT / "target/website" or not work.resolve().is_relative_to(ROOT):
        parser.error("Unsafe website scratch directory")
    if work.exists():
        shutil.rmtree(work)
    spec = importlib.util.spec_from_file_location("public_package", ROOT / "xtask-package.py")
    package = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(package)
    stage(package.public_payloads(), work / "docs")
    subprocess.run([sys.executable, "-m", "mkdocs", "build", "--strict", "-f", str(ROOT / "website/mkdocs.yml")], check=True)
    compose_coverage(args.coverage, work / "site", args.coverage_sha)
    validate_site(work / "site")
    (work / "site/.nojekyll").write_text("", encoding="utf-8")
    print(f"Website built: {work / 'site/index.html'}")


if __name__ == "__main__":
    main()
