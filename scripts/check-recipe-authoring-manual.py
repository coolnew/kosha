#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Validate the manual's complete recipe example using an existing Kosha CLI.

Extracts pack/manifest/corpus fences into an isolated temporary directory under
target/. Generates and checks indexes, verifies exact DSL and source_opened=false,
and checks Recipe entities and UTF-8 spans. No scans, installs, network or Git
operations. Run from any directory; no Cargo build is performed.
"""

import argparse
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
MANUAL = ROOT / 'docs/manual/recipe-authoring.md'


def verify(cli, workspace):
    spec = importlib.util.spec_from_file_location('kosha_marketplace_builder', ROOT / 'xtask-marketplace.py')
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
    text = MANUAL.read_text(encoding='utf-8')
    blocks = re.findall(r'```json\n(.*?)\n```', text, re.S)
    corpus = re.findall(r'```jsonl\n(.*?)\n```', text, re.S)
    if len(blocks) != 2 or len(corpus) != 1:
        raise ValueError('Expected exactly one complete pack, manifest and JSONL corpus in the authoring guide')
    pack, manifest = map(json.loads, blocks)
    if pack['metadata'] != manifest['metadata']:
        raise ValueError('Manual pack and manifest metadata disagree')
    artifact = builder.safe(workspace, manifest['path'])
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_bytes((blocks[0] + '\n').encode('utf-8'))
    (artifact.parent / 'manifest.json').write_bytes((blocks[1] + '\n').encode('utf-8'))
    builder.safe(workspace, manifest['cases']).write_bytes((corpus[0] + '\n').encode('utf-8'))

    generator = [sys.executable, str(ROOT / 'xtask-marketplace.py'), '--root', str(workspace), '--cli', str(cli)]
    subprocess.run(generator, check=True)
    subprocess.run(generator + ['--check'], check=True)
    count = 0
    for case in map(json.loads, corpus[0].splitlines()):
        result = subprocess.run(
            [str(cli), 'search', '--describe', '--json', '--recipes', str(artifact), case['input']],
            capture_output=True, text=True, encoding='utf-8',
        )
        if 'dsl' in case:
            if result.returncode != 0:
                raise ValueError(f"Describe failed for {case['input']!r}: {result.stderr}")
            output = json.loads(result.stdout)
            if output['source_opened'] is not False or output['dsl'] != case['dsl']:
                raise ValueError(f"Unexpected source access or DSL for {case['input']!r}")
            literal_fallback = 'name ~ ' + json.dumps(case['input'], ensure_ascii=False)
            if case['dsl'] != literal_fallback and not any(e['kind'] == 'Recipe' for e in output['entities']):
                raise ValueError(f"Expected a Recipe entity for {case['input']!r}, not a built-in fallback")
            raw = case['input'].encode('utf-8')
            for entity in output['entities']:
                start, end = entity['start_byte'], entity['end_byte']
                if not 0 <= start <= end <= len(raw) or raw[start:end].decode('utf-8') != entity['text']:
                    raise ValueError(f"Invalid UTF-8 entity span for {case['input']!r}")
        elif result.returncode != 1 or not result.stderr.strip():
            raise ValueError(f"Expected an explained failure for {case['input']!r}")
        count += 1
    print(f'Authoring manual verified: pack, manifest, {count} corpus cases and entity spans; no scans')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', type=Path, required=True, help='Existing Kosha CLI executable; this script never builds it')
    args = parser.parse_args()
    cli = args.cli.resolve()
    if not cli.is_file():
        parser.error(f'CLI executable is missing: {cli}')
    try:
        scratch = ROOT / 'target'
        if scratch.is_symlink() or getattr(scratch, 'is_junction', lambda: False)() or not scratch.resolve().is_relative_to(ROOT.resolve()):
            raise ValueError('Manual fixture directory must stay inside the checkout without links')
        scratch.mkdir(exist_ok=True)
        # The cleanup owns only this freshly allocated workspace-local directory.
        with tempfile.TemporaryDirectory(prefix='manual-authoring-', dir=scratch) as directory:
            verify(cli, Path(directory))
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f'Authoring manual verification failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
