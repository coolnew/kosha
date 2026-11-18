# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Verify example authoring preserves user additions and is idempotent."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class GeneratorTests(unittest.TestCase):
    def test_custom_cases_survive_and_regeneration_is_idempotent(self):
        directory=Path(__file__).parent
        custom={"id":"custom-sentinel","input":"custom literal","dsl":'name ~ "custom literal"',
                "entities":[{"kind":"Literal","span":[0,14],"text":"custom literal","value":"custom literal"}]}
        with tempfile.TemporaryDirectory() as scratch:
            corpus=Path(scratch)/"corpus.jsonl"
            corpus.write_text((directory/"corpus.jsonl").read_text(encoding="utf-8")+json.dumps(custom)+"\n",encoding="utf-8",newline="\n")
            subprocess.run([sys.executable,str(directory/"generate_examples.py"),str(corpus)],check=True,capture_output=True)
            first=corpus.read_bytes()
            self.assertIn(custom,[json.loads(line) for line in first.decode("utf-8").splitlines()])
            subprocess.run([sys.executable,str(directory/"generate_examples.py"),str(corpus)],check=True,capture_output=True)
            self.assertEqual(first,corpus.read_bytes())


if __name__=="__main__":
    unittest.main()
