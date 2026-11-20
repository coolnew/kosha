# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import io
import hashlib
from pathlib import Path
import runpy
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parent
windows = runpy.run_path(str(ROOT / 'xtask-windows.py'))
installer = runpy.run_path(str(ROOT / 'xtask-installer.py'))
nsis = runpy.run_path(str(ROOT / 'xtask-nsis.py'))


def image(name='KERNEL32.dll', delay=False):
    data = bytearray(2048)
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 0x3c, 128)
    data[128:132] = b'PE\0\0'
    struct.pack_into('<HH', data, 132, 0x8664, 1)
    struct.pack_into('<H', data, 148, 240)
    optional = 152
    struct.pack_into('<H', data, optional, 0x20b)
    struct.pack_into('<Q', data, optional + 24, 0x140000000)
    struct.pack_into('<I', data, optional + 60, 512)
    struct.pack_into('<I', data, optional + 108, 16)
    struct.pack_into('<IIII', data, optional + 240 + 8, 1536, 4096, 1536, 512)
    index, size = (13, 64) if delay else (1, 40)
    struct.pack_into('<II', data, optional + 112 + index * 8, 4096, size)
    if delay:
        struct.pack_into('<II', data, 512, 1, 4352)
    else:
        struct.pack_into('<I', data, 512 + 12, 4352)
    data[768:768+len(name)+1] = name.encode() + b'\0'
    return bytes(data)


class WindowsDistributionTests(unittest.TestCase):
    def test_rejects_normal_and_delay_loaded_redist_but_accepts_os_dlls(self):
        for delay in [False, True]:
            for dll in ['VCRUNTIME140.dll', 'vcruntime140_1.dll', 'msvcp140.dll', 'concrt140.dll', 'vcomp140.dll', 'ucrtbased.dll']:
                with self.subTest(dll=dll, delay=delay), self.assertRaisesRegex(ValueError, 'runtime dependency'):
                    windows['verify_bytes'](image(dll, delay))
            for dll in ['KERNEL32.dll', 'ucrtbase.dll', 'api-ms-win-crt-runtime-l1-1-0.dll']:
                self.assertEqual(windows['verify_bytes'](image(dll, delay)), [dll.lower()])

    def test_malformed_images_and_bad_import_names_fail_closed(self):
        original = image()
        for cut in [0, 60, 135, 300, 520, 790]:
            with self.subTest(cut=cut), self.assertRaises(ValueError):
                windows['imports'](original[:cut])
        for name in ['../vcruntime140.dll', 'C:\\runtime.dll', 'not-a-dll']:
            with self.assertRaises(ValueError):
                windows['imports'](image(name))

    def test_static_build_preserves_lints_and_sets_explicit_target(self):
        result = windows['build_environment']({'RUSTFLAGS': '-D warnings'})
        self.assertEqual(result['CARGO_BUILD_TARGET'], windows['TARGET'])
        self.assertEqual(result['CARGO_ENCODED_RUSTFLAGS'].split('\x1f'), ['-D','warnings','-C','target-feature=+crt-static'])
        self.assertNotIn('RUSTFLAGS', result)
        self.assertEqual(windows['build_environment']({'CARGO_ENCODED_RUSTFLAGS':'-D\x1fwarnings','RUSTFLAGS':'ignored'})['CARGO_ENCODED_RUSTFLAGS'], result['CARGO_ENCODED_RUSTFLAGS'])

    def test_windows_file_versions_order_preview_stages_and_final(self):
        versions = ['0.10.7-alpha.1','0.10.7-alpha.9999','0.10.7-beta.1','0.10.7-rc.2','0.10.7-rc.10','0.10.7','0.10.8-alpha.1']
        values = [tuple(map(int,installer['numeric_version'](v).split('.'))) for v in versions]
        self.assertEqual(values, sorted(set(values)))
        for v in ['0.10.7-rc.10000','0.10.7-rc.0','0.10.7+local','1.1.65536']:
            with self.assertRaises(ValueError):
                installer['numeric_version'](v)

    def test_generated_installer_escapes_inputs_and_deletes_only_known_files(self):
        with tempfile.TemporaryDirectory() as temp:
            p = Path(temp)
            (p/'some $ name').mkdir()
            (p/'some $ name/file.txt').write_text('fixture')
            text = installer['source'](p, p/'out.exe','0.10.7-rc.2')
            self.assertIn('some $$ name',text)
            self.assertNotIn('RMDir /r',text)
            self.assertNotIn('Delete "$INSTDIR\\*',text)
            self.assertIn('RequestExecutionLevel user',text)
            self.assertIn('RegQueryValueExW',text)
            self.assertNotIn('@INSTALL@',text)
        with self.assertRaises(ValueError):
            installer['quote']('bad\nFile /r *')

    def test_compiler_download_is_pinned_and_rejects_archive_traversal(self):
        for name in ['nsis-3.13/../../evil','/absolute','other/makensis.exe']:
            stream = io.BytesIO()
            with zipfile.ZipFile(stream,'w') as z:
                z.writestr(name,b'fixture')
            data = stream.getvalue()
            with tempfile.TemporaryDirectory() as temp:
                out = Path(temp)/'tools'
                with patch.dict(nsis['extract'].__globals__, SHA256=hashlib.sha256(data).hexdigest()):
                    with self.assertRaisesRegex(ValueError,'Unsafe'):
                        nsis['extract'](data,out)
                self.assertFalse(out.exists())
        with tempfile.TemporaryDirectory() as temp, self.assertRaisesRegex(ValueError,'SHA-256'):
            nsis['extract'](b'wrong',Path(temp)/'tools')
