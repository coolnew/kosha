#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Create an unsigned development or Partner Center upload MSIX.

Default is a read-only plan. --execute stages reviewed public manuals and two
version-checked binaries, generates icons, and invokes Windows SDK MakeAppx.
No certificate, trust, installation, submission or release mutation is performed.
The .Development identity and kosha-msix-dev.exe alias cannot replace the Store
identity or kosha.exe alias. --store-upload uses the exact registered identity,
checks clean-source binary provenance and excludes development diagnostics.
Store upload output is unsigned, not a directly installable consumer download.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent
FOUNDATION = "http://schemas.microsoft.com/appx/manifest/foundation/windows10"
NS = {"uap": "http://schemas.microsoft.com/appx/manifest/uap/windows10",
      "uap5": "http://schemas.microsoft.com/appx/manifest/uap/windows10/5",
      "uap10": "http://schemas.microsoft.com/appx/manifest/uap/windows10/10",
      "desktop4": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/4",
      "rescap": FOUNDATION + "/restrictedcapabilities"}


def package_module():
    spec = importlib.util.spec_from_file_location("msix_package", ROOT / "xtask-package.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def numeric_version(version):
    match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version)
    if not match:
        raise ValueError("Development package needs a suffix-free workspace version")
    major, minor, patch = map(int, match.groups())
    parts = (major + 1, minor, patch, 0)
    if any(p > 65535 for p in parts):
        raise ValueError("Version exceeds MSIX numeric limits")
    return ".".join(map(str, parts))


def manifest(identity, publisher, display, version, elevation, probe=False, unvirtualized=False, store=False):
    limit = 50 if store else 38
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]{2," + str(limit - 1) + "}", identity):
        raise ValueError(f"Identity name must be 3–{limit} ASCII letters/digits/dots/hyphens")
    if store and (probe or identity.endswith(".Development")):
        raise ValueError("Store uploads cannot contain a development identity or diagnostic probe")
    if not publisher.startswith("CN=") or not publisher[3:].strip() or len(publisher) > 8192:
        raise ValueError("Supply the full assigned Publisher distinguished name starting with CN=")
    if not display.strip() or len(display) > 256:
        raise ValueError("Supply PublisherDisplayName")
    ET.register_namespace("", FOUNDATION)
    for prefix, uri in NS.items():
        ET.register_namespace(prefix, uri)
    def child(parent, name, attrs=None, text=None):
        prefix, _, local = name.partition(":")
        tag = f"{{{NS[prefix]}}}{local}" if local else f"{{{FOUNDATION}}}{prefix}"
        element = ET.SubElement(parent, tag, attrs or {})
        element.text = text
        return element
    root = ET.Element(f"{{{FOUNDATION}}}Package", {"IgnorableNamespaces": " ".join(NS)})
    child(root, "Identity", {"Name": identity if store else identity + ".Development", "Publisher": publisher,
                            "Version": numeric_version(version), "ProcessorArchitecture": "x64"})
    properties = child(root, "Properties")
    app_name = "KoshaApp" if store else "KoshaApp Development"
    for name, value in [("DisplayName", app_name), ("PublisherDisplayName", display),
                        ("Description", "Kosha disk usage explorer" if store else "Kosha development package — not for Store submission"),
                        ("Logo", "Assets/StoreLogo.png")]:
        child(properties, name, text=value)
    resources = child(root, "Resources")
    child(resources, "Resource", {"Language": "en-us"})
    dependencies = child(root, "Dependencies")
    child(dependencies, "TargetDeviceFamily", {"Name": "Windows.Desktop", "MinVersion": "10.0.19041.0",
                                               "MaxVersionTested": "10.0.26100.0"})
    applications = child(root, "Applications")
    entries = [("Kosha", "kosha-gui.exe"), ("Cli", "kosha.exe")]
    if probe:
        entries.append(("Probe", "kosha-msix-probe.exe"))
    for app_id, executable in entries:
        attrs = {"Id": app_id, "Executable": executable, f"{{{NS['uap10']}}}RuntimeBehavior": "win32App" if unvirtualized else "packagedClassicApp",
                 f"{{{NS['uap10']}}}TrustLevel": "mediumIL"}
        if app_id != "Kosha":
            attrs[f"{{{NS['desktop4']}}}Subsystem"] = "console"
            attrs[f"{{{NS['desktop4']}}}SupportsMultipleInstances"] = "true"
        app = child(applications, "Application", attrs)
        visual = {"DisplayName": app_name, "Description": "Kosha disk usage explorer",
                  "Square150x150Logo": "Assets/Square150x150Logo.png",
                  "Square44x44Logo": "Assets/Square44x44Logo.png", "BackgroundColor": "transparent"}
        if app_id != "Kosha":
            visual["AppListEntry"] = "none"
        child(app, "uap:VisualElements", visual)
        extensions = child(app, "Extensions")
        extension = child(extensions, "uap5:Extension", {"Category": "windows.appExecutionAlias"})
        alias = child(extension, "uap5:AppExecutionAlias")
        aliases = {"Cli": "kosha.exe", "Kosha": "kosha-gui.exe"} if store else {"Cli": "kosha-msix-dev.exe", "Kosha": "kosha-gui-msix-dev.exe", "Probe": "kosha-msix-probe.exe"}
        child(alias, "uap5:ExecutionAlias", {"Alias": aliases[app_id]})
    capabilities = child(root, "Capabilities")
    child(capabilities, "rescap:Capability", {"Name": "runFullTrust"})
    if elevation:
        child(capabilities, "rescap:Capability", {"Name": "allowElevation"})
    if unvirtualized:
        child(capabilities, "rescap:Capability", {"Name": "unvirtualizedResources"})
    ET.indent(root)
    return ET.tostring(root, encoding="utf-8", xml_declaration=True)


def sdk_tool(name):
    base = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Windows Kits/10/bin"
    paths = [p for p in base.glob(f"*/x64/{name}") if re.fullmatch(r"[0-9]+(?:\.[0-9]+){3}", p.parent.parent.name)]
    paths.sort(key=lambda p: tuple(map(int, p.parent.parent.name.split("."))), reverse=True)
    if not paths:
        raise ValueError(f"Windows SDK {name} not found")
    return paths[0]


def fresh_output(root, value):
    path = value if value.is_absolute() else root / value
    for parent in (path, *path.parents):
        if parent.is_symlink() or getattr(parent, "is_junction", lambda: False)():
            raise ValueError("MSIX output cannot traverse a symlink/junction")
    path = path.resolve()
    if path == root / "dist" or not path.is_relative_to((root / "dist").resolve()):
        raise ValueError("Choose a fresh subdirectory of dist/")
    if path.exists():
        raise ValueError("Output exists; preserve it and choose another directory")
    return path


ICON_SCRIPT = r'''
Add-Type -AssemblyName System.Drawing
$source = [System.Drawing.Image]::FromFile($env:KOSHA_MSIX_ICON)
try {
    foreach ($item in @(@('StoreLogo.png',50),@('Square150x150Logo.png',150),@('Square44x44Logo.png',44))) {
        $size = [int]$item[1]
        $bitmap = New-Object System.Drawing.Bitmap($size,$size)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.DrawImage($source,0,0,$size,$size)
            $bitmap.Save((Join-Path $env:KOSHA_MSIX_ASSETS $item[0]),[System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
} finally { $source.Dispose() }
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity-file", type=Path, help="Private kosha.windows-store-config.v1 file; never copied to the package")
    parser.add_argument("--identity-name", default=os.environ.get("KOSHA_MSIX_IDENTITY_NAME"), help="Assigned package name; defaults to KOSHA_MSIX_IDENTITY_NAME")
    parser.add_argument("--publisher", default=os.environ.get("KOSHA_MSIX_PUBLISHER"), help="Exact CN=… Publisher; defaults to KOSHA_MSIX_PUBLISHER")
    parser.add_argument("--publisher-display-name", default=os.environ.get("KOSHA_MSIX_PUBLISHER_DISPLAY_NAME"))
    parser.add_argument("--author-name", default=os.environ.get("KOSHA_MSIX_AUTHOR_NAME"))
    parser.add_argument("--support-email", default=os.environ.get("KOSHA_MSIX_SUPPORT_EMAIL"))
    parser.add_argument("--website", default=os.environ.get("KOSHA_MSIX_WEBSITE"))
    parser.add_argument("--store-upload", action="store_true", help="Exact registered identity, clean binary provenance and unsigned Partner Center upload output")
    parser.add_argument("--binary-dir", type=Path, required=True, help="Directory with current x64 release binaries")
    parser.add_argument("--output-dir", type=Path, default=Path("dist/msix-development"))
    parser.add_argument("--allow-elevation", action="store_true", help="Include restricted capability for UAC/MFT tests, not Store approval")
    parser.add_argument("--probe-binary", type=Path, help="Optional explicit local diagnostic exe; development only")
    parser.add_argument("--unvirtualized", action="store_true", help="Experimental win32App runtime with restricted unvirtualizedResources; needs separate Store review")
    parser.add_argument("--execute", action="store_true")
    args = parser.parse_args()
    try:
        if args.identity_file:
            config = json.loads(args.identity_file.read_text(encoding="utf-8"))
            if config.get("schema") != "kosha.windows-store-config.v1":
                raise ValueError("Unsupported private Store configuration schema")
            args.identity_name = config["identity"]["name"]
            args.publisher = config["identity"]["publisher"]
            args.publisher_display_name = config["identity"]["publisher_display_name"]
            for key in ["author_name", "support_email", "website"]:
                setattr(args, key, config["listing"][key])
        if not all(isinstance(v, str) and v for v in [args.identity_name, args.publisher, args.publisher_display_name]):
            raise ValueError("Supply all assigned identity fields, a private identity file, or KOSHA_MSIX_* variables")
        if args.store_upload and (not args.author_name or not args.support_email or not args.website):
            raise ValueError("Store upload needs author name, support email and website contact metadata")
        if args.store_upload and (not re.fullmatch(r"[^\s@]+@[^\s@]+\.[^\s@]+", args.support_email)
                                  or not re.fullmatch(r"https://[^\s?#]+", args.website)):
            raise ValueError("Supply a valid support email and HTTPS website without query parameters")
        version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
        xml = manifest(args.identity_name, args.publisher, args.publisher_display_name, version, args.allow_elevation, bool(args.probe_binary), args.unvirtualized, args.store_upload)
        out = fresh_output(ROOT, args.output_dir)
        print(json.dumps({"mode": "msix-store-upload" if args.store_upload else "development-msix", "execute": args.execute, "output": str(out),
                          "identity": args.identity_name if args.store_upload else args.identity_name + ".Development", "package_version": numeric_version(version),
                          "cli_alias": "kosha.exe" if args.store_upload else "kosha-msix-dev.exe", "allow_elevation": args.allow_elevation,
                          "runtime": "win32App" if args.unvirtualized else "packagedClassicApp", "diagnostic_probe": bool(args.probe_binary),
                          "signed": False, "store_upload": args.store_upload, "store_submission": False}, indent=2), flush=True)
        if not args.execute:
            return 0
        if os.name != "nt":
            raise ValueError("Execution requires native Windows and Windows SDK")
        commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        if args.store_upload and subprocess.check_output(["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=ROOT, text=True).strip():
            raise ValueError("Store upload packaging requires a clean committed worktree")
        packer = package_module()
        stamps = []
        for name in ["kosha.exe", "kosha-gui.exe"]:
            binary = args.binary_dir.resolve() / name
            stamp = subprocess.check_output([str(binary), "--version"], text=True).strip()
            parsed = packer.parse_stamp(stamp)
            if parsed["version"] != version or parsed["target"] != "x86_64-pc-windows-msvc":
                raise ValueError("Rebuild current-version x64 Windows binaries")
            if args.store_upload and packer.stamp_problems(stamp, version, commit, "x86_64-pc-windows-msvc"):
                raise ValueError("Store upload binaries must match the clean selected source commit")
            stamps.append(stamp)
        if stamps[0] != stamps[1]:
            raise ValueError("CLI and GUI binary stamps disagree")
        makeappx = sdk_tool("makeappx.exe")
        payload = out / "payload"
        payload.mkdir(parents=True)
        for name, data in packer.documentation_payloads().items():
            dest = payload / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
        for name in ["kosha.exe", "kosha-gui.exe"]:
            shutil.copy2(args.binary_dir / name, payload / name)
        if args.probe_binary:
            shutil.copy2(args.probe_binary, payload / "kosha-msix-probe.exe")
        if args.store_upload:
            (payload / "CONTACT.txt").write_text(f"KoshaApp\nAuthor: {args.author_name}\nPublisher: {args.publisher_display_name}\nSupport: {args.support_email}\nWebsite: {args.website}\n", encoding="utf-8", newline="\n")
        (payload / "AppxManifest.xml").write_bytes(xml)
        assets = payload / "Assets"
        assets.mkdir(exist_ok=True)
        environment = dict(os.environ, KOSHA_MSIX_ICON=str(ROOT / "assets/kosha-256.png"), KOSHA_MSIX_ASSETS=str(assets))
        subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop';" + ICON_SCRIPT],
                       env=environment, check=True)
        package = out / f"kosha-v{version}-x64-{'store-upload' if args.store_upload else 'development-unsigned'}.msix"
        subprocess.run([str(makeappx), "pack", "/v", "/h", "SHA256", "/d", str(payload), "/p", str(package)], check=True)
        digest = packer.checksum(package)
        (out / ("msix-store-upload.json" if args.store_upload else "msix-development.json")).write_text(json.dumps({"schema": "kosha.msix-store-upload.v1" if args.store_upload else "kosha.msix-development.v1",
            "package": package.name, "sha256": digest, "binary_stamp": stamps[0],
            "identity": args.identity_name if args.store_upload else args.identity_name + ".Development", "publisher": args.publisher,
            "publisher_display_name": args.publisher_display_name,
            "author_name": args.author_name, "support_email": args.support_email, "website": args.website,
            "version": version, "commit": commit, "target": "x86_64-pc-windows-msvc",
            "publication_candidate": not (ROOT / "publication/files.jsonl").exists(), "bytes": package.stat().st_size,
            "runtime": "win32App" if args.unvirtualized else "packagedClassicApp", "diagnostic_probe": bool(args.probe_binary),
            "package_version": numeric_version(version), "signed": False, "store_submission": False}, indent=2) + "\n",
            encoding="utf-8", newline="\n")
        return 0
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f"MSIX refused: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
