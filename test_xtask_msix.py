# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import importlib.util
from contextlib import redirect_stdout
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("msix", Path(__file__).with_name("xtask-msix.py"))
msix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(msix)


class MsixTests(unittest.TestCase):
    def test_dry_run_creates_nothing_and_never_runs_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            (root / "Cargo.toml").write_text('[workspace.package]\nversion="0.10.4"\n', encoding="utf-8")
            argv = ["xtask-msix.py", "--identity-name", "Example.Kosha", "--publisher", "CN=Example",
                    "--publisher-display-name", "Example", "--binary-dir", "not-built"]
            with patch.object(msix, "ROOT", root), patch.object(msix.sys, "argv", argv), \
                 patch.object(msix.subprocess, "run") as run, \
                 patch.object(msix.subprocess, "check_output") as output, redirect_stdout(io.StringIO()):
                self.assertEqual(msix.main(), 0)
            run.assert_not_called()
            output.assert_not_called()
            self.assertFalse((root / "dist").exists())

    def test_numeric_versions_preserve_stable_order_and_reject_colliding_previews(self):
        self.assertEqual(msix.numeric_version("0.10.4"), "1.10.4.0")
        self.assertEqual(msix.numeric_version("1.0.0"), "2.0.0.0")
        for value in ["0.10.4-rc.1", "0.10.4+local", "01.2.3", "65535.0.0", "1.65536.0"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                msix.numeric_version(value)

    def test_development_identity_alias_and_capabilities_are_explicit(self):
        for elevation in [False, True]:
            xml = msix.manifest("Example.KoshaApp", "CN=Example & Co", "Example <Publisher>", "0.10.4", elevation)
            root = ET.fromstring(xml)
            identity = root.find(f"{{{msix.FOUNDATION}}}Identity")
            self.assertEqual(identity.attrib["Name"], "Example.KoshaApp.Development")
            self.assertEqual(identity.attrib["Publisher"], "CN=Example & Co")
            namespaces = {"f": msix.FOUNDATION, **msix.NS}
            apps = root.findall("f:Applications/f:Application", namespaces)
            self.assertEqual({a.attrib["Executable"] for a in apps}, {"kosha.exe", "kosha-gui.exe"})
            for app in apps:
                self.assertEqual(app.attrib[f"{{{msix.NS['uap10']}}}TrustLevel"], "mediumIL")
            aliases = {alias.attrib["Alias"] for alias in root.findall(".//uap5:ExecutionAlias", namespaces)}
            self.assertEqual(aliases, {"kosha-msix-dev.exe", "kosha-gui-msix-dev.exe"})
            capabilities = {e.attrib["Name"] for e in root.findall("f:Capabilities/rescap:Capability", namespaces)}
            self.assertEqual(capabilities, {"runFullTrust", "allowElevation"} if elevation else {"runFullTrust"})

    def test_identity_and_publisher_are_not_guessed(self):
        for identity, publisher in [("bad/name", "CN=Example"), ("A" * 39, "CN=Example"),
                                     ("Example.Kosha", "Example"), ("Example.Kosha", "CN=")]:
            with self.subTest(identity=identity), self.assertRaises(ValueError):
                msix.manifest(identity, publisher, "Display", "0.10.4", False)

    def test_unvirtualized_runtime_declares_its_restricted_capability(self):
        root = ET.fromstring(msix.manifest("Example.Kosha", "CN=Example", "Display", "0.10.4", True, True, True))
        namespaces = {"f": msix.FOUNDATION, **msix.NS}
        for app in root.findall("f:Applications/f:Application", namespaces):
            self.assertEqual(app.attrib[f"{{{msix.NS['uap10']}}}RuntimeBehavior"], "win32App")
        capabilities = {e.attrib["Name"] for e in root.findall("f:Capabilities/rescap:Capability", namespaces)}
        self.assertEqual(capabilities, {"runFullTrust", "allowElevation", "unvirtualizedResources"})

    def test_store_upload_preserves_registered_identity_and_has_no_dev_alias(self):
        root = ET.fromstring(msix.manifest("Example.Kosha", "CN=Example", "Assigned Publisher", "0.10.4", True, False, True, True))
        namespaces = {"f": msix.FOUNDATION, **msix.NS}
        self.assertEqual(root.find("f:Identity", namespaces).get("Name"), "Example.Kosha")
        self.assertEqual(root.find("f:Properties/f:PublisherDisplayName", namespaces).text, "Assigned Publisher")
        self.assertEqual(root.find("f:Properties/f:DisplayName", namespaces).text, "KoshaApp")
        self.assertEqual({a.get("Alias") for a in root.findall(".//uap5:ExecutionAlias", namespaces)}, {"kosha.exe", "kosha-gui.exe"})
        for probe, identity in [(True, "Example.Kosha"), (False, "Example.Kosha.Development")]:
            with self.assertRaises(ValueError):
                msix.manifest(identity, "CN=Example", "Assigned", "0.10.4", True, probe, True, True)

    def test_output_stays_in_fresh_dist_child(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            expected = root / "dist/msix-test"
            self.assertEqual(msix.fresh_output(root, Path("dist/msix-test")), expected)
            for path in [Path("dist"), Path("crates"), Path("dist/../../elsewhere")]:
                with self.assertRaises(ValueError):
                    msix.fresh_output(root, path)
            expected.mkdir(parents=True)
            with self.assertRaises(ValueError):
                msix.fresh_output(root, expected)


if __name__ == "__main__":
    unittest.main()
