# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("ci_selection", ROOT / "xtask-ci.py")
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


class SelectionTests(unittest.TestCase):
    def test_gate_accepts_only_intentional_skips(self):
        jobs = {name: {"result": "skipped"} for name in ("test", "musl", "reference")}
        jobs.update({"changes": {"result": "success", "outputs": {"full": "false", "reference": "false"}},
                     "hygiene": {"result": "success"}})
        ci.require_selected(jobs)
        jobs["changes"]["outputs"]["full"] = "true"
        with self.assertRaises(ValueError):
            ci.require_selected(jobs)
        jobs["test"]["result"] = jobs["musl"]["result"] = "success"
        ci.require_selected(jobs)
        jobs["test"]["result"] = "cancelled"
        with self.assertRaises(ValueError):
            ci.require_selected(jobs)
        jobs["changes"]["outputs"] = {}
        with self.assertRaises(ValueError):
            ci.require_selected(jobs)

    def test_gate_requires_reference_and_hygiene_checks(self):
        jobs = {"changes": {"result": "success", "outputs": {"full": "false", "reference": "true"}},
                "hygiene": {"result": "success"}, "reference": {"result": "failure"}}
        with self.assertRaises(ValueError):
            ci.require_selected(jobs)
        jobs["reference"]["result"] = "success"
        ci.require_selected(jobs)
        jobs["hygiene"]["result"] = "failure"
        with self.assertRaises(ValueError):
            ci.require_selected(jobs)

    def test_prose_and_images_skip_rust(self):
        r = ci.classify(["README.md", "PRIVACY.md", "docs/manual/images/gui-search-dark.png", "docs/manual/linux.md"])
        self.assertFalse(r["full"] or r["reference"] or r["coverage"])

    def test_embedded_and_executable_manuals_run_reference_tests(self):
        for p in ci.REFERENCE_DOCS:
            with self.subTest(path=p):
                self.assertEqual((ci.classify([p])["reference"], ci.classify([p])["coverage"]), (True, True))

    def test_unknown_and_build_inputs_require_full_matrix(self):
        for p in ["crates/kosha-core/src/scan.rs", "Cargo.lock", "assets/kosha.ico",
                  ".github/workflows/ci.yml", "marketplace/indexes/recipes.jsonl",
                  "xtask-ci.py", "test_xtask_ci.py", "new-input.dat", "docs/new-script.py",
                  "README.md\nfull=false", "docs/manual/file.MD"]:
            with self.subTest(path=p):
                self.assertTrue(ci.classify([p])["full"])

    def test_python_checks_stay_in_hygiene(self):
        self.assertFalse(ci.classify(["xtask-package.py", "test_xtask_release.py"])["full"])

    def test_mixed_changes_cannot_hide_code(self):
        r = ci.classify(["README.md", "docs/manual/query-language.md", "Cargo.toml"])
        self.assertTrue(r["full"])
        self.assertFalse(r["reference"])

    def test_manual_schedule_merge_queue_and_bad_history_run_full(self):
        for event in ["workflow_dispatch", "schedule", "merge_group", "push", "pull_request"]:
            self.assertTrue(ci.selection(ROOT, event, {})["full"])

    def test_new_embedded_manual_requires_policy_review(self):
        for source in (ROOT / "crates").rglob("*.rs"):
            for rel in re.findall(r'include_str!\s*\(\s*"([^"]+\.md)"\s*\)', source.read_text(encoding="utf-8")):
                path = (source.parent / rel).resolve().relative_to(ROOT).as_posix()
                self.assertTrue(ci.classify([path])["full"] or path in ci.REFERENCE_DOCS,
                                f"Embedded manual needs explicit policy: {path}")

    def test_git_ranges_include_prior_pr_code_and_both_sides_of_renames(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-b", "main")
            git("config", "user.name", "CI Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "README.md").write_text("first\n")
            git("add", "."); git("commit", "-m", "base")
            base = git("rev-parse", "HEAD")
            (root / "code.rs").write_text("source\n")
            git("add", "."); git("commit", "-m", "code")
            code = git("rev-parse", "HEAD")
            (root / "README.md").write_text("second\n")
            git("add", "."); git("commit", "-m", "docs")
            head = git("rev-parse", "HEAD")
            self.assertFalse(ci.selection(root, "push", {"before": code, "after": head})["full"])
            self.assertTrue(ci.selection(root, "pull_request", {
                "pull_request": {"base": {"sha": base}, "head": {"sha": head}}})["full"])
            (root / "code.rs").rename(root / "CHANGELOG.md")
            git("add", "-A"); git("commit", "-m", "rename code to prose")
            self.assertTrue(ci.selection(root, "push", {"before": head, "after": git("rev-parse", "HEAD")})["full"])
