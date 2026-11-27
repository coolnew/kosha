# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("website", Path(__file__).with_name("xtask-website.py"))
website = importlib.util.module_from_spec(spec)
spec.loader.exec_module(website)


class WebsiteTests(unittest.TestCase):
    def test_privacy_has_one_root_route_and_portable_manual_links(self):
        files = {"PRIVACY.md": b"[stored files](docs/manual/settings-and-files.md)",
                 "docs/manual/settings-and-files.md": b"[privacy](../../PRIVACY.md)",
                 "website/site.css": b"css", "llms.txt": b"guide"}
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            website.stage(files, out)
            self.assertEqual((out / 'privacy.md').read_text(), '[stored files](manual/settings-and-files.md)')
            self.assertIn('(../privacy.md)', (out / 'manual/settings-and-files.md').read_text())
            self.assertFalse((out / 'manual/privacy.md').exists())

    def test_manual_links_images_and_external_references_remain_portable(self):
        files = {"docs/manual/README.md": b"", "docs/manual/a.md": b"",
                 "docs/manual/images/a.png": b"", "assets/README.md": b""}
        result = website.rewrite_links('[contents](README.md) ![a](images/a.png) [install](../../assets/README.md#linux)\n'
            '```md\n[example](absent.md)\n```\n', "docs/manual/a.md", files)
        self.assertIn('(index.md)', result)
        self.assertIn('(images/a.png)', result)
        self.assertIn('https://github.com/coolnew/kosha/blob/main/assets/README.md#linux', result)
        self.assertIn('[example](absent.md)', result)
        with self.assertRaises(ValueError):
            website.rewrite_links('[private](../dev/private.md)', 'docs/manual/a.md', files)

    def test_staging_does_not_publish_private_docs_or_unrelated_sources(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            website.stage({'website/index.md': b'# Home', 'website/site.css': b'css', 'llms.txt': b'agent',
                           'docs/manual/README.md': b'# Manual', 'docs/dev/private.md': b'secret',
                           'CLAUDE.md': b'private', 'crates/anything.rs': b'source'}, out)
            self.assertEqual({p.relative_to(out).as_posix() for p in out.rglob('*') if p.is_file()},
                             {'index.md', 'styles/site.css', 'manual/index.md', 'llms.txt'})

    def test_composed_report_keeps_provenance_and_legacy_links(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            src, out = root / 'input', root / 'site'
            (src / 'coverage/html').mkdir(parents=True)
            (src / 'coverage/html/index.html').write_text('annotated source')
            (src / 'index.html').write_text('<h1>Coverage</h1>')
            (src / 'coverage.svg').write_text('<svg/>')
            data = {'schema': 'kosha.coverage.v1', 'commit': 'a' * 40, 'repository': 'coolnew/kosha',
                    'platform': 'linux', 'scope': 'Rust workspace; test'}
            (src / 'summary.json').write_text(json.dumps(data))
            website.compose_coverage(src, out, 'a' * 40)
            self.assertEqual(json.loads((out / 'summary.json').read_text()), data)
            self.assertIn('Kosha home', (out / 'coverage/index.html').read_text())
            self.assertTrue((out / 'coverage/coverage/html/index.html').is_file())
            self.assertIn('http-equiv="refresh"', (out / 'coverage/html/index.html').read_text())
            with self.assertRaises(ValueError):
                website.compose_coverage(src, out, 'b' * 40)
            data['platform'] = 'windows'
            (src / 'summary.json').write_text(json.dumps(data))
            with self.assertRaises(ValueError):
                website.compose_coverage(src, out)

    def test_no_measurement_is_explicitly_unavailable(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            website.compose_coverage(None, out)
            self.assertIn('does not mean zero coverage', (out / 'coverage/index.html').read_text())
            self.assertFalse((out / 'coverage.svg').exists())

    def test_generated_link_validation_detects_missing_assets_and_escapes(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / 'index.html').write_text('<a href="https://example.invalid/">external</a><img src="/kosha/image.png">')
            with self.assertRaises(ValueError):
                website.validate_site(root)
            (root / 'image.png').write_bytes(b'png')
            website.validate_site(root)
            (root / 'index.html').write_text('<a href="../outside">escape</a>')
            with self.assertRaises(ValueError):
                website.validate_site(root)

    def test_matrix_composition_preserves_platform_reports_and_refuses_mixed_commits(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            src, out = root / "input", root / "site"
            sha = "a" * 40
            for platform in ("linux", "windows", "macos"):
                report = src / platform
                (report / "coverage/html").mkdir(parents=True)
                (report / "coverage/html/index.html").write_text("annotated source")
                (report / "index.html").write_text("<h1>Platform</h1>")
                (report / "coverage.svg").write_text("<svg/>")
                (report / "summary.json").write_text(json.dumps({
                    "schema": "kosha.coverage.v1", "commit": sha, "repository": "coolnew/kosha",
                    "platform": platform, "scope": "Rust workspace; test",
                    "metrics": {"lines": {"covered": 8, "count": 10}},
                }))
            (src / "windows/scenarios.html").write_text("<h1>Scenarios</h1>")
            (src / "windows/scenarios.json").write_text(json.dumps({
                "schema": "kosha.scenarios.v1", "commit": sha, "platform": "windows", "repository": "coolnew/kosha",
            }))
            website.compose_coverage(src, out, sha)
            page = (out / "coverage/index.html").read_text()
            for label in ("Linux", "Windows", "macOS", "Fast NTFS"):
                self.assertIn(label, page)
            self.assertIn("80.00%", page)
            self.assertIn("../linux/coverage/html/index.html", (out / "coverage/html/index.html").read_text())
            summary = src / "macos/summary.json"
            summary.write_text(summary.read_text().replace(sha, "b" * 40))
            with self.assertRaisesRegex(ValueError, "same source commit"):
                website.compose_coverage(src, out)
