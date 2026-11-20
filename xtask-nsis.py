#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Fetch a hash-pinned portable NSIS compiler; no system installation. Preview by default."""

import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import stat
import sys
import urllib.request
import zipfile

URL = "https://downloads.sourceforge.net/project/nsis/NSIS%203/3.13/nsis-3.13.zip"
SHA256 = "ba63dffc4410ee89193e1cb5a41989991bd77c61068da17e3156d136b7b0b3d8"


def extract(data, output):
    if hashlib.sha256(data).hexdigest() != SHA256:
        raise ValueError("NSIS compiler download SHA-256 mismatch")
    if output.exists():
        raise ValueError("Tool output exists; preserve it and select a fresh directory")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = set()
        for entry in archive.infolist():
            name = PurePosixPath(entry.filename)
            if (name.is_absolute() or '..' in name.parts or '\\' in entry.filename or ':' in entry.filename
                    or not name.parts or name.parts[0] != 'nsis-3.13'
                    or stat.S_ISLNK(entry.external_attr >> 16) or entry.filename in names):
                raise ValueError("Unsafe NSIS archive entry")
            names.add(entry.filename)
        if sum(i.file_size for i in archive.infolist()) > 64 * 1024 * 1024:
            raise ValueError("Unexpected NSIS archive expansion size")
        output.mkdir(parents=True)
        archive.extractall(output)
    return output / 'nsis-3.13/makensis.exe'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir', type=Path, required=True)
    parser.add_argument('--execute', action='store_true')
    args = parser.parse_args()
    try:
        output = args.output_dir.resolve()
        if output.exists() or any(p.is_symlink() or getattr(p, 'is_junction', lambda: False)() for p in (args.output_dir, *args.output_dir.parents)):
            raise ValueError('Use a fresh tool output without links')
        print(json.dumps({'execute': args.execute, 'url': URL, 'sha256': SHA256, 'output': str(output)}, indent=2))
        if args.execute:
            request = urllib.request.Request(URL, headers={'User-Agent': 'Kosha-build-tools'})
            with urllib.request.urlopen(request, timeout=60) as response:
                if not response.url.startswith('https://'):
                    raise ValueError('Compiler download must use HTTPS')
                data = response.read(8 * 1024 * 1024 + 1)
            if len(data) > 8 * 1024 * 1024:
                raise ValueError('Compiler download exceeds size limit')
            compiler = extract(data, output)
            # Only GitHub CI uses this file; never change global/user PATH.
            import os
            if os.environ.get('GITHUB_PATH'):
                with open(os.environ['GITHUB_PATH'], 'a', encoding='utf-8') as target:
                    target.write(str(compiler.parent) + '\n')
            print(str(compiler))
        return 0
    except (ValueError, OSError, zipfile.BadZipFile) as error:
        print(f'NSIS tools refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
