#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Check inline Markdown file/image links in the reviewed public source (offline)."""

import importlib.util
from pathlib import Path
import posixpath
import re
import subprocess
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent


def broken_links(files, extra_paths=()):
    allowed = set(files) | set(extra_paths)
    broken = []
    for name, data in files.items():
        if not name.endswith(".md"):
            continue
        text = data.decode("utf-8")
        text = re.sub(r"(?ms)^```[^\n]*\n.*?^```[^\n]*$", "", text)
        for match in re.finditer(r'!?\[[^\]]*\]\(\s*(<[^>]+>|[^\s)]+)(?:\s+"[^"]*")?\s*\)', text):
            target = match[1].strip("<>")
            url = urlsplit(target)
            if url.scheme or url.netloc or not url.path:
                continue
            resolved = posixpath.normpath(posixpath.join(posixpath.dirname(name), unquote(url.path)))
            if resolved not in allowed and not any(p.startswith(resolved + "/") for p in allowed):
                broken.append((name, target))
    return broken


def main():
    spec = importlib.util.spec_from_file_location("public_package", ROOT / "xtask-package.py")
    package = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(package)
    files = package.public_payloads()
    names = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
    problems = broken_links(files, [n for n in names if n.startswith("marketplace/")])
    for source, target in problems:
        print(f"{source}: missing public link target {target}")
    if not problems:
        print("Public Markdown file/image links passed (anchors and external URLs are not checked).")
    return bool(problems)


if __name__ == "__main__":
    raise SystemExit(main())
