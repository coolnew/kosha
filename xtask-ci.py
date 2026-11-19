#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Conservative CI selection. Unknown paths or unavailable history run everything."""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess

ROOT = Path(__file__).resolve().parent
REFERENCE_DOCS = {
    "docs/manual/query-language.md", "docs/manual/search-recipes.md",
    "docs/manual/recipe-authoring.md",
}
PROSE_ROOTS = {
    "README.md", "CHANGELOG.md", "CONTRIBUTING.md", "RELEASING.md",
    "SECURITY.md", "TRADEMARKS.md", "LLMS.md", "llms.txt", "DCO",
    "LICENSE-MIT", "LICENSE-APACHE", "docs/README.md", "assets/README.md",
}
PYTHON_ONLY = {
    "xtask-dco.py", "xtask-eol.py", "xtask-spdx.py", "xtask-gaps.py",
    "xtask-mutations.py", "xtask-version.py", "xtask-package.py",
    "xtask-release.py", "xtask-llms.py", "xtask-marketplace.py",
    "test_xtask_version.py", "test_xtask_llms.py", "test_xtask_release.py",
    "test_xtask_marketplace.py",
    "xtask-website.py", "test_xtask_website.py",
}


def classify(paths, force=False, reason="changed paths"):
    full, reference = force, False
    for name in paths:
        p = PurePosixPath(name)
        if name in REFERENCE_DOCS:
            reference = True
        elif name in PROSE_ROOTS or name in PYTHON_ONLY:
            continue
        elif name.startswith("docs/manual/") and p.suffix == ".md":
            continue
        elif name.startswith("docs/manual/images/") and p.suffix == ".png":
            continue
        elif name in {"website/index.md", "website/site.css", "website/mkdocs.yml", "website/requirements.txt"}:
            continue
        else:
            full = True
    return {"full": full, "reference": reference and not full,
            "coverage": full or reference, "changed_count": len(paths), "reason": reason}


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL)


def selection(root, event_name, event):
    try:
        if event_name == "pull_request":
            base, head = event["pull_request"]["base"]["sha"], event["pull_request"]["head"]["sha"]
        elif event_name == "push":
            base, head = event["before"], event["after"]
        else:
            return classify([], True, "manual, scheduled or merge-queue full validation")
        if any(not isinstance(s, str) or not re.fullmatch(r"[0-9a-fA-F]{40,64}", s)
               or not s.strip("0") for s in (base, head)):
            raise ValueError("missing comparison revision")
        if event_name == "pull_request":
            base = git(root, "merge-base", base, head).decode().strip()
        raw = git(root, "diff", "--name-only", "--no-renames", "-z", base, head, "--")
        paths = [p.decode("utf-8", "surrogateescape") for p in raw.split(b"\0") if p]
        return classify(paths)
    except (KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError):
        return classify([], True, "comparison unavailable: full validation")


def require_selected(jobs):
    outputs = jobs.get("changes", {}).get("outputs", {})
    if any(outputs.get(k) not in ("true", "false") for k in ("full", "reference")):
        raise ValueError("CI selection failed or omitted outputs")
    required = ["changes", "hygiene"]
    if outputs["full"] == "true":
        required += ["test", "musl"]
    elif outputs["reference"] == "true":
        required += ["reference"]
    failed = [name for name in required if jobs.get(name, {}).get("result") != "success"]
    if failed:
        raise ValueError("Selected checks did not pass: " + ", ".join(failed))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", "workflow_dispatch"))
    parser.add_argument("--event-file", type=Path, default=os.environ.get("GITHUB_EVENT_PATH"))
    parser.add_argument("--gate", action="store_true", help="Require selected job results from RESULTS JSON")
    args = parser.parse_args()
    if args.gate:
        try:
            require_selected(json.loads(os.environ.get("RESULTS", "{}")))
        except (ValueError, TypeError, AttributeError) as error:
            parser.exit(1, f"CI gate failed: {error}\n")
        print("All selected checks passed; expensive unselected jobs were intentionally skipped.")
        return
    try:
        event = json.loads(args.event_file.read_text(encoding="utf-8")) if args.event_file else {}
    except (OSError, ValueError):
        event = {}
    result = selection(ROOT, args.event, event)
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8", newline="\n") as out:
            for key in ("full", "reference", "coverage"):
                out.write(f"{key}={str(result[key]).lower()}\n")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8", newline="\n") as out:
            out.write("## CI selection\n\n" + "\n".join(
                f"- {key}: `{value}`" for key, value in result.items()) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
