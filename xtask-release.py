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
import xml.etree.ElementTree as ET
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import HTTPRedirectHandler, Request, build_opener

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
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", value):
        raise ValueError("Version must be a SemVer number without the v prefix")
    if "+" in value and any(not part for part in value.split("+", 1)[1].split(".")):
        raise ValueError("Invalid SemVer build identifiers")
    core, _, pre = value.split("+", 1)[0].partition("-")
    if pre and any(not part or (part.isdigit() and len(part) > 1 and part[0] == "0") for part in pre.split(".")):
        raise ValueError("Invalid SemVer prerelease identifiers")
    return tuple(map(int, core.split("."))), not bool(pre), tuple((0, int(p)) if p.isdigit() else (1, p) for p in pre.split(".") if p)


def is_prerelease(version: str) -> bool:
    """SemVer prerelease identifiers determine the default channel, not major 0."""
    semver_key(version)
    return "-" in version.split("+", 1)[0]


def github_release_policy(version: str) -> dict:
    """Only numbered alpha/beta/rc tags opt into automatic preview publication."""
    preview = is_prerelease(version)
    if preview and not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+-(?:alpha|beta|rc)\.[1-9][0-9]*", version):
        raise ValueError("Preview releases require X.Y.Z-alpha.N, -beta.N or -rc.N with a positive counter and no build metadata")
    if preview and int(version.rsplit(".", 1)[1]) > 99_999:
        raise ValueError("Preview counter exceeds the Mac bundle slot limit (99999)")
    return {"prerelease": preview, "publish_preview": preview, "make_latest": "false",
            "msix_store": not preview and "+" not in version}


def next_preview_version(target: str, tags: list[str], stage: str = "rc") -> str:
    """Plan a fresh numbered preview using locally fetched public tags; no writes."""
    if semver_key(target)[1] is not True or "+" in target or stage not in {"alpha", "beta", "rc"}:
        raise ValueError("Preview target must be a suffix-free X.Y.Z and stage alpha, beta or rc")
    if f"v{target}" in tags:
        raise ValueError("Final target tag already exists; choose the next unreleased version")
    pattern = re.compile(r"v" + re.escape(target) + "-" + stage + r"\.([1-9][0-9]*)")
    counters = [int(match[1]) for tag in tags if (match := pattern.fullmatch(tag))]
    version = f"{target}-{stage}.{max(counters, default=0) + 1}"
    github_release_policy(version)
    return version


class _NoGitHubRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return None


def github_release(tag: str, release_id: str | None = None) -> dict | None:
    """CI-only read of one fixed repository's release; never reads private .env.

    Treat only HTTP 404 as absence. Errors/redirects fail closed and never expose
    token values or API response bodies in logs.
    """
    if os.environ.get("GITHUB_REPOSITORY") != "coolnew/kosha" or not tag.startswith("v"):
        raise ValueError("Release lookup requires the public upstream and a version tag")
    github_release_policy(tag[1:])
    if release_id is not None and not re.fullmatch(r"[1-9][0-9]*", release_id):
        raise ValueError("Release ID must be a positive integer")
    token = os.environ.get("GH_TOKEN")
    if not token:
        raise ValueError("Release lookup needs the workflow GH_TOKEN")
    # GitHub's tag endpoint returns published releases. A newly uploaded draft
    # must be read by the ID returned by the upload action, not treated as absent.
    route = release_id if release_id is not None else "tags/" + quote(tag, safe="")
    request = Request("https://api.github.com/repos/coolnew/kosha/releases/" + route,
                      headers={"Authorization": "Bearer " + token, "Accept": "application/vnd.github+json",
                               "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "kosha-release-workflow"})
    try:
        with build_opener(_NoGitHubRedirect()).open(request, timeout=30) as response:
            body = response.read(1024 * 1024 + 1)
        if len(body) > 1024 * 1024:
            raise ValueError("Release metadata exceeds limit")
        result = json.loads(body)
        if not isinstance(result, dict) or result.get("tag_name") != tag or type(result.get("draft")) is not bool:
            raise ValueError("Release metadata does not match the selected tag")
        if release_id is not None and (type(result.get("id")) is not int or result["id"] != int(release_id)):
            raise ValueError("Release metadata does not match the selected ID")
        return result
    except HTTPError as error:
        if error.code == 404:
            return None
        raise ValueError(f"GitHub release lookup failed (HTTP {error.code})") from None
    except (URLError, OSError, json.JSONDecodeError):
        raise ValueError("GitHub release lookup failed; no publication attempted") from None


def require_unpublished_release(tag: str) -> None:
    existing = github_release(tag)
    if existing is not None and existing["draft"] is not True:
        raise ValueError("Release is already published; preserve its assets and use the next preview number")


def verify_uploaded_release(folder: Path, release: dict, tag: str) -> None:
    """Check uploaded names, sizes and server SHA-256 before a draft is published."""
    policy = github_release_policy(tag.removeprefix("v"))
    if (release is None or release.get("tag_name") != tag or release.get("draft") is not True
            or release.get("prerelease") is not policy["prerelease"]):
        raise ValueError("Uploaded release is not the expected draft/channel")
    expected = {p.name: p for p in folder.iterdir() if p.is_file()}
    assets = release.get("assets", [])
    if not expected or not isinstance(assets, list) or any(not isinstance(a, dict) for a in assets):
        raise ValueError("Missing uploaded asset inventory")
    if len(assets) != len(expected) or {a.get("name") for a in assets} != set(expected):
        raise ValueError("Uploaded asset inventory differs from the verified local inventory")
    for asset in assets:
        path = expected[asset["name"]]
        if (asset.get("state") != "uploaded" or asset.get("size") != path.stat().st_size
                or asset.get("digest") != "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()):
            raise ValueError("Uploaded asset hash/size/state mismatch: " + path.name)


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
    root = root.resolve()
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
            info.create_system = 3  # Stable Unix metadata, even on a Windows producer.
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


RELEASE_TARGETS = {"x86_64-unknown-linux-gnu": ".tar.gz",
                   "x86_64-pc-windows-msvc": ".zip", "aarch64-apple-darwin": ".dmg"}
SOURCE_OWNER = "x86_64-unknown-linux-gnu"


def verify_release_assets(root: Path, commit: str, expect_msix: bool = False) -> None:
    """Verify the exact published inventory, with one producer for shared sources."""
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Expected the full selected source commit")
    files = list(root.iterdir())
    if any(not p.is_file() or p.is_symlink() for p in files):
        raise ValueError("Release assets must be regular files")
    manifests = {p.name for p in files if p.name.startswith("release-manifest-") and p.suffix == ".json"}
    if manifests != {f"release-manifest-{target}.json" for target in RELEASE_TARGETS}:
        raise ValueError("Expected exactly the three named host manifests")
    expected, version = set(manifests), None
    for target, extension in RELEASE_TARGETS.items():
        manifest = json.loads((root / f"release-manifest-{target}.json").read_text(encoding="utf-8"))
        if (manifest.get("schema") != "kosha.release-candidate.v1" or manifest.get("project") != "kosha"
                or manifest.get("publication_candidate") is not True or manifest.get("commit") != commit
                or manifest.get("target") != target):
            raise ValueError(f"Manifest is not from the selected public commit/host: {target}")
        current = manifest.get("version", "")
        semver_key(current)
        if version is not None and current != version:
            raise ValueError("Host manifest versions disagree")
        version = current
        prefix = f"kosha-v{version}"
        names = {prefix + "-" + target + extension}
        if target == "x86_64-pc-windows-msvc":
            names.add(prefix + "-x64-setup.exe")
        if target == SOURCE_OWNER:
            names.update({prefix + "-source.zip", prefix + "-source.tar.gz"})
        entries = manifest.get("artifacts", [])
        if len(entries) != len(names) or {e.get("name") for e in entries} != names:
            raise ValueError(f"Manifest inventory does not match its published files: {target}")
        for entry in entries:
            name = entry["name"]  # Membership above confines this to exact basenames.
            archive, sidecar = root / name, root / (name + ".sha256")
            if not archive.is_file() or not sidecar.is_file():
                raise ValueError(f"Missing archive or SHA-256 sidecar: {name}")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            if (entry.get("sha256") != digest or type(entry.get("bytes")) is not int
                    or entry["bytes"] != archive.stat().st_size):
                raise ValueError(f"Manifest/archive hash or size mismatch: {name}")
            if sidecar.read_text(encoding="utf-8").strip() != f"{digest}  {name}":
                raise ValueError(f"SHA-256 sidecar mismatch: {name}")
            expected.update({name, name + ".sha256"})
    if expect_msix:
        expected.update(verify_msix_upload(root, commit, version))
    if {p.name for p in files} != expected:
        raise ValueError("Unexpected files in published release inventory")


def verify_msix_upload(root: Path, commit: str, version: str) -> set[str]:
    """Verify Store-upload identity/provenance and the same binaries as the ZIP."""
    metadata_name = "msix-store-upload.json"
    metadata = json.loads((root / metadata_name).read_text(encoding="utf-8"))
    name = f"kosha-v{version}-x64-store-upload.msix"
    if (metadata.get("schema") != "kosha.msix-store-upload.v1" or metadata.get("package") != name
            or metadata.get("commit") != commit or metadata.get("version") != version
            or metadata.get("publication_candidate") is not True or metadata.get("signed") is not False
            or metadata.get("diagnostic_probe") is not False or metadata.get("target") != "x86_64-pc-windows-msvc"
            or metadata.get("runtime") != "win32App"):
        raise ValueError("MSIX upload provenance/purpose does not match the public release")
    package, sidecar = root / name, root / (name + ".sha256")
    digest = hashlib.sha256(package.read_bytes()).hexdigest()
    if (digest != metadata.get("sha256") or type(metadata.get("bytes")) is not int
            or metadata["bytes"] != package.stat().st_size
            or sidecar.read_text(encoding="utf-8").strip() != f"{digest}  {name}"):
        raise ValueError("MSIX upload digest, size or sidecar mismatch")
    ns = {"f": "http://schemas.microsoft.com/appx/manifest/foundation/windows10",
          "uap10": "http://schemas.microsoft.com/appx/manifest/uap/windows10/10",
          "uap": "http://schemas.microsoft.com/appx/manifest/uap/windows10",
          "uap5": "http://schemas.microsoft.com/appx/manifest/uap/windows10/5",
          "desktop4": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/4",
          "rescap": "http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"}
    numeric = module(ROOT / "xtask-msix.py", "verify_msix").numeric_version(version)
    with zipfile.ZipFile(package) as msix:
        names = msix.namelist()
        if len(names) != len(set(names)) or any("probe" in n.lower() or n.lower().endswith((".pfx", ".cer", ".ps1", ".cs")) for n in names):
            raise ValueError("MSIX upload contains duplicate or development-only entries")
        if "AppxSignature.p7x" in names:
            raise ValueError("Store upload must not contain a development signature")
        tree = ET.fromstring(msix.read("AppxManifest.xml"))
        identity = tree.find("f:Identity", ns)
        if (identity is None or identity.get("Name") != metadata.get("identity") or identity.get("Name", "").endswith(".Development")
                or identity.get("Publisher") != metadata.get("publisher") or identity.get("Version") != numeric
                or identity.get("ProcessorArchitecture") != "x64"):
            raise ValueError("MSIX packed identity differs from its reviewed metadata")
        display = tree.find("f:Properties/f:PublisherDisplayName", ns)
        if display is None or display.text != metadata.get("publisher_display_name"):
            raise ValueError("MSIX publisher display name mismatch")
        contact = f"KoshaApp\nAuthor: {metadata.get('author_name')}\nPublisher: {metadata.get('publisher_display_name')}\nSupport: {metadata.get('support_email')}\nWebsite: {metadata.get('website')}\n"
        if msix.read("CONTACT.txt").decode("utf-8") != contact:
            raise ValueError("MSIX contact metadata mismatch")
        capabilities = {e.get("Name") for e in tree.findall("f:Capabilities/rescap:Capability", ns)}
        if capabilities != {"runFullTrust", "allowElevation", "unvirtualizedResources"}:
            raise ValueError("MSIX restricted capability inventory mismatch")
        apps = tree.findall("f:Applications/f:Application", ns)
        if len(apps) != 1 or apps[0].get("Id") != "Kosha" or apps[0].get("Executable") != "kosha-gui.exe":
            raise ValueError("MSIX must advertise one visible desktop application")
        visual = apps[0].find("uap:VisualElements", ns)
        if visual is None or visual.get("AppListEntry", "default") != "default":
            raise ValueError("MSIX must not declare a hidden/headless application")
        families = tree.findall("f:Dependencies/f:TargetDeviceFamily", ns)
        if len(families) != 1 or families[0].get("Name") != "Windows.Desktop":
            raise ValueError("MSIX supports Windows.Desktop only")
        extensions = apps[0].findall("f:Extensions/uap5:Extension", ns)
        aliases = {}
        for extension in extensions:
            alias = extension.find("uap5:AppExecutionAlias", ns)
            entries = [] if alias is None else alias.findall("uap5:ExecutionAlias", ns)
            if extension.get("Category") != "windows.appExecutionAlias" or len(entries) != 1:
                raise ValueError("MSIX execution alias inventory mismatch")
            alias_name = entries[0].get("Alias")
            if alias_name in aliases:
                raise ValueError("Duplicate MSIX execution alias")
            aliases[alias_name] = (extension.get("Executable"), alias.get("{" + ns["desktop4"] + "}Subsystem"))
        if aliases != {"kosha.exe": ("kosha.exe", "console")}:
            raise ValueError("MSIX execution alias targets/subsystems differ")
        if any(app.get("{" + ns["uap10"] + "}RuntimeBehavior") != "win32App"
               or app.get("{" + ns["uap10"] + "}TrustLevel") != "mediumIL" for app in apps):
            raise ValueError("MSIX runtime mismatch")
        with zipfile.ZipFile(root / f"kosha-v{version}-x86_64-pc-windows-msvc.zip") as windows:
            for binary in ["kosha.exe", "kosha-gui.exe"]:
                module(ROOT / "xtask-windows.py", "msix_windows_imports").verify_bytes(msix.read(binary))
                original = windows.read(f"kosha-v{version}-x86_64-pc-windows-msvc/{binary}")
                if msix.read(binary) != original:
                    raise ValueError("MSIX and Windows ZIP binaries differ: " + binary)
    return {metadata_name, name, name + ".sha256"}


def validation_commands(root: Path, reuse_rust: bool = False) -> list[list[str]]:
    commands = [] if reuse_rust else [["cargo", "fmt", "--all", "--check"],
                ["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"],
                ["cargo", "test", "--locked", "--workspace"]]
    for script, args in (("xtask-version.py", []), ("xtask-llms.py", ["--check"]), ("xtask-eol.py", []), ("xtask-spdx.py", ["--check"]), ("xtask-gaps.py", []), ("xtask-mutations.py", []), ("xtask-dco.py", [])):
        if not (root / script).is_file():
            raise ValueError(f"Missing required release validation: {script}")
        commands.append([sys.executable, script, *args])
    commands.append([sys.executable, "-m", "unittest", "discover", "-p", "test_*.py"])
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
    parser.add_argument("--next-preview", metavar="X.Y.Z", help="Read-only next numbered preview plan from locally fetched tags")
    parser.add_argument("--preview-stage", choices=["alpha", "beta", "rc"], default="rc", help="Stage for --next-preview (default: rc)")
    content = parser.add_mutually_exclusive_group()
    content.add_argument("--source-only", action="store_true", help="Validate and archive sources without building host binaries")
    content.add_argument("--binary-only", action="store_true", help="Validate and package this host only; CI publishes shared source archives from Linux")
    parser.add_argument("--output-dir", type=Path, help="Fresh subdirectory within selected checkout's dist/")
    parser.add_argument("--target-dir", type=Path, help="Separate rebuildable Cargo target directory")
    parser.add_argument("--msix-store", action="store_true", help="Windows only: also package an unsigned Partner Center upload MSIX from KOSHA_MSIX_* identity/contact variables")
    parser.add_argument("--ci-evidence", type=Path, help="Public tag workflow only: revalidate trusted CI reports and reuse compatible Rust validation")
    args = parser.parse_args()
    try:
        if args.ci_evidence and (args.prepare or args.next_preview):
            raise ValueError("CI evidence is for hosted tag packaging, not version preparation")
        root = choose_repo(args.public, args.repo_path).resolve()
        if Path(git(root, "rev-parse", "--show-toplevel")).resolve() != root:
            raise ValueError("Select the repository root, not a nested directory")
        current = module(root / "xtask-version.py", "release_versions").validate(root)
        if args.next_preview:
            if any((args.execute, args.prepare, args.version, args.notes_file, args.source_only,
                    args.binary_only, args.msix_store, args.output_dir, args.target_dir)):
                raise ValueError("--next-preview is a read-only plan; do not combine it with build/preparation flags")
            version = next_preview_version(args.next_preview, git(root, "tag", "--list").splitlines(), args.preview_stage)
            if semver_key(version) <= semver_key(current):
                raise ValueError("Planned preview must be newer than the current workspace version")
            print(json.dumps({"mode": "next-preview", "version": version, "tag": "v" + version,
                              "policy": github_release_policy(version), "git_mutations": [], "remote_mutations": [],
                              "next": "Fetch public tags first; review this plan, then prepare matching versions and notes"}, indent=2))
            return 0
        if args.msix_store and (os.name != "nt" or args.source_only or args.prepare):
            raise ValueError("--msix-store requires a Windows host binary candidate build")
        if args.msix_store:
            if not github_release_policy(current)["msix_store"]:
                raise ValueError("Store MSIX needs a suffix-free numeric version; preview releases use the Windows ZIP")
            required = ["IDENTITY_NAME", "PUBLISHER", "PUBLISHER_DISPLAY_NAME", "AUTHOR_NAME", "SUPPORT_EMAIL", "WEBSITE"]
            missing = ["KOSHA_MSIX_" + name for name in required if not os.environ.get("KOSHA_MSIX_" + name)]
            if missing:
                raise ValueError("Configure MSIX identity/contact variables before building: " + ", ".join(missing))
        state = git_state(root)
        version = args.version or current
        semver_key(version)
        if git(root, "tag", "--list", f"v{version}") and (args.prepare or git(root, "rev-list", "-n", "1", f"v{version}") != state["head"]):
            raise ValueError("Candidate tag already points elsewhere; release tags are never moved")
        if args.prepare:
            if not args.version or not args.notes_file or args.source_only or args.binary_only:
                raise ValueError("--prepare needs --version and --notes-file, without source/binary-only selection")
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
        windows = module(root / "xtask-windows.py", "release_windows") if os.name == "nt" else None
        files = package.public_payloads(root)
        candidate = not (root / "publication/files.jsonl").exists()
        if candidate:
            remote = git(root, "remote", "get-url", "origin")
            if remote.rstrip("/").removesuffix(".git") not in {"git@github.com:coolnew/kosha", "https://github.com/coolnew/kosha"}:
                raise ValueError("Public candidate origin must identify coolnew/kosha")
        notes_ready = bool(re.search(r'^## \[?' + re.escape(version) + r'\]?\s+[—-]\s+\d{4}-\d{2}-\d{2}\s*$', files.get("CHANGELOG.md", b"").decode("utf-8"), re.M))
        ci_validation = None
        reuse_rust = False
        if args.ci_evidence:
            evidence_tool = module(root / "xtask-ci-evidence.py", "release_ci_evidence")
            evidence = evidence_tool.verified(root, args.ci_evidence)
            rustc = subprocess.check_output(["rustc", "-vV"], text=True).strip()
            reuse_rust = evidence_tool.reuse_for_host(evidence, os.environ.get("RUNNER_OS"), rustc,
                                                     os.environ.get("ImageOS", ""), os.environ.get("ImageVersion", ""))
            ci_validation = {k: evidence[k] for k in ("repository", "commit", "run_id", "run_attempt", "url")}
            ci_validation["reused_rust_validation"] = reuse_rust
            ci_validation["decision"] = ("Reuse CI format/Clippy/Rust tests" if reuse_rust else
                                          "Runner image or compiler differs; run full local Rust validation")
            print(ci_validation["decision"], flush=True)
        checks = validation_commands(root, reuse_rust=reuse_rust)
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
                "archives": ([] if args.binary_only else ["source.zip", "source.tar.gz"]) + ([] if args.source_only else ["host binary archive"]) + (["Partner Center upload MSIX (unsigned)"] if args.msix_store else []),
                "git_mutations": [], "remote_mutations": [],
                "remaining_release_acceptance": "Linux/non-host, clean-machine GUI/Sandbox, agreement and hosted workflow checks remain separate evidence"}
        if ci_validation is not None:
            plan["ci_validation"] = ci_validation
        if windows and not args.source_only:
            module(root / "xtask-installer.py", "release_installer").numeric_version(version)
            plan["build"] += ["--target", windows.TARGET]
            plan["archives"].append("NSIS offline Setup EXE (static CRT)")
        print(json.dumps(plan, indent=2), flush=True)
        if not args.execute:
            return 0
        before = require_clean(root)
        unchanged_preflight(state, before)
        if candidate and not notes_ready:
            raise ValueError("Public candidate needs a matching dated CHANGELOG entry; prepare and commit release notes first")
        environment = dict(os.environ, CARGO_TARGET_DIR=str(target))
        binary_dir = (target / windows.TARGET if windows else target) / "release"
        for command in checks:
            run_command(root, command, environment, before)
        if not args.source_only:
            run_command(root, plan["build"], windows.build_environment(environment) if windows else environment, before)
            console_probe = root / "scripts/test-windows-gui-console.py"
            if os.name == "nt" and console_probe.exists():
                run_command(root, [sys.executable, str(console_probe), "--gui", str(binary_dir / "kosha-gui.exe")], environment, before)
        if package.public_payloads(root) != files:
            raise ValueError("Public source bytes changed during validation/build")
        out.mkdir(parents=True, exist_ok=True)
        epoch = int(git(root, "show", "-s", "--format=%ct", "HEAD"))
        artifacts = [] if args.binary_only else write_sources(files, out, version, epoch)
        if not args.source_only:
            package.ROOT, package.DIST = root, out
            previous_target = os.environ.get("CARGO_TARGET_DIR")
            previous_triple = os.environ.get("CARGO_BUILD_TARGET")
            os.environ["CARGO_TARGET_DIR"] = str(target)
            if windows:
                os.environ["CARGO_BUILD_TARGET"] = windows.TARGET
            try:
                name = f"kosha-v{version}-{package.target_triple()}"
                folder = package.assemble(name)
                binary_archive = package.archive(folder)
                wrong = package.verify(binary_archive, name)
                if wrong:
                    raise ValueError("Binary package smoke checks failed: " + "; ".join(wrong))
                artifacts.append(binary_archive)
                if windows:
                    installer = module(root / "xtask-installer.py", "release_installer_build")
                    setup = installer.build(folder, out / "installer-work", version)
                    destination = out / setup.name
                    setup.rename(destination)
                    artifacts.append(destination)
            finally:
                if previous_target is None:
                    os.environ.pop("CARGO_TARGET_DIR", None)
                else:
                    os.environ["CARGO_TARGET_DIR"] = previous_target
                if previous_triple is None:
                    os.environ.pop("CARGO_BUILD_TARGET", None)
                else:
                    os.environ["CARGO_BUILD_TARGET"] = previous_triple
        protected(root, before)
        if args.msix_store:
            msix_out = out / "msix-work"
            run_command(root, [sys.executable, "xtask-msix.py", "--store-upload", "--allow-elevation", "--unvirtualized",
                              "--binary-dir", str(binary_dir), "--output-dir", str(msix_out), "--execute"], environment, before)
            for name in [f"kosha-v{version}-x64-store-upload.msix", f"kosha-v{version}-x64-store-upload.msix.sha256", "msix-store-upload.json"]:
                (msix_out / name).rename(out / name)
        manifest = {"schema": "kosha.release-candidate.v1", "project": "kosha", "version": version,
                    "commit": state["head"], "target": package.target_triple(),
                    "publication_candidate": candidate, "validation": "selected host checks passed",
                    "source_scope": "reviewed public files only", "artifacts": []}
        if ci_validation is not None:
            manifest["ci_validation"] = ci_validation
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
