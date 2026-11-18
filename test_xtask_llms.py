# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("llms_generator", Path(__file__).with_name("xtask-llms.py"))
guide = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guide)


class GuideTests(unittest.TestCase):
    def test_generation_normalizes_eol_and_has_one_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "LLMS.md").write_bytes(b"# Guide\r\n\r\nUse kosha.\r\n\r\n")
            self.assertEqual(guide.generated(root), b"# Guide\n\nUse kosha.\n")

    def test_tracked_output_matches_editable_source(self):
        self.assertEqual((guide.ROOT / "llms.txt").read_bytes(), guide.generated(guide.ROOT))


if __name__ == "__main__":
    unittest.main()
