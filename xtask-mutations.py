#!/usr/bin/env python3
"""Check that only `kosha-fsops` can change a file the user has.

    python xtask-mutations.py

The dependency graph is part of the safety argument: "could this
code path delete a file?" is meant to be answerable by reading `Cargo.toml`,
because nothing outside `kosha-fsops` links a filesystem-mutating API. That was
a claim nothing checked, and it was false — `kosha_core::set_compression` was
public, needed no `ReadWriteGrant`, opened a file `GENERIC_READ|GENERIC_WRITE`
and issued `FSCTL_SET_COMPRESSION`. Nothing was calling it that way; nothing
stopped another caller either, which is the whole of what the ADR buys.

So this is the check that was missing. A guarantee nobody verifies is a comment.

**What counts as a mutation here.** Removing, renaming, truncating, changing
permissions, moving to the trash, or setting a filesystem attribute — acts on a
file that already exists and that somebody else made. Creating a new file is
*not* in the list: an export written where the user asked for one, a snapshot,
a settings file. Those are outputs, and refusing them would be refusing the
program's own reason to run.

`kosha-import` is in the non-mutating majority on purpose: its parsers read one
reader and write nothing. The only write an import causes is the new record the
command line saves through `snapshot/store.rs`, the exception below.

**The exceptions are listed one at a time, with why.** That is the point rather
than a weakness: such a dependency must be justified explicitly,
and an allow-list entry is where the argument goes.
"""
import pathlib
import sys

# Each of these acts on a file that already exists.
MUTATIONS = [
    "fs::remove_file(",
    "fs::remove_dir(",
    "fs::remove_dir_all(",
    "fs::rename(",
    "set_len(",
    "set_permissions(",
    "trash::",
    # Windows, where a mutation can also be an ioctl or an access mode.
    "FSCTL_SET_",
    "GENERIC_WRITE",
    "DeleteFileW",
    "MoveFileW",
    "SHFileOperation",
]

# The crate that is allowed to do all of it, and the reason it exists.
ALLOWED_CRATE = "kosha-fsops"

# Everywhere else, one entry per file, with the argument for it.
EXCEPTIONS = {
    "crates/kosha-marketplace/src/lib.rs": (
        "An explicit uninstall removes only a managed pack at a validated ID "
        "under Kosha's own configuration directory. Personal files and the "
        "original import are outside that store."
    ),
    "crates/kosha-core/src/snapshot/store.rs": (
        "kosha's own snapshots, under its own data directory. Written to a "
        "temporary name and renamed over the old one, which is what makes a "
        "half-written snapshot impossible; the removals are that temporary "
        "file and a snapshot the user asked to forget."
    ),
    "crates/kosha-gui/src/settings.rs": (
        "kosha's own settings file, renamed over itself for the same reason. "
        "A settings file truncated by a crash is a window that opens wrong "
        "forever after."
    ),
    "crates/kosha-core/src/export.rs": (
        "The export the user asked for, at the path they named. Writing it "
        "was never in question — `--out` is an instruction to write that "
        "file, and this crate has always done it — and what is new is only "
        "*how*: a temporary beside it, renamed into place, so an export that "
        "did not finish cannot replace a baseline with a shorter file that "
        "still parses. Measured: killed four and a half seconds into a "
        "228,058-entry export, the seven-row baseline that had been there was "
        "gone and 15,487 rows of valid CSV sat in its place. The rename "
        "replaces exactly the path that was asked for and the removal is the "
        "temporary this function made; neither can reach anything the caller "
        "did not name."
    ),
}


def production(path: pathlib.Path):
    """The file's lines, with any `#[cfg(test)] mod …` block left out.

    A test may do what it likes to a temporary directory it made. What this is
    about is the code that ships.
    """
    lines = path.read_text(encoding="utf-8", errors="replace").split("\n")
    out, skipping = [], False
    for number, line in enumerate(lines, start=1):
        if not skipping and line.strip() == "#[cfg(test)]":
            following = lines[number] if number < len(lines) else ""
            if following.startswith("mod "):
                skipping = True
                continue
        if skipping:
            # A module opened at column zero closes at column zero.
            if line == "}":
                skipping = False
            continue
        out.append((number, line))
    return out


def main() -> int:
    unexplained = []
    explained = 0

    for crate in sorted(pathlib.Path("crates").iterdir()):
        if not (crate / "src").is_dir():
            continue
        for path in sorted((crate / "src").rglob("*.rs")):
            # A file *named* for tests is all tests; the fixture builders in
            # them write images and trees by the thousand.
            if path.name == "tests.rs":
                continue
            posix = path.as_posix()
            for number, line in production(path):
                stripped = line.strip()
                if stripped.startswith("//"):
                    continue
                for word in MUTATIONS:
                    if word not in line:
                        continue
                    if crate.name == ALLOWED_CRATE:
                        explained += 1
                    elif posix in EXCEPTIONS:
                        explained += 1
                    else:
                        unexplained.append((posix, number, word, stripped[:70]))

    if unexplained:
        print("A crate that is not kosha-fsops can change a file:\n")
        for posix, number, word, text in unexplained:
            print(f"  {posix}:{number}")
            print(f"      {word}  {text}")
        print()
        print("This is a failure and not a note: the dependency")
        print("graph is part of the safety argument, and a mutation outside")
        print("kosha-fsops needs a ReadWriteGrant that it cannot name.")
        print()
        print("Move it, or add the file to EXCEPTIONS here with the argument for")
        print("it — which is a thing a reviewer should stop on.")
        return 1

    print(f"Only {ALLOWED_CRATE} mutates, plus {len(EXCEPTIONS)} argued exceptions.")
    print(f"{explained} call sites, all accounted for.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
