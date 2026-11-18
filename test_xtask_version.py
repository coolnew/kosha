# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("version_check", Path(__file__).with_name("xtask-version.py"))
versions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(versions)


class VersionTests(unittest.TestCase):
    def test_current_tree_and_tag_match(self):
        value = versions.validate(versions.ROOT)
        self.assertEqual(versions.validate(versions.ROOT, f"v{value}"), value)
        with self.assertRaisesRegex(ValueError, "must equal"):
            versions.validate(versions.ROOT, "v999.0.0")

    def test_inconsistent_dependency_and_lockfile_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "crates/core").mkdir(parents=True)
            (root / "crates/core/Cargo.toml").write_text('[package]\nname="core"\nversion.workspace=true\n')
            def manifest(dep):
                return '[workspace]\nmembers=["crates/core"]\n[workspace.package]\nversion="1.2.3"\n[workspace.dependencies]\ncore={path="crates/core",version="' + dep + '"}\n'
            (root / "Cargo.toml").write_text(manifest("1.2.2"))
            (root / "Cargo.lock").write_text('[[package]]\nname="core"\nversion="1.2.3"\n')
            with self.assertRaisesRegex(ValueError, "Local dependency"):
                versions.validate(root)
            (root / "Cargo.toml").write_text(manifest("1.2.3"))
            (root / "Cargo.lock").write_text('[[package]]\nname="core"\nversion="1.2.2"\n')
            with self.assertRaisesRegex(ValueError, "Cargo.lock"):
                versions.validate(root)


if __name__ == "__main__":
    unittest.main()
