#!/usr/bin/env python3
"""Verify that tracked text files use the line endings .gitattributes promises.

Shell one-liners for this are unreliable: `grep $'\\r$'` behaves differently
across grep builds and can silently match every line, which makes the check pass
when it should fail and fail when it should pass. A few lines of Python is
deterministic everywhere, and matches how the SPDX check already works.

Run with no arguments; exits non-zero and names the offending files.
"""
import subprocess
import sys

# The only files that legitimately need CRLF: Windows shells refuse to run
# scripts without it.
CRLF_SUFFIXES = (".bat", ".cmd", ".ps1")


def tracked_files():
    out = subprocess.run(
        ["git", "ls-files", "-z"],
        capture_output=True,
        check=True,
    ).stdout
    return [name.decode("utf-8", "surrogateescape") for name in out.split(b"\0") if name]


def main() -> int:
    wrong_crlf = []
    wrong_lf = []

    for name in tracked_files():
        try:
            data = open(name, "rb").read()
        except OSError:
            continue
        # Skip binary files the same way git does: a NUL byte means binary.
        if b"\0" in data:
            continue

        crlf = data.count(b"\r\n")
        bare_lf = data.count(b"\n") - crlf

        if name.endswith(CRLF_SUFFIXES):
            if bare_lf:
                wrong_lf.append(name)
        elif crlf:
            wrong_crlf.append(name)

    for name in wrong_crlf:
        print(f"CRLF in an LF-only file: {name}")
    for name in wrong_lf:
        print(f"LF in a file that needs CRLF to run: {name}")

    if wrong_crlf or wrong_lf:
        print()
        print("Tracked text uses LF, except Windows shell scripts; see .gitattributes.")
        print("Fix with:  git add --renormalize . && git checkout -- .")
        return 1

    print("Line endings are correct.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
