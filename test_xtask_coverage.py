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

    def test_scenario_evidence_requires_a_passing_named_test_and_reports_unattributed_hits(self):
        root = Path(__file__).resolve().parent
        sha = "a" * 40
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            (out / "coverage/html").mkdir(parents=True)
            (out / "coverage/html/index.html").write_text("annotated report")
            coverage.render(report(), out, sha, "coolnew/kosha", "windows")
            source = coverage.GUI
            lines = (root / source).read_text(encoding="utf-8").splitlines()
            number = next(n for n, line in enumerate(lines, 1) if line.strip() == coverage.fast_ntfs_cases()[0][3])
            lcov = f"SF:C:/{source}\nDA:{number},7\nend_of_record\n".replace("/", chr(92))
            test_log = ("test ntfs_worker::launch::worker_preflight_requires_a_whole_lettered_drive ... ok\n"
                        "test ntfs_worker::tests::malformed_requests_are_rejected_before_scanning ... ignored\n"
                        "test app::monitoring::tests::desktop_monitoring_lifecycle_keeps_observations_after_profile_removal ... ok\n"
                        "test folder_comparison::tests::saved_comparison_preserves_baseline_and_refuses_content_verification ... ok\n"
                        "     Running tests\\end_to_end.rs (target/test.exe)\n"
                        "test a_scan_reports_the_tree_it_was_given ... ok\n"
                        "     Running tests/unrelated.rs (target/other.exe)\n"
                        "test a_path_that_is_not_there_says_so_and_fails ... ok\n"
                        "     Running crates/kosha-cli/tests/search.rs (target/search.exe)\n"
                        "test freshly_scanned_files_keep_their_recorded_creation_dates ... ok\n"
                        "     Running tests/unreadable_root.rs (target/unreadable.exe)\n"
                        "test a_root_that_could_not_be_read_says_so_rather_than_reading_as_empty ... ok\n")
            coverage.scenario_report(root, lcov, test_log, out, sha, "coolnew/kosha")
            result = json.loads((out / "scenarios.json").read_text())
            by_id = {row["id"]: row for row in result["scenarios"]}
            self.assertEqual(by_id["offer-action"]["line_hits"], 7)
            self.assertEqual(by_id["offer-action"]["status"], "Documented manual observation")
            self.assertEqual(by_id["accept"]["status"], "Needs end-to-end evidence")
            self.assertEqual(by_id["cli-scan"]["status"], "CLI integration test passed")
            self.assertEqual(by_id["cli-missing-path"]["status"], "Mapped test did not pass in this run")
            self.assertEqual(by_id["windows-creation-dates"]["status"], "CLI integration test passed")
            self.assertEqual(by_id["scan-denied"]["status"], "Scanner integration test passed")
            for platform in ("linux", "macos"):
                coverage.render(report(), out, sha, "coolnew/kosha", platform)
                coverage.scenario_report(root, lcov, test_log, out, sha, "coolnew/kosha", platform)
                cases = json.loads((out / "scenarios.json").read_text())["scenarios"]
                self.assertTrue(any(row["workflow"] == "Kosha CLI: search" for row in cases))
                identifiers = {row["id"] for row in cases}
                self.assertIn("unix-symlink", identifiers)
                self.assertNotIn("offer-action", identifiers)
                self.assertEqual("macos-volume-uuid" in identifiers, platform == "macos")
                self.assertEqual("linux-rollback" in identifiers, platform == "linux")
                statuses = {row["id"]: row["status"] for row in cases}
                self.assertEqual(statuses["gui-monitoring-lifecycle"], "Component test passed")
                self.assertEqual(statuses["gui-saved-baseline"], "Component test passed")
                self.assertEqual(statuses["gui-verify-content"], "Mapped test did not pass in this run")
                self.assertGreaterEqual(len(cases), 40)
            coverage.render(report(), out, sha, "coolnew/kosha", "windows")
            self.assertEqual(by_id["ineligible"]["status"], "Component test passed")
            self.assertEqual(by_id["malformed"]["status"], "Mapped test did not pass in this run")
            with self.assertRaisesRegex(ValueError, "matching Windows measurement"):
                coverage.scenario_report(root, lcov, test_log, out, "b" * 40, "coolnew/kosha")
