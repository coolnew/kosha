# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("docs_links", Path(__file__).with_name("xtask-docs.py"))
docs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(docs)


class LinkTests(unittest.TestCase):
    def test_relative_images_spaces_directories_anchors_and_external_links(self):
        files = {"docs/manual/a.md": b'[b](b.md#section) ![img](images/a.png) [space](<two words.md>) [folder](images/) [external](https://example.invalid/) [here](#a)',
                 "docs/manual/b.md": b"", "docs/manual/two words.md": b"",
                 "docs/manual/images/a.png": b"png"}
        self.assertEqual(docs.broken_links(files), [])

    def test_excluded_private_targets_and_missing_images_are_not_accepted(self):
        files = {"README.md": b'[internal](docs/dev/private.md) ![gone](gone.png) [escape](../outside.md)'}
        self.assertEqual(len(docs.broken_links(files)), 3)

    def test_fenced_examples_are_not_live_links(self):
        self.assertEqual(docs.broken_links({"README.md": b'```md\n[example](not-real.md)\n```\n'}), [])
