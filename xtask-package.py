#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Build the release archive for this machine's platform, and prove it works.

`.github/workflows/release.yml` does this on three runners for a tag. This does
the same thing locally, for the one platform you are sitting at, and then does
the part a workflow cannot: it unpacks the archive somewhere with no build tree
in sight and runs what came out.

That last step is the whole point. The first time it ran it found that
`kosha … --export csv | head` panicked with a Rust backtrace and exited 101,
which is what the most ordinary shape of a command-line invocation did to the
binary whose argument is that you can script it. Nothing else had noticed,
because nothing else pipes the output of a release build.

    python xtask-package.py                 # read-only package plan
    python xtask-package.py --execute       # package existing release binaries
    python xtask-release.py --execute       # validate, build and package

Never a substitute for running the workflow. It cannot prove the matrix builds,
that the macOS bundle launches, or that a draft release appears — see T-06.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
# Ignored by git. A `git add -A` took 26 MB of an earlier rehearsal into a
# commit, which is why this says so here as well as in `.gitignore`.
DIST = ROOT / "dist"

WINDOWS = os.name == "nt"
MACOS = sys.platform == "darwin"


def git_only_data(name: str) -> bool:
    return name.startswith("marketplace/")


def public_payloads(root: Path | None = None) -> dict[str, bytes]:
    """Resolve reviewed public bytes, never package the private docs tree."""
    root = (root or ROOT).resolve()
    manifest = root / "publication/files.jsonl"
    if manifest.exists():
        spec = importlib.util.spec_from_file_location("kosha_public_export", root / "xtask-public.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        files, _ = module.payloads(root, manifest, module.tracked(root))
        module.links(files)
        return {name: data for name, data in files.items() if not git_only_data(name)}
    # A public checkout deliberately contains no private export manifest. Only
    # reviewed source/build/manual categories are admissible here; unfamiliar
    # tracked files require a policy update rather than a recursive copy.
    names = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().strip("\0").split("\0")
    roots = {".gitattributes", ".gitignore", "Cargo.toml", "Cargo.lock", "DCO", "LICENSE-MIT", "LICENSE-APACHE", "README.md", "CHANGELOG.md", "CONTRIBUTING.md", "RELEASING.md", "SECURITY.md", "TRADEMARKS.md", "LLMS.md", "llms.txt", "clippy.toml", "rust-toolchain.toml", "rustfmt.toml", "xtask-dco.py", "xtask-eol.py", "xtask-spdx.py", "xtask-gaps.py", "xtask-mutations.py", "xtask-version.py", "xtask-package.py", "xtask-release.py", "xtask-llms.py", "test_xtask_version.py", "test_xtask_llms.py", "test_xtask_release.py"}
    roots.update({"xtask-marketplace.py", "test_xtask_marketplace.py"})
    roots.update({"xtask-ci.py", "test_xtask_ci.py", "xtask-coverage.py",
                  "test_xtask_coverage.py", "xtask-docs.py", "test_xtask_docs.py",
                  "xtask-website.py", "test_xtask_website.py"})
    files = {}
    for name in names:
        if not name:
            continue
        if git_only_data(name):
            if any(part.lower().startswith(".env") or part.lower() in {".git", ".claude", ".codex", ".agents"} for part in Path(name).parts):
                raise ValueError(f"Private file in Git-only data inventory: {name}")
            continue
        if name not in roots and name not in {"docs/README.md", "scripts/test-windows-gui-console.py", "scripts/check-recipe-authoring-manual.py"} and not name.startswith(("crates/", "assets/", "docs/manual/", "website/", ".github/workflows/")):
            raise ValueError(f"Unclassified public-checkout file: {name}")
        path = root / name
        if path.is_symlink() or getattr(path, "is_junction", lambda: False)() or not path.resolve().is_relative_to(root):
            raise ValueError(f"Unsafe public source path: {name}")
        if any(part.lower().startswith(".env") or part.lower() in {".git", ".claude", ".codex", ".agents"} for part in Path(name).parts):
            raise ValueError(f"Private file in public source inventory: {name}")
        data = path.read_bytes()
        if re.search(rb"-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----|\b(?:github_pat_[A-Za-z0-9_]{40,}|gh[pousr]_[A-Za-z0-9]{30,}|AKIA[A-Z0-9]{16})\b", data):
            raise ValueError(f"Potential credential in public source: {name}")
        files[name] = data
    return files


def documentation_payloads(root: Path | None = None) -> dict[str, bytes]:
    files = public_payloads(root)
    roots = {"README.md", "CHANGELOG.md", "CONTRIBUTING.md", "RELEASING.md", "SECURITY.md", "TRADEMARKS.md", "LLMS.md", "llms.txt", "DCO", "LICENSE-MIT", "LICENSE-APACHE"}
    return {name: data for name, data in files.items() if name in roots or name.startswith(("docs/", "assets/"))}


def target_triple() -> str:
    """The triple a local `cargo build --release` produced."""
    machine = platform.machine().lower()
    arch = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
    if WINDOWS:
        return f"{arch}-pc-windows-msvc"
    if MACOS:
        return f"{arch}-apple-darwin"
    return f"{arch}-unknown-linux-gnu"


def binaries() -> tuple[Path, Path]:
    """Where `cargo build --release` put the two front ends.

    `CARGO_TARGET_DIR` is honoured because cargo honours it, and building a
    tree that lives on a Windows drive from inside WSL is one of the places it
    earns its keep: the 9P redirector makes `target/` on `/mnt/j` slow enough
    that everybody sets it.
    """
    suffix = ".exe" if WINDOWS else ""
    target = os.environ.get("CARGO_TARGET_DIR")
    release = (Path(target).expanduser() if target else ROOT / "target") / "release"
    cli, gui = release / f"kosha{suffix}", release / f"kosha-gui{suffix}"
    for one in (cli, gui):
        if not one.is_file():
            sys.exit(
                f"{one} is not there.\n"
                "Build first: cargo build --release -p kosha-cli -p kosha-gui"
            )
    return cli, gui


def version() -> str:
    """What the binary says it is, which is what the archive is named for."""
    cli, _ = binaries()
    out = subprocess.run([str(cli), "--version"], capture_output=True, text=True)
    # `kosha 0.9.0 (abc1234, x86_64-pc-windows-msvc)`
    if out.returncode != 0:
        raise ValueError("The release CLI could not report its version")
    stamp = parse_stamp(out.stdout.strip())
    expected = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if stamp["version"] != expected:
        raise ValueError("The release CLI version disagrees with Cargo.toml; rebuild")
    return stamp["version"]


def parse_stamp(text: str) -> dict[str, str]:
    match = re.fullmatch(r"kosha (?P<version>\S+) \((?P<commit>[0-9a-f]{4,40})(?P<dirty>\+)?, (?P<target>[A-Za-z0-9_-]+)\)", text)
    if not match:
        raise ValueError("Missing or malformed kosha version/commit/target stamp")
    return match.groupdict(default="")


def stamp_problems(text: str, expected_version: str, head: str, target: str) -> list[str]:
    try:
        stamp = parse_stamp(text)
    except ValueError as error:
        return [str(error)]
    wrong = []
    if stamp["version"] != expected_version:
        wrong.append(f"binary version {stamp['version']} does not match {expected_version}")
    if stamp["dirty"]:
        wrong.append("built from a modified tree")
    if not re.fullmatch(r"[0-9a-f]{40}", head) or not head.startswith(stamp["commit"]):
        wrong.append("binary commit does not match the selected HEAD")
    if stamp["target"] != target:
        wrong.append(f"binary target {stamp['target']} does not match {target}")
    return wrong


def assemble(name: str) -> Path:
    """The same file list the workflow copies, for this platform."""
    expected_guide = (ROOT / "LLMS.md").read_bytes().replace(b"\r\n", b"\n").rstrip() + b"\n"
    if (ROOT / "llms.txt").read_bytes() != expected_guide:
        raise ValueError("llms.txt is stale; run python xtask-llms.py before packaging")
    out = DIST / name
    if out.exists():
        raise ValueError(f"Package output already exists; choose a fresh output directory: {out}")
    out.mkdir(parents=True)

    cli, gui = binaries()
    for name, data in documentation_payloads().items():
        target = out / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    shutil.copy2(ROOT / "assets" / "kosha-256.png", out / "kosha-256.png")

    if MACOS:
        # The bundle is what a Mac user drags into Applications; `kosha` stays
        # loose beside it, because a command-line tool inside one is a tool
        # nobody can type.
        app = out / "Kosha.app" / "Contents"
        (app / "MacOS").mkdir(parents=True)
        (app / "Resources").mkdir(parents=True)
        shutil.copy2(gui, app / "MacOS" / "kosha-gui")
        shutil.copy2(ROOT / "assets" / "kosha.icns", app / "Resources" / "kosha.icns")
        (app / "PkgInfo").write_text("APPL????")
        (app / "Info.plist").write_text(plist(version()))
        shutil.copy2(cli, out / cli.name)
    else:
        shutil.copy2(gui, out / gui.name)
        shutil.copy2(cli, out / cli.name)

    if not WINDOWS and not MACOS:
        # Without these the window comes up with a placeholder icon and no name
        # in the switcher.
        shutil.copy2(
            ROOT / "assets" / "app.coolnew.kosha.desktop",
            out / "app.coolnew.kosha.desktop",
        )
    return out


def plist(number: str) -> str:
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>kosha</string>
  <key>CFBundleDisplayName</key><string>kosha</string>
  <key>CFBundleExecutable</key><string>kosha-gui</string>
  <key>CFBundleIdentifier</key><string>app.coolnew.kosha</string>
  <key>CFBundleShortVersionString</key><string>{number}</string>
  <key>CFBundleVersion</key><string>{number}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>kosha.icns</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"""


def archive(folder: Path) -> Path:
    """Zip on Windows, tar.gz everywhere else — as the workflow does."""
    if WINDOWS:
        # Concatenated, never `with_suffix`: the folder name has dots in it —
        # `kosha-v0.9.0-x86_64-pc-windows-msvc` — and `with_suffix` replaces
        # everything after the last one, so the first archive this wrote was
        # called `kosha-v0.9.zip`.
        path = folder.parent / (folder.name + ".zip")
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
            for item in sorted(folder.rglob("*")):
                zf.write(item, item.relative_to(folder.parent))
        return path

    path = folder.parent / (folder.name + ".tar.gz")
    with tarfile.open(path, "w:gz") as tf:
        # `filter` keeps the mode, which is the point: a bundle and a binary
        # carry an executable bit that a zip would drop.
        tf.add(folder, arcname=folder.name)
    return path


def checksum(path: Path) -> str:
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    beside = Path(str(path) + ".sha256")
    beside.write_text(f"{digest}  {path.name}\n", encoding="utf-8", newline="\n")
    return digest


def remove_unpack_directory(path: Path) -> None:
    """Remove only the named disposable child, never an output root or source."""
    if path.name != "_unpacked" or path.resolve() == DIST.resolve() or not path.resolve().is_relative_to(DIST.resolve()):
        raise ValueError("Unsafe unpack cleanup path")
    for parent in (path, *path.parents):
        if parent.is_symlink() or getattr(parent, "is_junction", lambda: False)():
            raise ValueError("Unpack cleanup cannot traverse a symlink/junction")
    shutil.rmtree(path)


def verify(path: Path, folder_name: str) -> list:
    """Unpack it somewhere clean and run what came out.

    A binary that only works beside its own target directory is a binary
    nobody can download.

    Returns what is wrong with the archive, so that the last line this script
    prints cannot say "verified" over the top of a warning nobody read.
    """
    wrong = []
    sandbox = DIST / "_unpacked"
    if sandbox.exists():
        remove_unpack_directory(sandbox)
    sandbox.mkdir(parents=True)

    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as zf:
            zf.extractall(sandbox)
    else:
        with tarfile.open(path) as tf:
            tf.extractall(sandbox)

    unpacked = sandbox / folder_name
    # Verify the archive, not only the staging directory. A working scanner
    # with a README pointing at missing installation/manual files is incomplete.
    required_docs = [Path(p) for p in documentation_payloads()]
    for relative in required_docs:
        if not (unpacked / relative).is_file():
            wrong.append(f"missing packaged documentation: {relative.as_posix()}")
    suffix = ".exe" if WINDOWS else ""
    cli = unpacked / f"kosha{suffix}"
    if not WINDOWS:
        cli.chmod(0o755)

    stamp = subprocess.run(
        [str(cli), "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    print(f"  from the archive: {stamp}")
    # And the *right* commit, which the marker above does not cover. A binary
    # left over from an earlier commit is stamped with that commit and no marker
    # if the tree was clean when it was built, so the archive would claim, in
    # perfectly good faith, to be something it is not. This script does not
    # build — it packages what is in `target/release` — so a stale binary is the
    # ordinary outcome of committing after a build, and it happened here: an
    # archive stamped two commits behind HEAD, caught only because that build
    # also carried a `+`.
    head = subprocess.check_output(["git", "--no-optional-locks", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    expected_version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    for problem in stamp_problems(stamp, expected_version, head, target_triple()):
        print(f"  ! {problem}")
        wrong.append(problem)

    # And the window, which had been going unchecked. The front page promises
    # the two binaries answer with the same line, and this script packages what
    # is in `target/release` rather than building it — so an archive can carry a
    # CLI from HEAD beside a GUI from nine commits ago, each stamped honestly and
    # the pair of them a lie. That is what it was carrying when this was written.
    #
    # A windowed program can be asked: it answers before opening anything, and
    # `subprocess` waits for it where PowerShell would not.
    window = unpacked / f"kosha-gui{suffix}"
    if not window.exists():
        # macOS keeps it inside the bundle, with a loose copy of the CLI beside.
        window = unpacked / "Kosha.app" / "Contents" / "MacOS" / "kosha-gui"
    if window.exists():
        if not WINDOWS:
            window.chmod(0o755)
        theirs = subprocess.run(
            [str(window), "--version"], capture_output=True, text=True
        ).stdout.strip()
        if theirs != stamp:
            print(f"  ! the window says {theirs!r} and the scanner {stamp!r}")
            wrong.append("the two binaries were built from different commits")
        else:
            print("  both binaries answer with the same line")
    else:
        print("  ! no kosha-gui in the archive to check")
        wrong.append("no kosha-gui in the archive")

    # `--version` returns before anything is loaded, so this is the smoke test
    # that matters: a real scan, printed through a pipe that is then closed.
    scan = subprocess.run(
        [str(cli), str(ROOT / "crates"), "--export", "csv"],
        capture_output=True,
        text=True,
    )
    if scan.returncode != 0 or "panicked" in scan.stderr:
        print(f"  ! scanning from the archive failed ({scan.returncode})")
        print("   ", scan.stderr.strip().splitlines()[:3])
        sys.exit(1)
    rows = scan.stdout.count("\n")
    print(f"  a scan through a pipe: {rows} rows, exit {scan.returncode}")

    remove_unpack_directory(sandbox)
    return wrong


def main() -> None:
    global ROOT, DIST
    parser = argparse.ArgumentParser(description=__doc__)
    location = parser.add_mutually_exclusive_group()
    location.add_argument("--public", action="store_true", help="Package the separate public checkout")
    location.add_argument("--repo-path", type=Path, help="Package this explicit repository root")
    parser.add_argument("--execute", action="store_true", help="Package and verify; default is a dry-run")
    parser.add_argument("--output-dir", type=Path, help="Fresh output directory under selected checkout's dist/")
    parser.add_argument(
        "--keep", action="store_true", help="Compatibility option; artifacts are always retained now"
    )
    args = parser.parse_args()

    if args.repo_path:
        ROOT = args.repo_path.resolve()
    elif args.public:
        ROOT = ROOT if not (ROOT / "publication/files.jsonl").exists() else ROOT.parent / "github" / "kosha"
    ROOT = ROOT.resolve()
    DIST = args.output_dir or ROOT / "dist"
    if not DIST.is_absolute():
        DIST = ROOT / DIST
    if not DIST.resolve().is_relative_to((ROOT / "dist").resolve()):
        raise ValueError("Package output must stay under the selected checkout's dist/")
    for parent in (DIST, *DIST.parents):
        if parent.is_symlink() or getattr(parent, "is_junction", lambda: False)():
            raise ValueError("Package output cannot traverse symlinks/junctions")
    expected_version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    public_payloads()
    status = subprocess.check_output(["git", "--no-optional-locks", "status", "--porcelain=v1", "--untracked-files=all"], cwd=ROOT).decode()
    if not args.execute:
        print(json.dumps({"mode": "binary-package-dry-run", "version": expected_version,
                          "repository": str(ROOT), "output": str(DIST), "clean": not bool(status),
                          "git_mutations": [], "next": "Build clean release binaries, then add --execute; use xtask-release.py for the full pipeline"}, indent=2))
        return
    if status:
        raise ValueError("Package execution requires a clean worktree/index; review and commit changes first")

    number = version()
    name = f"kosha-v{number}-{target_triple()}"
    print(f"packaging {name}")

    folder = assemble(name)
    print("  contents:")
    for item in sorted(folder.iterdir()):
        size = item.stat().st_size if item.is_file() else 0
        print(f"    {item.name:<28} {size:>10,}")

    path = archive(folder)
    digest = checksum(path)
    print(f"  archive: {path.name}  {path.stat().st_size:,} bytes")
    print(f"  sha256:  {digest}")

    wrong = verify(path, name)

    print("  retained archives and checksums for review/upload")
    if wrong:
        print("packaged, and not fit to release:")
        for reason in wrong:
            print(f"  - {reason}")
        sys.exit(1)
    print("packaged and verified")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Packaging refused: {error}", file=sys.stderr)
        raise SystemExit(1)
