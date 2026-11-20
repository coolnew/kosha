# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
from contextlib import redirect_stdout
import io
import hashlib
import json
import os
import plistlib
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile
from test_windows_distribution import image
from urllib.error import HTTPError, URLError


ROOT = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), ROOT / (name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


release = load("xtask-release")
package = load("xtask-package")


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL).decode().strip()


class ReleaseTests(unittest.TestCase):
    def test_numbered_previews_publish_without_latest_or_store_packages(self):
        for version in ["0.10.7-alpha.1", "0.10.7-beta.12", "0.10.7-rc.2"]:
            self.assertEqual(release.github_release_policy(version),
                             {"prerelease": True, "publish_preview": True, "make_latest": "false", "msix_store": False})
        self.assertEqual(release.github_release_policy("0.10.7"),
                         {"prerelease": False, "publish_preview": False, "make_latest": "false", "msix_store": True})
        self.assertFalse(release.github_release_policy("0.10.7+build-7")["msix_store"])
        for version in ["0.10.7-rc", "0.10.7-rc.0", "0.10.7-rc.01", "0.10.7-preview.1", "0.10.7-rc.1+build.2", "0.10.7-rc.100000"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.github_release_policy(version)

    def test_preview_counter_uses_numeric_order_and_does_not_reuse_final_targets(self):
        tags = ["v0.10.7-rc.1", "v0.10.7-rc.9", "v0.10.7-rc.10", "v0.10.7-beta.15", "v0.10.8-rc.90"]
        self.assertEqual(release.next_preview_version("0.10.7", tags), "0.10.7-rc.11")
        self.assertEqual(release.next_preview_version("0.10.7", tags, "beta"), "0.10.7-beta.16")
        self.assertEqual(release.next_preview_version("0.10.7", [], "alpha"), "0.10.7-alpha.1")
        for target in ["0.10.7-rc.1", "0.10.7+build.1", "v0.10.7"]:
            with self.assertRaises(ValueError):
                release.next_preview_version(target, [])
        with self.assertRaisesRegex(ValueError, "Final target"):
            release.next_preview_version("0.10.7", ["v0.10.7"])

    def test_preview_preparation_updates_versions_through_multiple_candidates_and_final(self):
        self.project()
        for version in ["0.10.0-rc.1", "0.10.0-rc.2", "0.10.0-rc.10", "0.10.0"]:
            updates = release.preparation(self.root, version, "Candidate notes", "2026-11-19")
            for name, data in updates.items():
                (self.root / name).write_bytes(data)
            self.assertEqual(load("xtask-version").validate(self.root, "v" + version), version)

    def test_release_lookup_treats_only_404_as_absent_and_refuses_redirects(self):
        environment = {"GITHUB_REPOSITORY": "coolnew/kosha", "GH_TOKEN": "fake-workflow-token"}
        with patch.dict(release.os.environ, environment, clear=True), patch.object(release, "build_opener") as opener:
            for code in [404, 403, 500, 302]:
                opener.return_value.open.side_effect = HTTPError("https://api.github.com/", code, "failed", {}, None)
                if code == 404:
                    self.assertIsNone(release.github_release("v0.10.7-rc.1"))
                else:
                    with self.assertRaisesRegex(ValueError, "HTTP " + str(code)):
                        release.github_release("v0.10.7-rc.1")
            opener.return_value.open.side_effect = URLError("fake-workflow-token")
            with self.assertRaises(ValueError) as failure:
                release.github_release("v0.10.7-rc.1")
            self.assertNotIn("fake-workflow-token", str(failure.exception))
        handler = release._NoGitHubRedirect()
        self.assertIsNone(handler.redirect_request(None, None, 302, "redirect", {}, "https://elsewhere.invalid"))

    def test_release_lookup_confines_credentials_and_checks_tag_identity(self):
        with patch.dict(release.os.environ, {"GITHUB_REPOSITORY": "other/repo", "GH_TOKEN": "fake"}), \
             patch.object(release, "build_opener") as opener:
            with self.assertRaises(ValueError):
                release.github_release("v0.10.7-rc.1")
            opener.assert_not_called()
        with patch.dict(release.os.environ, {"GITHUB_REPOSITORY": "coolnew/kosha", "GH_TOKEN": "fake"}), \
             patch.object(release, "build_opener") as opener:
            response = opener.return_value.open.return_value.__enter__.return_value
            response.read.return_value = b'{"tag_name":"v0.10.7-rc.1","draft":true}'
            self.assertTrue(release.github_release("v0.10.7-rc.1")["draft"])
            self.assertEqual(opener.return_value.open.call_args.args[0].full_url,
                             "https://api.github.com/repos/coolnew/kosha/releases/tags/v0.10.7-rc.1")
            response.read.return_value = b'{"tag_name":"v0.10.7-rc.2","draft":true}'
            with self.assertRaisesRegex(ValueError, "selected tag"):
                release.github_release("v0.10.7-rc.1")

    def test_draft_lookup_uses_upload_id_and_still_checks_tag_and_id(self):
        environment = {"GITHUB_REPOSITORY": "coolnew/kosha", "GH_TOKEN": "fake-workflow-token"}
        with patch.dict(release.os.environ, environment, clear=True), patch.object(release, "build_opener") as opener:
            response = opener.return_value.open.return_value.__enter__.return_value
            info = {"id": 123, "tag_name": "v0.10.7-rc.2", "draft": True, "prerelease": True}
            response.read.return_value = json.dumps(info).encode()
            self.assertEqual(release.github_release("v0.10.7-rc.2", "123"), info)
            self.assertEqual(opener.return_value.open.call_args.args[0].full_url,
                             "https://api.github.com/repos/coolnew/kosha/releases/123")
            for bad in [dict(info, id=124), dict(info, id=True), dict(info, tag_name="v0.10.7-rc.1")]:
                response.read.return_value = json.dumps(bad).encode()
                with self.assertRaises(ValueError):
                    release.github_release("v0.10.7-rc.2", "123")
            for bad_id in ["", "0", "01", "../tags/v0.10.7-rc.2", "123?token=secret"]:
                with self.assertRaisesRegex(ValueError, "positive integer"):
                    release.github_release("v0.10.7-rc.2", bad_id)
            opener.return_value.open.side_effect = HTTPError("https://api.github.com/", 404, "missing", {}, None)
            self.assertIsNone(release.github_release("v0.10.7-rc.2", "123"))
            with self.assertRaises(ValueError):
                release.verify_uploaded_release(self.root, None, "v0.10.7-rc.2")

    def test_published_releases_are_never_reopened_or_overwritten(self):
        with patch.object(release, "github_release", return_value={"draft": False}):
            with self.assertRaisesRegex(ValueError, "already published"):
                release.require_unpublished_release("v0.10.7-rc.1")
        for existing in [None, {"draft": True}]:
            with patch.object(release, "github_release", return_value=existing):
                release.require_unpublished_release("v0.10.7-rc.1")

    def test_uploaded_assets_must_match_names_sizes_and_github_sha256_before_publish(self):
        (self.root / "archive.zip").write_bytes(b"candidate")
        info = {"tag_name": "v0.10.7-rc.1", "draft": True, "prerelease": True,
                "assets": [{"name": "archive.zip", "size": 9, "state": "uploaded",
                            "digest": "sha256:" + hashlib.sha256(b"candidate").hexdigest()}]}
        release.verify_uploaded_release(self.root, info, "v0.10.7-rc.1")
        for field, value in [("size", 10), ("digest", None), ("state", "starter"), ("name", "unexpected.zip")]:
            broken = dict(info, assets=[dict(info["assets"][0], **{field: value})])
            with self.subTest(field=field), self.assertRaises(ValueError):
                release.verify_uploaded_release(self.root, broken, "v0.10.7-rc.1")
        for broken in [dict(info, draft=False), dict(info, prerelease=False), dict(info, assets=[]),
                       dict(info, assets=info["assets"] * 2), dict(info, tag_name="v0.10.7-rc.2")]:
            with self.assertRaises(ValueError):
                release.verify_uploaded_release(self.root, broken, "v0.10.7-rc.1")

    def test_release_channel_uses_semver_suffix_not_major_zero_or_build_metadata(self):
        for version, expected in [("0.10.3", False), ("1.0.0", False),
                                  ("0.10.4-rc.1", True), ("1.0.0-beta.2+build.7", True),
                                  ("0.10.4+build-with-hyphens", False)]:
            with self.subTest(version=version):
                self.assertEqual(release.is_prerelease(version), expected)
        with self.assertRaises(ValueError):
            release.is_prerelease("v0.10.3")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        # Windows temp paths can use RUNNER~1 while resolve()/Git use the full
        # name; macOS /var can likewise resolve through /private/var.
        self.root = Path(self.temp.name).resolve()

    def project(self):
        (self.root / "crates/core").mkdir(parents=True)
        (self.root / "crates/core/Cargo.toml").write_text('[package]\nname="kosha-core"\nversion.workspace=true\n', encoding="utf-8")
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers=["crates/core"]\n[workspace.package]\nversion = "0.9.0"\n[workspace.dependencies]\nkosha-core = {path="crates/core",version = "0.9.0"}\nexternal = {version = "0.9.0"}\n', encoding="utf-8")
        (self.root / "Cargo.lock").write_text('version = 4\n\n[[package]]\nname = "kosha-core"\nversion = "0.9.0"\n\n[[package]]\nname = "external"\nversion = "0.9.0"\nsource = "registry+https://example.invalid"\n', encoding="utf-8")
        (self.root / "CHANGELOG.md").write_text('# Changelog\n\n## [Unreleased]\n\n- Existing work must survive.\n\n## [0.9.0] — 2026-09-21\n\nBaseline.\n\n[Unreleased]: https://github.com/coolnew/kosha/compare/v0.9.0...HEAD\n', encoding="utf-8")

    def test_preparation_changes_only_local_versions_and_preserves_notes(self):
        self.project()
        original = (self.root / "Cargo.toml").read_bytes()
        updates = release.preparation(self.root, "0.9.1", "- Reviewed release summary.", "2026-11-18")
        self.assertEqual((self.root / "Cargo.toml").read_bytes(), original, "planning must not write")
        self.assertIn(b'kosha-core = {path="crates/core",version = "0.9.1"}', updates["Cargo.toml"])
        self.assertIn(b'external = {version = "0.9.0"}', updates["Cargo.toml"])
        self.assertIn(b'name = "external"\nversion = "0.9.0"', updates["Cargo.lock"])
        self.assertIn("## [0.9.1] — 2026-11-18", updates["CHANGELOG.md"].decode())
        self.assertIn(b"Existing work must survive", updates["CHANGELOG.md"])
        self.assertIn(b"v0.9.1...HEAD", updates["CHANGELOG.md"])
        with self.assertRaises(ValueError):
            release.preparation(self.root, "0.8.0", "notes", "2026-11-18")

    def test_semver_order_and_existing_changelog_entries(self):
        values = ["1.0.0-alpha.1", "1.0.0-alpha.2", "1.0.0-alpha.10", "1.0.0-beta", "1.0.0", "1.0.1"]
        self.assertEqual(sorted(values, key=release.semver_key), values)
        for value in ["v1.0.0", "1.02.3", "1.0.0-01"]:
            with self.assertRaises(ValueError):
                release.semver_key(value)
        with self.assertRaisesRegex(ValueError, "already contains"):
            release.updated_changelog("## Unreleased\n\nnotes\n\n## [1.0.0] — 2026-01-01\n", "1.0.0", "more", "2026-11-18")

    def test_output_is_confined_and_existing_artifacts_are_preserved(self):
        good = release.safe_output(self.root, Path("dist/releases/candidate"))
        self.assertEqual(good, self.root / "dist/releases/candidate")
        for bad in [self.root, self.root / ".git/objects", self.root / "dist", self.root.parent / "elsewhere"]:
            with self.assertRaises(ValueError):
                release.safe_output(self.root, bad)
        good.mkdir(parents=True)
        (good / "archive.zip").write_bytes(b"keep me")
        with self.assertRaisesRegex(ValueError, "not empty"):
            release.safe_output(self.root, good)
        self.assertEqual((good / "archive.zip").read_bytes(), b"keep me")

    def test_source_archives_match_reviewed_bytes_and_are_reproducible(self):
        files = {"README.md": b"public README\n", "crates/core/src/lib.rs": b"// code\n"}
        outputs = []
        for name in ("one", "two"):
            folder = self.root / name
            folder.mkdir()
            outputs.append(release.write_sources(files, folder, "0.9.1", 1234567890))
        for left, right in zip(*outputs):
            self.assertEqual(left.read_bytes(), right.read_bytes())
        with zipfile.ZipFile(outputs[0][0]) as archive:
            self.assertEqual(set(archive.namelist()), {"kosha-v0.9.1-source/" + p for p in files})
            self.assertTrue(all(entry.create_system == 3 for entry in archive.infolist()))
        with tarfile.open(outputs[0][1]) as archive:
            for entry in archive.getmembers():
                self.assertEqual(archive.extractfile(entry).read(), files[entry.name.removeprefix("kosha-v0.9.1-source/")])

    def test_build_target_cannot_pollute_git_or_source(self):
        self.assertEqual(release.safe_target(self.root, Path("target/release-check")), self.root / "target/release-check")
        self.assertEqual(release.safe_target(self.root, self.root.parent / "separate-build"), self.root.parent / "separate-build")
        for target in (self.root, self.root / "crates", self.root / ".git/objects", self.root.parent / "other/.git/build"):
            with self.assertRaises(ValueError):
                release.safe_target(self.root, target)

    def test_aliased_root_cannot_bypass_build_target_confinement(self):
        alias = self.root / ".." / self.root.name
        self.assertEqual(release.safe_target(alias, Path("target/check")), self.root / "target/check")
        for target in (Path("crates"), Path(".")):
            with self.assertRaises(ValueError):
                release.safe_target(alias, target)

    def test_nested_repository_selection_is_still_refused(self):
        git(self.root, "init")
        nested = self.root / "nested"
        nested.mkdir()
        with patch.object(release.sys, "argv", ["xtask-release.py", "--repo-path", str(nested)]), \
             patch.object(release.sys, "stderr", io.StringIO()) as errors:
            self.assertEqual(release.main(), 1)
        self.assertIn("repository root", errors.getvalue())

    def test_clean_guard_and_git_refs_index_are_not_repaired_or_overwritten(self):
        git(self.root, "init")
        git(self.root, "config", "user.name", "Fixture")
        git(self.root, "config", "user.email", "fixture@example.invalid")
        (self.root / "README.md").write_text("baseline", encoding="utf-8")
        git(self.root, "add", "README.md")
        git(self.root, "commit", "-s", "-m", "fixture")
        before = release.require_clean(self.root)
        (self.root / "README.md").write_text("work to preserve", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "staged, modified"):
            release.require_clean(self.root)
        git(self.root, "add", "README.md")
        with self.assertRaisesRegex(ValueError, "index changed"):
            release.protected(self.root, before)
        self.assertEqual((self.root / "README.md").read_text(), "work to preserve")
        self.assertEqual(git(self.root, "rev-parse", "HEAD"), before["head"])

    def test_failed_commands_still_check_git_protection(self):
        before = {"head": "a" * 40, "index": "same", "refs": "same", "status": ""}
        failure = subprocess.CalledProcessError(1, ["fixture"])
        with patch.object(release.subprocess, "run", side_effect=failure), \
             patch.object(release, "protected") as guard, redirect_stdout(io.StringIO()):
            with self.assertRaises(subprocess.CalledProcessError):
                release.run_command(self.root, ["fixture"], {}, before)
        guard.assert_called_once_with(self.root, before)

    def test_preflight_rejects_concurrent_git_changes(self):
        before = {"head": "a" * 40, "index": "same", "refs": "same", "status": ""}
        release.unchanged_preflight(before, dict(before))
        for key in ("head", "index", "refs"):
            after = dict(before, **{key: "changed"})
            with self.assertRaisesRegex(ValueError, "Git changed while planning"):
                release.unchanged_preflight(before, after)

    def test_public_selection_is_portable_and_private_selects_separate_checkout(self):
        with patch.object(release, "ROOT", self.root):
            self.assertEqual(release.choose_repo(public=True), self.root)
            (self.root / "publication").mkdir()
            (self.root / "publication/files.jsonl").write_text("fixture", encoding="utf-8")
            self.assertEqual(release.choose_repo(public=True), self.root.parent / "github/kosha")

    def test_binary_stamp_must_match_version_head_and_target(self):
        head = "abcdef123456" + "0" * 28
        target = "x86_64-pc-windows-msvc"
        stamp = f"kosha 0.9.0 ({head[:12]}, {target})"
        self.assertEqual(package.stamp_problems(stamp, "0.9.0", head, target), [])
        for broken, version, commit, triple in (
            (stamp, "0.9.1", head, target),
            (stamp, "0.9.0", "0" * 40, target),
            (stamp, "0.9.0", head, "aarch64-apple-darwin"),
            (stamp.replace(", ", "+, "), "0.9.0", head, target),
            ("kosha 0.9.0 (unknown, unknown)", "0.9.0", head, target),
        ):
            self.assertTrue(package.stamp_problems(broken, version, commit, triple), broken)

    def test_private_payloads_use_templates_and_exclude_internal_docs_and_env(self):
        if not (ROOT / "xtask-public.py").exists():
            self.skipTest("Private manifest exporter is intentionally absent from public source")
        git(self.root, "init")
        files = {"README.md": b"internal README", "publication/templates/README.md": b"# Public\n", "docs/manual/README.md": b"# Public manual\n", "docs/dev/private.md": b"private design", "marketplace/packages/recipes/coolnew/linux-essentials/1.0.0/pack.json": b"[]\n", "secret.txt": b"private credential fixture", "xtask-public.py": (ROOT / "xtask-public.py").read_bytes()}
        for name, data in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        rows = [{"path": name, "visibility": "public" if name in {"README.md", "docs/manual/README.md", "marketplace/packages/recipes/coolnew/linux-essentials/1.0.0/pack.json"} else "private", "reason": "fixture"} for name in files]
        rows[0]["source"] = "publication/templates/README.md"
        rows.append({"path": "publication/files.jsonl", "visibility": "private", "reason": "fixture"})
        (self.root / "publication/files.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
        git(self.root, "add", ".")
        (self.root / ".env").write_text("private untracked token fixture", encoding="utf-8")
        payload = package.public_payloads(self.root)
        self.assertEqual(payload, {"README.md": b"# Public\n", "docs/manual/README.md": b"# Public manual\n"})

    def test_public_checkout_rejects_unclassified_tracked_files(self):
        git(self.root, "init")
        (self.root / "README.md").write_text("# Public\n", encoding="utf-8")
        git(self.root, "add", "README.md")
        self.assertEqual(set(package.public_payloads(self.root)), {"README.md"})
        (self.root / ".env").write_text("not a real credential", encoding="utf-8")
        git(self.root, "add", ".env")
        with self.assertRaisesRegex(ValueError, "Unclassified"):
            package.public_payloads(self.root)

    def test_public_privacy_policy_is_kept_in_source_and_offline_documentation(self):
        git(self.root, "init")
        for name in ["README.md", "PRIVACY.md"]:
            (self.root / name).write_text("# Public document\n", encoding="utf-8")
        git(self.root, "add", "README.md", "PRIVACY.md")
        self.assertIn("PRIVACY.md", package.public_payloads(self.root))
        self.assertIn("PRIVACY.md", package.documentation_payloads(self.root))

    def test_git_recipe_packs_are_omitted_from_archive_payloads(self):
        git(self.root, "init")
        (self.root / "README.md").write_text("# Public\n", encoding="utf-8")
        for name in ["marketplace/catalog.json", "marketplace/indexes/recipes.jsonl", "marketplace/packages/recipes/coolnew/tools/1.0.0/pack.json"]:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("[]\n", encoding="utf-8")
        git(self.root, "add", ".")
        self.assertEqual(set(package.public_payloads(self.root)), {"README.md"})
        self.assertEqual(set(package.documentation_payloads(self.root)), {"README.md"})

    def test_public_source_includes_authoring_validator_and_rejects_retired_flat_packs(self):
        git(self.root, "init")
        helper = self.root / "scripts/check-recipe-authoring-manual.py"
        helper.parent.mkdir()
        helper.write_text("# public validator fixture\n", encoding="utf-8")
        git(self.root, "add", ".")
        self.assertEqual(set(package.public_payloads(self.root)), {"scripts/check-recipe-authoring-manual.py"})
        retired = self.root / "recipes/linux.json"
        retired.parent.mkdir()
        retired.write_text("[]\n", encoding="utf-8")
        git(self.root, "add", ".")
        with self.assertRaisesRegex(ValueError, "Unclassified"):
            package.public_payloads(self.root)

    def test_public_asset_cannot_smuggle_a_private_key(self):
        git(self.root, "init")
        (self.root / "assets").mkdir()
        marker = "-----BEGIN RSA " + "PRIVATE KEY-----"
        (self.root / "assets/key.pem").write_text(marker + "\nfixture, not a key\n", encoding="utf-8")
        git(self.root, "add", "assets/key.pem")
        with self.assertRaisesRegex(ValueError, "Potential credential"):
            package.public_payloads(self.root)

    def test_dry_run_never_builds_or_creates_output(self):
        (self.root / "publication").mkdir()
        (self.root / "publication/files.jsonl").write_text("fixture", encoding="utf-8")
        class Version:
            @staticmethod
            def validate(root):
                return "0.9.0"
        class Package:
            @staticmethod
            def public_payloads(root):
                return {"README.md": b"# Public\n", "CHANGELOG.md": b"# Changes\n"}
        def selected_module(path, name):
            if name == "release_versions":
                return Version
            if name == "release_windows":
                return load("xtask-windows")
            if name == "release_installer":
                return load("xtask-installer")
            return Package
        before = {"head": "a" * 40, "index": "unchanged", "refs": "unchanged", "status": ""}
        def selected_git(root, *args):
            return str(getattr(self, "reported_git_root", self.root)) if args == ("rev-parse", "--show-toplevel") else ""
        with patch.object(release.sys, "argv", ["xtask-release.py", "--repo-path", str(self.root), *getattr(self, "release_flags", [])]), \
             patch.object(release, "module", selected_module), patch.object(release, "git", selected_git), \
             patch.object(release, "git_state", return_value=before), \
             patch.object(release, "validation_commands", return_value=[["must-not-run"]]), \
             patch.object(release.subprocess, "run") as run, redirect_stdout(io.StringIO()) as output:
            self.assertEqual(release.main(), 0)
        run.assert_not_called()
        self.assertFalse((self.root / "dist").exists())
        plan = json.loads(output.getvalue())
        self.assertFalse(plan["execute"])
        if "--binary-only" in getattr(self, "release_flags", []):
            self.assertEqual(plan["archives"], ["host binary archive"] + (["NSIS offline Setup EXE (static CRT)"] if os.name == "nt" else []))

    def test_binary_only_dry_run_omits_shared_sources(self):
        self.release_flags = ["--binary-only"]
        self.test_dry_run_never_builds_or_creates_output()

    def test_dry_run_accepts_equivalent_git_root_spelling(self):
        self.reported_git_root = self.root / ".." / self.root.name
        self.test_dry_run_never_builds_or_creates_output()

    @unittest.skipUnless(os.name == "nt", "Windows short paths")
    def test_windows_short_path_root_is_accepted_without_weakening_confinement(self):
        import ctypes
        from ctypes import wintypes
        folder = self.root / "release path alias regression fixture"
        folder.mkdir()
        short_path = ctypes.WinDLL("kernel32", use_last_error=True).GetShortPathNameW
        short_path.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
        short_path.restype = wintypes.DWORD
        buffer = ctypes.create_unicode_buffer(32768)
        length = short_path(str(folder), buffer, len(buffer))
        self.assertGreater(length, 0)
        self.assertLess(length, len(buffer))
        alias = Path(buffer.value)
        if alias == folder:
            self.skipTest("This volume does not generate 8.3 aliases")
        self.assertEqual(alias.resolve(), folder.resolve())
        with self.assertRaises(ValueError):
            release.safe_target(alias, Path("crates"))
        self.assertEqual(release.safe_output(alias, Path("dist/check")), folder.resolve() / "dist/check")
        self.root = folder.resolve()
        self.reported_git_root = alias
        self.test_dry_run_never_builds_or_creates_output()


class MacDmgTests(unittest.TestCase):
    def test_mac_bundle_fields_are_numeric_ordered_and_retain_full_preview_version(self):
        versions = ["0.10.7-alpha.1", "0.10.7-alpha.99999", "0.10.7-beta.1",
                    "0.10.7-rc.1", "0.10.7-rc.10", "0.10.7", "0.10.8-alpha.1"]
        builds = []
        for version in versions:
            info = plistlib.loads(package.plist(version).encode())
            self.assertEqual(info["CFBundleShortVersionString"], version.split('-')[0])
            builds.append(tuple(map(int, info["CFBundleVersion"].split('.'))))
            self.assertEqual(info["KoshaVersion"], version)
            self.assertEqual(info["CFBundleGetInfoString"], "Kosha " + version)
        self.assertEqual(builds, sorted(set(builds)))
        for version in ["0.10.7-rc", "0.10.7-rc.100000", "0.10.99999", "0.10.7-unknown.1"]:
            with self.assertRaises(ValueError):
                package.plist(version)

    def test_mac_payload_signs_complete_bundle_and_cli_then_verifies_both(self):
        folder = Path("candidate")
        with patch.object(package, "MACOS", True), patch.object(package.subprocess, "run") as run:
            package.sign_mac_payload(folder)
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual(commands, [
            ["codesign", "--force", "--sign", "-", str(folder / "Kosha.app")],
            ["codesign", "--force", "--sign", "-", str(folder / "kosha")],
            ["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(folder / "Kosha.app")],
            ["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(folder / "kosha")],
        ])
        self.assertTrue(all(call.kwargs["check"] for call in run.call_args_list))

    def test_invalid_mac_signature_stops_packaging(self):
        with patch.object(package, "MACOS", True), patch.object(package.subprocess, "run",
             side_effect=subprocess.CalledProcessError(1, "codesign")) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                package.sign_mac_payload(Path("candidate"))
        self.assertEqual(run.call_count, 1)

    @unittest.skipUnless(package.MACOS, "Native macOS bundle signing and DMG integrity")
    def test_native_mac_bundle_seal_survives_dmg_and_detects_changed_resources(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            folder = root / "kosha-signing-test"
            app = folder / "Kosha.app/Contents"
            (app / "MacOS").mkdir(parents=True)
            (app / "Resources").mkdir()
            source = root / "main.c"
            source.write_text("int main(void) { return 0; }\n", encoding="utf-8")
            subprocess.run(["cc", str(source), "-o", str(folder / "kosha")], check=True)
            package.shutil.copy2(folder / "kosha", app / "MacOS/kosha-gui")
            (app / "Info.plist").write_text(package.plist("0.10.6"), encoding="utf-8")
            (app / "Resources/fixture.txt").write_text("original", encoding="utf-8")
            package.sign_mac_payload(folder)
            self.assertTrue((app / "_CodeSignature/CodeResources").is_file())
            image = package.archive(folder)
            unpacked = root / "unpacked"
            package.unpack_dmg(image, unpacked)
            package.verify_mac_payload(unpacked)
            (unpacked / "Kosha.app/Contents/Resources/fixture.txt").write_text("changed", encoding="utf-8")
            with self.assertRaises(subprocess.CalledProcessError):
                package.verify_mac_payload(unpacked)

    def test_mac_archive_creates_compressed_dmg_without_changing_payload(self):
        with tempfile.TemporaryDirectory() as temp:
            folder = Path(temp) / "kosha-v0.10.4-aarch64-apple-darwin"
            folder.mkdir()
            with patch.object(package, "MACOS", True), patch.object(package.shutil, "copytree"), \
                 patch.object(Path, "symlink_to") as link, patch.object(package.subprocess, "run") as run:
                path = package.archive(folder)
            self.assertEqual(path, folder.parent / (folder.name + ".dmg"))
            link.assert_called_once_with("/Applications", target_is_directory=True)
            command = run.call_args.args[0]
            self.assertEqual(command[:4], ["hdiutil", "create", "-volname", folder.name])
            self.assertEqual(command[-5:], ["-fs", "HFS+", "-format", "UDZO", str(path)])
            self.assertTrue(run.call_args.kwargs["check"])
            self.assertFalse((folder / "Applications").exists())
            path.write_bytes(b"preserve existing image")
            with patch.object(package, "MACOS", True), self.assertRaises(ValueError):
                package.archive(folder)
            self.assertEqual(path.read_bytes(), b"preserve existing image")

    def test_dmg_detaches_when_payload_copy_fails(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(package, "MACOS", True), \
             patch.object(package.subprocess, "run") as run, patch.object(Path, "is_symlink", return_value=True), \
             patch.object(package.os, "readlink", return_value="/Applications"), \
             patch.object(package.shutil, "copytree", side_effect=OSError("copy failed")):
            with self.assertRaisesRegex(OSError, "copy failed"):
                package.unpack_dmg(Path(temp) / "test.dmg", Path(temp) / "payload")
            commands = [call.args[0] for call in run.call_args_list]
            self.assertEqual([c[:2] for c in commands],
                             [["hdiutil", "verify"], ["hdiutil", "attach"], ["hdiutil", "detach"]])
            self.assertIn("-readonly", commands[1])
            self.assertIn("-nobrowse", commands[1])
            self.assertEqual(commands[1][-1], commands[2][-1])

    def test_invalid_dmg_is_never_mounted(self):
        with patch.object(package, "MACOS", True), \
             patch.object(package.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "hdiutil verify")) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                package.unpack_dmg(Path("bad.dmg"), Path("unused"))
            self.assertEqual(run.call_count, 1)
            self.assertEqual(run.call_args.args[0][:2], ["hdiutil", "verify"])

    @unittest.skipUnless(package.MACOS, "Native macOS hdiutil round trip")
    def test_native_dmg_preserves_bundle_cli_and_manual(self):
        with tempfile.TemporaryDirectory() as temp:
            folder = Path(temp) / "kosha-dmg-test"
            binary = folder / "Kosha.app/Contents/MacOS/kosha-gui"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            (folder / "kosha").write_bytes(binary.read_bytes())
            (folder / "kosha").chmod(0o755)
            (folder / "README.md").write_text("Offline manual\n", encoding="utf-8")
            image = package.archive(folder)
            unpacked = Path(temp) / "extracted"
            package.unpack_dmg(image, unpacked)
            self.assertEqual((unpacked / "README.md").read_text(), "Offline manual\n")
            for relative in ["kosha", "Kosha.app/Contents/MacOS/kosha-gui"]:
                self.assertTrue(os.access(unpacked / relative, os.X_OK))
                subprocess.run([str(unpacked / relative)], check=True)
            self.assertFalse((unpacked / "Applications").exists())


class ReleaseAssetTests(unittest.TestCase):
    commit = "a" * 40

    def seed(self, root):
        for target, extension in release.RELEASE_TARGETS.items():
            names = [f"kosha-v0.10.2-{target}{extension}"]
            if target == "x86_64-pc-windows-msvc":
                names.append("kosha-v0.10.2-x64-setup.exe")
            if target == release.SOURCE_OWNER:
                names += ["kosha-v0.10.2-source.zip", "kosha-v0.10.2-source.tar.gz"]
            entries = []
            for name in names:
                data = ("archive " + name).encode()
                digest = hashlib.sha256(data).hexdigest()
                (root / name).write_bytes(data)
                (root / (name + ".sha256")).write_text(f"{digest}  {name}\n", encoding="utf-8")
                entries.append({"name": name, "sha256": digest, "bytes": len(data)})
            manifest = {"schema": "kosha.release-candidate.v1", "project": "kosha", "version": "0.10.2",
                        "commit": self.commit, "target": target, "publication_candidate": True, "artifacts": entries}
            (root / f"release-manifest-{target}.json").write_text(json.dumps(manifest), encoding="utf-8")

    def test_three_hosts_and_one_shared_source_owner_verify_together(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            self.seed(root)
            release.verify_release_assets(root, self.commit)

    def seed_msix(self, root):
        self.seed(root)
        version = "0.10.2"
        prefix = "kosha-v" + version
        win_name = prefix + "-x86_64-pc-windows-msvc.zip"
        binaries = {"kosha.exe": image() + b"same cli", "kosha-gui.exe": image() + b"same gui"}
        with zipfile.ZipFile(root / win_name, "w") as archive:
            for name, data in binaries.items():
                archive.writestr(prefix + "-x86_64-pc-windows-msvc/" + name, data)
        win_meta_path = root / "release-manifest-x86_64-pc-windows-msvc.json"
        win_meta = json.loads(win_meta_path.read_text())
        digest = hashlib.sha256((root / win_name).read_bytes()).hexdigest()
        win_meta["artifacts"][0].update(sha256=digest, bytes=(root / win_name).stat().st_size)
        win_meta_path.write_text(json.dumps(win_meta), encoding="utf-8")
        (root / (win_name + ".sha256")).write_text(f"{digest}  {win_name}\n", encoding="utf-8")
        name = prefix + "-x64-store-upload.msix"
        msix_module = load("xtask-msix")
        with zipfile.ZipFile(root / name, "w") as archive:
            archive.writestr("AppxManifest.xml", msix_module.manifest("Example.Kosha", "CN=Example", "Example Publisher", version, True, False, True, True))
            archive.writestr("CONTACT.txt", "KoshaApp\nAuthor: Example Author\nPublisher: Example Publisher\nSupport: support@example.com\nWebsite: https://example.com/\n")
            for binary, data in binaries.items():
                archive.writestr(binary, data)
        metadata = {"schema": "kosha.msix-store-upload.v1", "package": name, "commit": self.commit,
                    "version": version, "target": "x86_64-pc-windows-msvc", "publication_candidate": True,
                    "signed": False, "diagnostic_probe": False, "runtime": "win32App",
                    "identity": "Example.Kosha", "publisher": "CN=Example", "publisher_display_name": "Example Publisher",
                    "author_name": "Example Author", "support_email": "support@example.com", "website": "https://example.com/"}
        self.refresh_msix(root, metadata)
        return metadata

    def refresh_msix(self, root, metadata):
        name = metadata["package"]
        metadata.update(sha256=hashlib.sha256((root / name).read_bytes()).hexdigest(), bytes=(root / name).stat().st_size)
        (root / (name + ".sha256")).write_text(f"{metadata['sha256']}  {name}\n", encoding="utf-8")
        (root / "msix-store-upload.json").write_text(json.dumps(metadata), encoding="utf-8")

    def test_opted_in_msix_verifies_with_identical_windows_zip_binaries(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            self.seed_msix(root)
            release.verify_release_assets(root, self.commit, expect_msix=True)
            with self.assertRaisesRegex(ValueError, "Unexpected"):
                release.verify_release_assets(root, self.commit)

    def test_msix_integrity_private_provenance_and_development_artifacts_are_rejected(self):
        for problem in ["private", "identity", "publisher", "runtime", "signature", "probe", "binary", "contact", "headless", "device", "alias", "hash", "sidecar", "missing"]:
            with self.subTest(problem=problem), tempfile.TemporaryDirectory() as d:
                root = Path(d)
                metadata = self.seed_msix(root)
                path = root / metadata["package"]
                if problem in {"signature", "probe", "binary", "contact", "headless", "device", "alias"}:
                    with zipfile.ZipFile(path) as archive:
                        files = {name: archive.read(name) for name in archive.namelist()}
                    if problem == "signature":
                        files["AppxSignature.p7x"] = b"development signature"
                    elif problem == "probe":
                        files["kosha-msix-probe.exe"] = b"private probe"
                    elif problem == "contact":
                        files["CONTACT.txt"] = b"wrong contact metadata"
                    elif problem in {"headless", "device", "alias"}:
                        import xml.etree.ElementTree as ET
                        msix_module = load("xtask-msix")
                        ns = {"f": msix_module.FOUNDATION, **msix_module.NS}
                        xml = ET.fromstring(files["AppxManifest.xml"])
                        if problem == "headless":
                            xml.find(".//uap:VisualElements", ns).set("AppListEntry", "none")
                        elif problem == "device":
                            xml.find("f:Dependencies/f:TargetDeviceFamily", ns).set("Name", "Windows.Universal")
                        else:
                            xml.find(".//uap5:Extension", ns).set("Executable", "wrong.exe")
                        files["AppxManifest.xml"] = ET.tostring(xml, encoding="utf-8", xml_declaration=True)
                    else:
                        files["kosha.exe"] = b"different binary"
                    with zipfile.ZipFile(path, "w") as archive:
                        for name, data in files.items():
                            archive.writestr(name, data)
                elif problem == "private":
                    metadata["publication_candidate"] = False
                elif problem == "identity":
                    metadata["identity"] += ".Development"
                elif problem == "publisher":
                    metadata["publisher"] = "CN=Wrong"
                elif problem == "runtime":
                    metadata["runtime"] = "packagedClassicApp"
                self.refresh_msix(root, metadata)
                if problem == "hash":
                    path.write_bytes(b"changed package")
                elif problem == "sidecar":
                    (root / (path.name + ".sha256")).write_text("wrong")
                elif problem == "missing":
                    path.unlink()
                with self.assertRaises((ValueError, OSError)):
                    release.verify_release_assets(root, self.commit, expect_msix=True)

    def test_mac_host_requires_dmg_instead_of_legacy_tar(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            self.seed(root)
            manifest_path = root / "release-manifest-aarch64-apple-darwin.json"
            manifest = json.loads(manifest_path.read_text())
            entry = manifest["artifacts"][0]
            self.assertTrue(entry["name"].endswith(".dmg"))
            entry["name"] = entry["name"].removesuffix(".dmg") + ".tar.gz"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "inventory"):
                release.verify_release_assets(root, self.commit)

    def test_integrity_provenance_and_exact_inventory_are_required(self):
        for problem in ["archive", "sidecar", "missing", "commit", "target", "version", "private", "size", "extra", "unpublished-source"]:
            with self.subTest(problem=problem), tempfile.TemporaryDirectory() as d:
                root = Path(d)
                self.seed(root)
                path = root / "release-manifest-x86_64-pc-windows-msvc.json"
                manifest = json.loads(path.read_text())
                source = root / "kosha-v0.10.2-source.zip"
                if problem == "archive":
                    source.write_bytes(b"changed")
                elif problem == "sidecar":
                    (root / (source.name + ".sha256")).write_text("bad digest")
                elif problem == "missing":
                    source.unlink()
                elif problem == "extra":
                    (root / "unreviewed.zip").write_bytes(b"extra")
                elif problem == "unpublished-source":
                    manifest["artifacts"].append({"name": source.name, "sha256": "b" * 64, "bytes": 1})
                elif problem == "private":
                    manifest["publication_candidate"] = False
                elif problem == "size":
                    manifest["artifacts"][0]["bytes"] += 1
                else:
                    manifest[problem] = {"commit": "b" * 40, "target": release.SOURCE_OWNER, "version": "0.10.3"}[problem]
                path.write_text(json.dumps(manifest), encoding="utf-8")
                with self.assertRaises(ValueError):
                    release.verify_release_assets(root, self.commit)


if __name__ == "__main__":
    unittest.main()
