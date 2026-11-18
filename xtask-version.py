#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Validate workspace, local dependency, lockfile and optional release-tag versions."""

import argparse
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parent
SEMVER = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?")


def validate(root: Path, tag: str | None = None) -> str:
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    version = workspace["package"]["version"]
    if not SEMVER.fullmatch(version):
        raise ValueError(f"Invalid workspace version: {version}")
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"Release tag {tag!r} must equal v{version}")
    packages = {}
    for member in workspace["members"]:
        package = tomllib.loads((root / member / "Cargo.toml").read_text(encoding="utf-8"))["package"]
        declared = package["version"]
        if declared != {"workspace": True} and declared != version:
            raise ValueError(f"{member}: package version does not match {version}")
        packages[package["name"]] = version
    for name, dep in workspace.get("dependencies", {}).items():
        if isinstance(dep, dict) and "path" in dep and dep.get("version") != version:
            raise ValueError(f"Local dependency {name}: expected version {version}")
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    locked = {p["name"]: p["version"] for p in lock["package"] if "source" not in p}
    for name, expected in packages.items():
        if locked.get(name) != expected:
            raise ValueError(f"Cargo.lock {name}: expected version {expected}; rebuild the lockfile")
    return version


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="Require this exact vX.Y.Z tag to match Cargo")
    args = parser.parse_args()
    try:
        print(f"Versions agree: {validate(ROOT, args.tag)}" + (f"; tag {args.tag}" if args.tag else ""))
    except (ValueError, KeyError, OSError) as error:
        print(f"Version check failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
