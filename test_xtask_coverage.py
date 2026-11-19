# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("coverage_report", Path(__file__).with_name("xtask-coverage.py"))
coverage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(coverage)


def report(count=100, covered=83):
    return {"type": "llvm.coverage.json.export", "data": [{"totals": {
        kind: {"count": count, "covered": covered, "percent": 99.99}
        for kind in ("lines", "functions", "regions")}}]}


class CoverageTests(unittest.TestCase):
    def test_percent_comes_from_counts_not_untrusted_percent(self):
        self.assertEqual(coverage.metrics(report())["lines"]["percent"], 83)

    def test_invalid_or_empty_counts_do_not_make_a_green_badge(self):
        for r in [report(0, 0), report(2, 3), report(-1, 0), report(100, True), {"data": []}]:
            with self.subTest(report=r), self.assertRaises(ValueError):
                coverage.metrics(r)

    def test_rendered_outputs_have_commit_scope_and_valid_svg(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            (out / "coverage/html").mkdir(parents=True)
            (out / "coverage/html/index.html").write_text("annotated report")
            sha = "a" * 40
            data, summary = coverage.render(report(), out, sha, "coolnew/kosha", "linux")
            self.assertEqual(json.loads((out / "summary.json").read_text())["commit"], sha)
            self.assertEqual(data["platform"], "linux")
            self.assertIn("Windows-only", (out / "index.html").read_text())
            self.assertIn("83.0%", (out / "coverage.svg").read_text())
            self.assertIn(sha, summary)
            ET.parse(out / "coverage.svg")
            data, _ = coverage.render(report(), out, sha, "coolnew/kosha", "windows", "smoke")
            self.assertIn("not project coverage", data["scope"])

    def test_missing_html_and_invalid_provenance_are_refused(self):
        with tempfile.TemporaryDirectory() as d:
            for sha, repo, platform in [("bad", "coolnew/kosha", "linux"),
                                        ("a" * 40, '<script>/x', "linux"),
                                        ("a" * 40, "coolnew/kosha", "untrusted"),
                                        ("a" * 40, "coolnew/kosha", "linux")]:
                with self.assertRaises(ValueError):
                    coverage.render(report(), Path(d), sha, repo, platform)
