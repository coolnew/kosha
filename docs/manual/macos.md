# Install and run on macOS

Kosha's macOS download is an **Apple Silicon** DMG containing `Kosha.app`, the
companion `kosha` CLI and offline manuals. macOS support is experimental.

## Install from GitHub

1. Download the DMG and its `.sha256` file from the
   [official release](https://github.com/coolnew/kosha/releases).
2. Verify the checksum in Terminal, using the exact downloaded filename:

   ```sh
   cd ~/Downloads
   shasum -a 256 -c kosha-vVERSION-aarch64-apple-darwin.dmg.sha256
   ```

3. Open the DMG, drag **Kosha.app** to **Applications**, then eject the image.
4. Open Kosha from Applications.

## If macOS blocks the first launch

The app is **ad-hoc signed for integrity**, but has no Developer ID signature
or Apple notarization. A valid checksum or signature does not establish that
Apple trusts the publisher.

For an unidentified-developer/notarization warning, if you trust the downloaded
release: first try opening the app, then go to **System Settings → Privacy &
Security → Open Anyway** and confirm **Open**. Your organization may prohibit
this exception. Follow [Apple's guidance](https://support.apple.com/en-us/102445).

An alert saying **damaged**, **will damage your computer**, or a failed checksum
needs investigation. Stop and [report the exact message](https://github.com/coolnew/kosha/issues/new/choose)
with the version and macOS version. Do not disable Gatekeeper or recursively
remove quarantine attributes as an installation step.

## Use the CLI

The companion `kosha` executable is alongside the app in the image. Copy it to
a directory you control before ejecting the DMG. Run it with its full path or
add its directory to your shell's PATH; dragging the GUI to Applications does
not install the CLI on PATH. The CLI has the same download-trust limitations.

```sh
/path/to/kosha --version
/path/to/kosha ~/Downloads --stats
```

Snapshots and settings stay in your account's
`~/Library/Application Support/app.coolnew.kosha/`. See
[stored files and privacy](settings-and-files.md). GUI testing automation may
need Screen Recording and Accessibility permission for its controlling tool;
ordinary Kosha scanning does not require those permissions.
