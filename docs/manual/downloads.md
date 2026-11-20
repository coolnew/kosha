# Choose a download

Download Kosha from the [official releases](https://github.com/coolnew/kosha/releases).
Choose a regular release for ordinary use, or a numbered pre-release to test
candidate changes. Read the selected release's notes: older versions do not
gain new packaging features automatically. This page describes the current
source's packaging, including changes awaiting the next release.

| Download | Contents and use |
| --- | --- |
| Windows `*-x64-setup.exe` | Offline NSIS installer for the GUI, CLI and manuals. Offers per-user or all-users installation, a Start menu shortcut, optional CLI PATH integration, upgrades and uninstall. |
| Windows `*-x86_64-pc-windows-msvc.zip` | Portable GUI, CLI and manuals. Extract before running; no installation, automatic PATH change, shortcut or uninstaller. |
| Windows `*-x64-store-upload.msix` | Unsigned package for submission to Microsoft Partner Center. It is **not** a consumer sideload installer. A Store-installed copy gets its signature through Microsoft. |
| macOS `*-aarch64-apple-darwin.dmg` | Apple Silicon app bundle, companion CLI, manuals and Applications shortcut. See [macOS installation](macos.md) for signing limitations and first-launch steps. |
| Linux `*-x86_64-unknown-linux-gnu.tar.gz` | GUI, CLI, desktop integration files and manuals. See [Linux prerequisites](linux.md). |
| `*-source.zip` / `*-source.tar.gz` | Reviewed public source and build instructions, without compiled Kosha executables. |
| `*.sha256` and `release-manifest-*.json` | Checksums and build provenance. These are verification files, not installers. |

New Windows packages use the same **static Visual C++ runtime** CLI and GUI
binaries across ZIP, Setup and MSIX. They do not require a separately installed
`VCRUNTIME140.dll` or Visual C++ Redistributable. Windows system libraries and
supported graphics drivers are still required. Earlier releases, including
0.10.6 and 0.10.7-rc.2, were built differently; this promise does not apply to them.
The packaging gate checks ordinary and delay-load PE imports before distribution.

## Windows Setup

Setup defaults to **just you**, under `%LOCALAPPDATA%\Programs\Kosha`.
Choose **Install for all users** for installation under Program Files; Windows
asks for administrator approval. The GUI runs normally afterward. Installing
for all users does not automatically elevate scans or enable MFT access.

The PATH checkbox adds the installation directory to your user PATH or the
machine PATH for the chosen scope. Open a **new terminal** afterward and run
`kosha --version`. Existing terminal processes keep their old environment.
PATH updates use native registry buffers to preserve long existing values;
unsupported values or access failures are reported instead of truncating PATH.

Close Kosha before upgrading. Run the newer Setup in the same installation
scope. Downgrades are refused. Uninstall from Windows Settings → Apps; snapshots,
saved scans and settings remain in their [per-user storage](settings-and-files.md).
The uninstaller removes only its own PATH entry if Setup added it, and deletes
known application files without recursively deleting unknown files.
Unchecking PATH on a later upgrade leaves an existing installer-owned entry in
place until uninstall. Avoid installing multiple channels/scopes together:
Windows PATH and Store aliases can select a different `kosha` than intended.
Use `where.exe kosha` to inspect command resolution.

For unattended installation, `/S` is case-sensitive:

```powershell
.\kosha-vVERSION-x64-setup.exe /S /AddToPath=1
.\kosha-vVERSION-x64-setup.exe /ALLUSERS /AddToPath=1
```

All-users automation should launch Setup from an elevated deployment process.
Otherwise the installer requests UAC consent and relaunches interactively;
that consent flow is not a substitute for managed deployment exit-code reporting.
No runtime downloader or background updater is installed. Setup EXEs and ZIP
binaries remain unsigned until separately configured code signing is available;
follow your software policy when Windows displays a publisher warning.

## Portable and Store command-line access

For ZIP use, run `.\kosha.exe` from its extracted directory or add that directory
to your user PATH yourself. Store MSIX uses the Windows **app execution alias**
`kosha.exe`, managed in Settings → Apps → App execution aliases, rather than
putting its versioned WindowsApps package directory on PATH.

The installer is an EXE today. A standalone MSI is not currently distributed;
enterprise MSI packaging would be a separate future addition.
