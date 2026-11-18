#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Plan or build a protected release candidate. Default is a read-only dry-run.

Use --prepare --version X.Y.Z --notes-file FILE --execute to prepare version
and changelog edits, review/commit them yourself, then --execute to validate,
build, smoke-test and archive. No command commits, tags, pushes or uploads.
"""

from __future__ import annotations

import argparse
from datetime import date
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent


def module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "--no-optional-locks", *args], cwd=root, text=True, encoding="utf-8").strip()


def choose_repo(public: bool = False, repo_path: Path | None = None) -> Path:
    if repo_path is not None:
        return repo_path.resolve()
    if public:
        return ROOT if not (ROOT / "publication/files.jsonl").exists() else ROOT.parent / "github" / "kosha"
    return ROOT


def git_state(root: Path) -> dict[str, str]:
    return {"head": git(root, "rev-parse", "HEAD"),
            "index": git(root, "ls-files", "--stage", "-z"),
            "refs": git(root, "for-each-ref", "--format=%(refname) %(objectname)"),
            "status": git(root, "status", "--porcelain=v1", "--untracked-files=all")}


def require_clean(root: Path) -> dict[str, str]:
    state = git_state(root)
    if state["status"]:
        raise ValueError("Selected repository has staged, modified or untracked files; review and commit them before release execution")
    return state


def protected(root: Path, before: dict[str, str], allow_worktree: bool = False) -> None:
    after = git_state(root)
    for key in ("head", "index", "refs"):
        if after[key] != before[key]:
            raise ValueError(f"Git {key} changed during release work; stopped without attempting any reset or repair")
    if not allow_worktree and after["status"]:
        raise ValueError("Working tree changed during validation/build; stopped without touching those changes")


def unchanged_preflight(initial: dict[str, str], before: dict[str, str]) -> None:
    if any(initial[key] != before[key] for key in ("head", "index", "refs")):
        raise ValueError("Git changed while planning the candidate; rerun from the new reviewed revision")


def semver_key(value: str):
    if not re.fullmatch(r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", value):
        raise ValueError("Version must be a SemVer number without the v prefix")
    core, _, pre = value.split("+", 1)[0].partition("-")
    if pre and any(not part or (part.isdigit() and len(part) > 1 and part[0] == "0") for part in pre.split(".")):
        raise ValueError("Invalid SemVer prerelease identifiers")
    return tuple(map(int, core.split("."))), not bool(pre), tuple((0, int(p)) if p.isdigit() else (1, p) for p in pre.split(".") if p)


def updated_manifest(text: str, old: str, new: str) -> str:
    section = ""
    result = []
    for line in text.splitlines(keepends=True):
        if line.startswith("["):
            section = line.strip()
        if section == "[workspace.package]" and re.match(r"version\s*=", line):
            line, count = re.subn(r'^(version\s*=\s*)"' + re.escape(old) + '"', r'\g<1>"' + new + '"', line)
            if count != 1:
                raise ValueError("Workspace version could not be updated uniquely")
        elif section == "[workspace.dependencies]" and re.search(r"\bpath\s*=", line):
            line, count = re.subn(r'(\bversion\s*=\s*)"' + re.escape(old) + '"', r'\g<1>"' + new + '"', line)
            if count != 1:
                raise ValueError("A local path dependency needs an explicit matching version")
        result.append(line)
    return "".join(result)


def updated_lock(text: str, names: set[str], old: str, new: str) -> str:
    blocks = re.split(r"(?m)(?=^\[\[package\]\]$)", text)
    changed = set()
    for index, block in enumerate(blocks):
        match = re.search(r'^name = "([^"]+)"$', block, re.M)
        if match and match[1] in names and not re.search(r"^source =", block, re.M):
            blocks[index], count = re.subn(r'^version = "' + re.escape(old) + '"$', 'version = "' + new + '"', block, count=1, flags=re.M)
            if count != 1 or match[1] in changed:
                raise ValueError("Workspace lockfile version is ambiguous")
            changed.add(match[1])
    if changed != names:
        raise ValueError("Cargo.lock does not contain all workspace members")
    return "".join(blocks)


def updated_changelog(text: str, version: str, notes: str, today: str) -> str:
    if re.search(r'^## \[?' + re.escape(version) + r'\]?(?:\s|$)', text, re.M):
        raise ValueError(f"Changelog already contains {version}")
    match = re.search(r'^## (?:\[Unreleased\]|Unreleased)\s*\n', text, re.M)
    if not match:
        raise ValueError("Changelog needs an Unreleased heading")
    next_heading = re.search(r'^## ', text[match.end():], re.M)
    end = match.end() + next_heading.start() if next_heading else len(text)
    previous = text[match.end():end].strip()
    if previous.lower() in {"nothing yet.", "nothing yet", ""}:
        previous = ""
    body = notes.strip() + ("\n\n" + previous if previous else "")
    replacement = f"## [Unreleased]\n\nNothing yet.\n\n## [{version}] — {today}\n\n{body}\n\n"
    result = text[:match.start()] + replacement + text[end:]
    result = re.sub(r'(?m)^\[Unreleased\]:.*$', f"[Unreleased]: https://github.com/coolnew/kosha/compare/v{version}...HEAD", result)
    if not re.search(r'^\[' + re.escape(version) + r'\]:', result, re.M):
        result = result.rstrip() + f"\n\n[{version}]: https://github.com/coolnew/kosha/releases/tag/v{version}\n"
    return result


def preparation(root: Path, version: str, notes: str, today: str) -> dict[str, bytes]:
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    workspace = manifest["workspace"]
    old = workspace["package"]["version"]
    if semver_key(version) <= semver_key(old):
        raise ValueError("Prepared version must be newer than the current workspace version")
    if not notes.strip() or len(notes.encode()) > 1024 * 1024:
        raise ValueError("Provide reviewed, nonempty public release notes of at most 1 MiB")
    names = {tomllib.loads((root / member / "Cargo.toml").read_text(encoding="utf-8"))["package"]["name"] for member in workspace["members"]}
    changes = {"Cargo.toml": updated_manifest((root / "Cargo.toml").read_text(encoding="utf-8"), old, version).encode(),
               "Cargo.lock": updated_lock((root / "Cargo.lock").read_text(encoding="utf-8"), names, old, version).encode()}
    for name in ("CHANGELOG.md", "publication/templates/CHANGELOG.md"):
        path = root / name
        if path.exists():
            changes[name] = updated_changelog(path.read_text(encoding="utf-8"), version, notes, today).encode()
    return changes


def safe_output(root: Path, value: Path) -> Path:
    if not value.is_absolute():
        value = root / value
    out = value.resolve()
    dist = (root / "dist").resolve()
    if out == dist or not out.is_relative_to(dist):
        raise ValueError("Output must be a fresh subdirectory inside the selected repository's dist directory")
    for parent in (value, *value.parents):
        if parent.is_symlink() or getattr(parent, "is_junction", lambda: False)():
            raise ValueError("Output cannot traverse symlinks or junctions")
    if out.exists() and any(out.iterdir()):
        raise ValueError("Output is not empty; choose a new directory so existing artifacts are preserved")
    return out


def safe_target(root: Path, value: Path) -> Path:
    target = value if value.is_absolute() else root / value
    for parent in (target, *target.parents):
        if parent.name.lower() == ".git" or parent.is_symlink() or getattr(parent, "is_junction", lambda: False)():
            raise ValueError("Cargo target cannot traverse Git metadata, symlinks or junctions")
    target = target.resolve()
    if target.is_relative_to(root) and not target.is_relative_to(root / "target"):
        raise ValueError("In-repository Cargo output must stay under target/, never source or Git metadata")
    return target


def write_sources(files: dict[str, bytes], out: Path, version: str, epoch: int) -> list[Path]:
    prefix = f"kosha-v{version}-source"
    zip_path = out / (prefix + ".zip")
    tar_path = out / (prefix + ".tar.gz")
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in sorted(files.items()):
            info = zipfile.ZipInfo(f"{prefix}/{name}", (2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data)
    # Gzip's timestamp is kept deterministic as well as each TAR member's.
    import gzip
    with tar_path.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as archive:
            for name, data in sorted(files.items()):
                info = tarfile.TarInfo(f"{prefix}/{name}")
                info.size = len(data)
                info.mode = 0o644
                info.mtime = epoch
                archive.addfile(info, io.BytesIO(data))
    return [zip_path, tar_path]


def validation_commands(root: Path) -> list[list[str]]:
    commands = [["cargo", "fmt", "--all", "--check"],
                ["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"],
                ["cargo", "test", "--locked", "--workspace"]]
    for script, args in (("xtask-version.py", []), ("xtask-llms.py", ["--check"]), ("xtask-eol.py", []), ("xtask-spdx.py", ["--check"]), ("xtask-gaps.py", []), ("xtask-mutations.py", []), ("xtask-dco.py", [])):
        if not (root / script).is_file():
            raise ValueError(f"Missing required release validation: {script}")
        commands.append([sys.executable, script, *args])
    commands.append([sys.executable, "-m", "unittest", "discover", "-p", "test_*.py"])
    if (root / "marketplace/catalog.json").exists():
        commands.append([sys.executable, "xtask-marketplace.py", "--root", "marketplace", "--check"])
    if (root / "scripts/github/test_kosha_github.py").exists():
        commands.append([sys.executable, "-m", "unittest", "discover", "-s", "scripts/github", "-p", "test_*.py"])
    return commands


def run_command(root: Path, command: list[str], environment: dict[str, str], before: dict[str, str]) -> None:
    print("Running:", " ".join(command), flush=True)
    try:
        subprocess.run(command, cwd=root, env=environment, check=True)
    finally:
        protected(root, before)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    location = parser.add_mutually_exclusive_group()
    location.add_argument("--public", action="store_true", help="Select the separate public checkout")
    location.add_argument("--repo-path", type=Path, help="Select another explicit checkout")
    parser.add_argument("--execute", action="store_true", help="Perform the planned preparation or full validation/build/package")
    parser.add_argument("--prepare", action="store_true", help="Prepare version/changelog edits only; never commits them")
    parser.add_argument("--version", help="Prepared version, or the expected existing candidate version")
    parser.add_argument("--notes-file", type=Path, help="Reviewed public notes for --prepare")
    parser.add_argument("--source-only", action="store_true", help="Validate and archive sources without building host binaries")
    parser.add_argument("--output-dir", type=Path, help="Fresh subdirectory within selected checkout's dist/")
    parser.add_argument("--target-dir", type=Path, help="Separate rebuildable Cargo target directory")
    args = parser.parse_args()
    try:
        root = choose_repo(args.public, args.repo_path).resolve()
        if git(root, "rev-parse", "--show-toplevel").replace("\\", "/").lower() != str(root).replace("\\", "/").lower():
            raise ValueError("Select the repository root, not a nested directory")
        current = module(root / "xtask-version.py", "release_versions").validate(root)
        state = git_state(root)
        version = args.version or current
        semver_key(version)
        if git(root, "tag", "--list", f"v{version}") and (args.prepare or git(root, "rev-list", "-n", "1", f"v{version}") != state["head"]):
            raise ValueError("Candidate tag already points elsewhere; release tags are never moved")
        if args.prepare:
            if not args.version or not args.notes_file or args.source_only:
                raise ValueError("--prepare needs --version and --notes-file, without --source-only")
            notes = args.notes_file.read_text(encoding="utf-8-sig")
            changes = preparation(root, version, notes, date.today().isoformat())
            print(json.dumps({"mode": "prepare", "execute": args.execute, "version": version,
                              "files_to_edit": list(changes), "git_mutations": [],
                              "next": "Review and sign-off commit these edits yourself before --execute packaging"}, indent=2))
            if args.execute:
                before = require_clean(root)
                unchanged_preflight(state, before)
                originals = {name: (root / name).read_bytes() for name in changes}
                try:
                    for name, data in changes.items():
                        if (root / name).read_bytes() != originals[name]:
                            raise ValueError(f"{name} changed while preparing the version; edits were preserved")
                        (root / name).write_bytes(data)
                    module(root / "xtask-version.py", "prepared_versions").validate(root, f"v{version}")
                    protected(root, before, allow_worktree=True)
                except Exception:
                    # Restore only files this operation edited; never reset a
                    # branch, index, unrelated file or an entire worktree.
                    for name, old in originals.items():
                        if (root / name).read_bytes() == changes[name]:
                            (root / name).write_bytes(old)
                    raise
            return 0
        if args.notes_file or version != current:
            raise ValueError("Use --prepare for a version bump; packaging requires the committed current version")
        package = module(root / "xtask-package.py", "release_package")
        files = package.public_payloads(root)
        candidate = not (root / "publication/files.jsonl").exists()
        if candidate:
            remote = git(root, "remote", "get-url", "origin")
            if remote.rstrip("/").removesuffix(".git") not in {"git@github.com:coolnew/kosha", "https://github.com/coolnew/kosha"}:
                raise ValueError("Public candidate origin must identify coolnew/kosha")
        notes_ready = bool(re.search(r'^## \[?' + re.escape(version) + r'\]?\s+[—-]\s+\d{4}-\d{2}-\d{2}\s*$', files.get("CHANGELOG.md", b"").decode("utf-8"), re.M))
        checks = validation_commands(root)
        short = state["head"][:12]
        out = args.output_dir or root / "dist/releases" / f"v{version}-{short}"
        out = safe_output(root, out)
        target = args.target_dir
        if target is None and os.name == "nt" and Path("R:/").is_dir():
            checkout = "sizer-windows" if root == ROOT and not candidate else ("kosha-public-windows" if root.name == "kosha" and root.parent.name == "github" else "release-" + hashlib.sha256(str(root).encode()).hexdigest()[:10] + "-windows")
            target = Path("R:/build/kosha") / checkout / "target"
        target = target or root / "target"
        target = safe_target(root, target)
        plan = {"mode": "release-candidate", "execute": args.execute, "version": version,
                "commit": state["head"], "clean": not bool(state["status"]),
                "publication_candidate": candidate, "source_files": len(files),
                "dated_release_notes_ready": notes_ready,
                "output": str(out), "cargo_target": str(target), "validations": checks,
                "build": [] if args.source_only else ["cargo", "build", "--locked", "--release", "-p", "kosha-cli", "-p", "kosha-gui"],
                "archives": ["source.zip", "source.tar.gz"] + ([] if args.source_only else ["host binary archive"]),
                "git_mutations": [], "remote_mutations": [],
                "remaining_release_acceptance": "Linux/non-host, clean-machine GUI/Sandbox, agreement and hosted workflow checks remain separate evidence"}
        print(json.dumps(plan, indent=2), flush=True)
        if not args.execute:
            return 0
        before = require_clean(root)
        unchanged_preflight(state, before)
        if candidate and not notes_ready:
            raise ValueError("Public candidate needs a matching dated CHANGELOG entry; prepare and commit release notes first")
        environment = dict(os.environ, CARGO_TARGET_DIR=str(target))
        for command in checks:
            run_command(root, command, environment, before)
        if not args.source_only:
            run_command(root, plan["build"], environment, before)
            console_probe = root / "scripts/test-windows-gui-console.py"
            if os.name == "nt" and console_probe.exists():
                run_command(root, [sys.executable, str(console_probe), "--gui", str(target / "release/kosha-gui.exe")], environment, before)
        if package.public_payloads(root) != files:
            raise ValueError("Public source bytes changed during validation/build")
        out.mkdir(parents=True, exist_ok=True)
        epoch = int(git(root, "show", "-s", "--format=%ct", "HEAD"))
        artifacts = write_sources(files, out, version, epoch)
        if not args.source_only:
            package.ROOT, package.DIST = root, out
            previous_target = os.environ.get("CARGO_TARGET_DIR")
            os.environ["CARGO_TARGET_DIR"] = str(target)
            try:
                name = f"kosha-v{version}-{package.target_triple()}"
                folder = package.assemble(name)
                binary_archive = package.archive(folder)
                wrong = package.verify(binary_archive, name)
                if wrong:
                    raise ValueError("Binary package smoke checks failed: " + "; ".join(wrong))
                artifacts.append(binary_archive)
            finally:
                if previous_target is None:
                    os.environ.pop("CARGO_TARGET_DIR", None)
                else:
                    os.environ["CARGO_TARGET_DIR"] = previous_target
        protected(root, before)
        manifest = {"schema": "kosha.release-candidate.v1", "project": "kosha", "version": version,
                    "commit": state["head"], "target": package.target_triple(),
                    "publication_candidate": candidate, "validation": "selected host checks passed",
                    "source_scope": "reviewed public files only", "artifacts": []}
        for artifact in artifacts:
            digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
            Path(str(artifact) + ".sha256").write_text(f"{digest}  {artifact.name}\n", encoding="utf-8", newline="\n")
            manifest["artifacts"].append({"name": artifact.name, "sha256": digest, "bytes": artifact.stat().st_size})
        (out / "release-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
        protected(root, before)
        print(f"Candidate artifacts ready in {out}; no commit, tag, push, upload or publication performed")
        return 0
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Release refused: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
