#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Generate llms.txt from LLMS.md; --check rejects stale output. Edit LLMS.md only."""

import argparse
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def generated(root: Path) -> bytes:
    return (root / "LLMS.md").read_bytes().replace(b"\r\n", b"\n").rstrip() + b"\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    expected = generated(ROOT)
    destination = ROOT / "llms.txt"
    if args.check:
        if not destination.exists() or destination.read_bytes() != expected:
            print("llms.txt is stale; run python xtask-llms.py after editing LLMS.md")
            return 1
        print("llms.txt matches LLMS.md")
    else:
        destination.write_bytes(expected)
        print("Generated llms.txt from LLMS.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
