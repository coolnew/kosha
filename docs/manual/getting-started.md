# Getting started

For a visual walkthrough of the desktop app, see the
[illustrated desktop tour](desktop-tour.md).

## Open kosha

Download the archive for your operating system from the project's
[release page](https://github.com/coolnew/kosha/releases). Extract it before
running the application. On Windows, open `kosha-gui.exe`; on Linux, run
`kosha-gui`. The companion terminal program is `kosha.exe` on Windows and `kosha`
on Linux. On macOS, download the `.dmg` for your architecture and open it.
Drag `Kosha.app` onto the **Applications** shortcut, then launch it from
Applications. The current hosted macOS build is for Apple Silicon (ARM64).
Older releases may supply a `.tar.gz` instead; extract that to find the app.

The macOS disk image also contains the companion `kosha` terminal program and
offline manuals. Copy the CLI to a directory on your `PATH` if you want to use
it from any terminal; dragging the app to Applications does not install the CLI.
Copy any manuals you want to keep, then eject the disk image. Check the download
in Terminal with `shasum -a 256 -c FILE.dmg.sha256` from the download directory.
The app and disk image are unsigned and not notarized, so macOS Gatekeeper may
block them. Follow your organization's policy; see Apple's
[guidance for opening apps safely](https://support.apple.com/en-us/102445).

Use the recommended regular release for ordinary use, or choose a release marked
**Pre-release** when you want to try a preview and provide feedback. Versions
with a suffix such as `-rc.1` or `-beta.2` are previews; a suffix-free version may
also be marked as a preview while gathering field feedback. `0.x` indicates
initial development and possible compatibility changes, but does not by itself
mean the GitHub release is a preview. Read the selected release's known issues
and platform notes, and include `kosha --version` when reporting a problem.

The binaries are not code-signed. Follow your organization's software policy
when deciding whether to run downloaded software. Release archives have a
companion `.sha256` checksum. See the distribution's installation notes for
desktop integration and operating-system prerequisites.
On Linux, see [install and build on Linux](linux.md) for archive verification,
desktop dependencies, and source builds.

## Your first scan

1. Choose a drive or a personal folder in the left sidebar. Use **Choose a
   folder** to select another folder.
2. Watch the entry count while kosha scans. Use **Stop scan** or **Escape** to stop.
3. Explore the large rectangles in the treemap and the rows below it. The
   right-hand findings panel explains possible ways to recover space.
4. Use **Ctrl+F** to search and **Ctrl+K** to open the command palette. On macOS,
   use **Command** instead of **Ctrl**.

A previously scanned drive can open its saved scan immediately. The **Saved
scan** label includes its age. Choose **Scan again** for a fresh disk view.
Folder scans start fresh and are not saved automatically.

kosha starts in **Read-only** mode. Scanning and browsing do not delete or move
your files. The app can still write its own settings and saved drive scans.
Read [Cleaning up safely](safety.md) before enabling file-changing actions.

## Find a command

The command palette lists actions applicable to the current view, including
rescanning, exporting, keeping a snapshot, ownership, and settings. Search for
an action's name rather than memorizing every control.

Next: [Scanning](scanning.md) or [reading results](results-and-search.md).
