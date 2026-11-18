#!/usr/bin/env python3
"""Add or verify the SPDX header on every source file.

Run `python xtask-spdx.py --check` in CI; run it with no arguments to fix.
Kept as a script rather than a crate so it needs no build to run.
"""
import sys
import pathlib

HEADER = [
    "// SPDX-License-Identifier: MIT OR Apache-2.0",
    "// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors",
]
SKIP_DIRS = {"target", ".git", "node_modules"}

def sources():
    for path in pathlib.Path(".").rglob("*.rs"):
        if SKIP_DIRS & set(path.parts):
            continue
        yield path

def main() -> int:
    check = "--check" in sys.argv
    missing = []
    for path in sources():
        text = path.read_text(encoding="utf-8")
        if text.startswith(HEADER[0]):
            continue
        missing.append(path)
        if not check:
            path.write_text(
                "\n".join(HEADER) + "\n\n" + text, encoding="utf-8", newline="\n"
            )
    if check and missing:
        print("Missing SPDX header:")
        for path in missing:
            print(f"  {path.as_posix()}")
        print("\nRun: python xtask-spdx.py")
        return 1
    if not check:
        print(f"Added header to {len(missing)} file(s).")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
