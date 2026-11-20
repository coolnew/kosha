#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Explicit native Setup install/CLI/uninstall smoke; refuses an existing desktop installation."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

KEY = r'Software\Microsoft\Windows\CurrentVersion\Uninstall\KoshaDesktop'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--setup', type=Path, required=True)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--upgrade-setup', type=Path, help='Optional newer installer fixture for upgrade/downgrade testing')
    args = parser.parse_args()
    if not args.execute:
        print(json.dumps({'execute': False, 'setup': str(args.setup), 'scope': 'current user', 'changes': 'temporary app installation and owned user PATH entry'}))
        return
    if os.name != 'nt':
        raise ValueError('Native Windows test required')
    import winreg
    setup = args.setup.resolve(strict=True)
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER,KEY,0,winreg.KEY_READ | winreg.KEY_WOW64_64KEY):
            raise ValueError('Existing Kosha desktop installation; test refuses to replace it')
    except FileNotFoundError:
        pass
    def path_value():
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER,'Environment') as key:
                return winreg.QueryValueEx(key,'Path')
        except FileNotFoundError:
            return None
    before = path_value()
    with tempfile.TemporaryDirectory(prefix='kosha-setup-smoke-') as temp:
        root = Path(temp).resolve()
        install = root/'app'
        # /D must be last and its remainder is not quoted in NSIS syntax.
        command = subprocess.list2cmdline([str(setup),'/S','/AddToPath=1']) + ' /D=' + str(install)
        subprocess.run(command, check=True, timeout=120)
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER,KEY) as key:
            actual = Path(winreg.QueryValueEx(key,'InstallLocation')[0])
        try:
            assert actual == install, 'Setup did not honor /D'
            assert str(install).casefold() in path_value()[0].casefold().split(';')
            env = dict(os.environ, APPDATA=str(root/'roaming'),LOCALAPPDATA=str(root/'local'))
            stamp = subprocess.check_output([str(install/'kosha.exe'),'--version'],text=True,env=env).strip()
            assert subprocess.check_output([str(install/'kosha-gui.exe'),'--version'],text=True,env=env).strip() == stamp
            fixture = root/'scan'; fixture.mkdir(); (fixture/'hello.txt').write_text('fixture')
            subprocess.run([str(install/'kosha.exe'),str(fixture),'--stats'],check=True,env=env,stdout=subprocess.DEVNULL)
            (install/'keep-unknown.txt').write_text('must survive uninstall')
            if args.upgrade_setup:
                before_upgrade = path_value()
                upgrade = subprocess.list2cmdline([str(args.upgrade_setup.resolve()),'/S','/AddToPath=1']) + ' /D=' + str(install)
                subprocess.run(upgrade,check=True,timeout=120)
                assert path_value() == before_upgrade, 'Upgrade changed PATH unexpectedly'
                assert (install/'keep-unknown.txt').is_file(), 'Upgrade removed unknown data'
                rejected = subprocess.run(command,timeout=120)
                assert rejected.returncode != 0, 'Downgrade was not rejected'
        finally:
            subprocess.run([str(actual/'Uninstall.exe'),'/S'],check=True,timeout=120)
            for _ in range(100):
                if not (install/'kosha.exe').exists():
                    break
                time.sleep(.1)
        assert not (install/'kosha.exe').exists(), 'Uninstall left known binary'
        assert (install/'keep-unknown.txt').is_file(), 'Uninstall deleted an unknown file'
        # An originally absent PATH may become an empty string; both resolve identically.
        after = path_value()
        assert after == before or (before is None and after is not None and after[0] == ''), 'User PATH was not restored'
        print(json.dumps({'passed': True,'stamp':stamp,'scope':'current user','path_restored':True,'unknown_file_preserved':True}))


if __name__ == '__main__':
    main()
