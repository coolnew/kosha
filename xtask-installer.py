#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Build an offline NSIS Setup EXE from a verified portable payload; preview by default."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
NSIS_VERSION = "v3.13"


def numeric_version(version):
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta|rc)\.([1-9]\d*))?", version)
    if not match:
        raise ValueError("Installer requires a regular or numbered preview version")
    core = list(map(int, match.group(1, 2, 3)))
    stage, number = match.group(4, 5)
    if any(v > 65535 for v in core) or (number and int(number) > 9999):
        raise ValueError("Installer version fields must fit UInt16; preview counter <=9999")
    slot = {"alpha": 10000, "beta": 20000, "rc": 30000, None: 60000}[stage] + (int(number) if number else 0)
    return ".".join(map(str, [*core, slot]))


def quote(value):
    value = str(value)
    if any(c in value for c in '\r\n\x00"'):
        raise ValueError("Unsafe installer input")
    return '"' + value.replace('$', '$$') + '"'


def source(payload, output, version):
    files = []
    for file in sorted(payload.rglob("*")):
        if file.is_symlink() or getattr(file, "is_junction", lambda: False)():
            raise ValueError("Installer payload cannot contain links")
        if file.is_file():
            rel = file.relative_to(payload)
            if any(p.startswith('.') or p in {'publication', 'target'} for p in rel.parts):
                raise ValueError("Unexpected private/build file in installer payload")
            files.append((rel, file))
    install, remove = [], []
    for rel, path in files:
        parent = str(rel.parent).replace('/', '\\')
        install += ['SetOutPath "$INSTDIR' + ('\\' + parent if parent != '.' else '') + '"', 'File ' + quote(path.resolve())]
        remove.append('Delete "$INSTDIR\\' + str(rel).replace('/', '\\').replace('$', '$$') + '"')
    directories = {p for rel, _ in files for p in rel.parents if str(p) != '.'}
    remove += ['RMDir "$INSTDIR\\' + str(p).replace('/', '\\').replace('$', '$$') + '"' for p in sorted(directories, key=lambda p: (-len(p.parts), str(p)))]
    template = (ROOT / "installer/setup.nsi.in").read_text(encoding="utf-8")
    replacements = {'@VERSION@': version, '@NUMERIC@': numeric_version(version), '@OUTPUT@': quote(output.resolve()),
                    '@INSTALL@': '\n'.join(install), '@REMOVE@': '\n'.join(remove),
                    '@ICON@': quote((ROOT / 'assets/kosha.ico').resolve())}
    for key, value in replacements.items():
        template = template.replace(key, value)
    return template


def build(payload, output, version, compiler=None):
    compiler = compiler or os.environ.get("KOSHA_MAKENSIS", "makensis")
    runpy.run_path(str(ROOT / "xtask-windows.py"))["verify_folder"](payload)
    package = runpy.run_path(str(ROOT / 'xtask-package.py'))
    stamps = [subprocess.check_output([str((payload / name).resolve()), '--version'], text=True).strip()
              for name in ['kosha.exe', 'kosha-gui.exe']]
    if stamps[0] != stamps[1] or package['parse_stamp'](stamps[0])['version'] != version:
        raise ValueError('Installer version and executable stamps disagree')
    expected = package['documentation_payloads']()
    expected.update({'kosha.exe': (payload / 'kosha.exe').read_bytes(),
                     'kosha-gui.exe': (payload / 'kosha-gui.exe').read_bytes(),
                     'kosha-256.png': (ROOT / 'assets/kosha-256.png').read_bytes()})
    actual_files = {p.relative_to(payload).as_posix(): p.read_bytes() for p in payload.rglob('*') if p.is_file()}
    if actual_files != expected:
        raise ValueError('Installer payload differs from reviewed portable files')
    actual = subprocess.check_output([compiler, "/VERSION"], text=True).strip()
    if actual != NSIS_VERSION:
        raise ValueError(f"Use pinned NSIS {NSIS_VERSION}, found {actual}")
    if output.exists():
        raise ValueError("Installer output exists; choose a fresh directory")
    output.mkdir(parents=True)
    setup = output / f"kosha-v{version}-x64-setup.exe"
    script = output / 'setup.nsi'
    script.write_text(source(payload, setup, version), encoding='utf-8', newline='\n')
    subprocess.run([compiler, "/NOCONFIG", "/V3", str(script)], check=True)
    digest = hashlib.sha256(setup.read_bytes()).hexdigest()
    (output / (setup.name + '.sha256')).write_text(f'{digest}  {setup.name}\n', encoding='utf-8', newline='\n')
    return setup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--payload', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--execute', action='store_true')
    args = parser.parse_args()
    try:
        numeric_version(args.version)
        out = runpy.run_path(str(ROOT / 'xtask-msix.py'))['fresh_output'](ROOT, args.output_dir)
        print(json.dumps({'execute': args.execute, 'version': args.version, 'output': str(out), 'nsis': NSIS_VERSION, 'static_crt': True}, indent=2))
        if args.execute:
            if os.name != 'nt':
                raise ValueError('Native Windows NSIS build required')
            build(args.payload.resolve(), out, args.version)
        return 0
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'Installer refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
