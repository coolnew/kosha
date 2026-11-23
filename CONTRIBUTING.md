# Contributing

Changes require Developer Certificate of Origin sign-off: `git commit -s`.
See [DCO](DCO). Use LF text files, with CRLF only for Windows shell scripts.
New Rust source files need the existing SPDX license and copyright header.
The project notice is `Copyright (c) 2026 Neeraj Sharma
<neerajsharma.9@outlook.com> and The kosha contributors`.
Contributors retain copyright in their contributions; sign-off is not an
assignment. Preserve other applicable copyright and third-party notices.
The Linux Foundation notice in [DCO](DCO) concerns the certificate text itself;
keep that document verbatim, including its original attribution.

Run these checks before submitting a pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python xtask-version.py
python xtask-eol.py
python xtask-spdx.py --check
python xtask-gaps.py
python xtask-mutations.py
```

Platform changes need validation on affected systems. Analysis rules should use
synthetic test data and remain deterministic. Include before/after measurements
for scanner performance changes. User-file mutations require `kosha-fsops` and
its explicit write grant. Update the [manual](docs/manual/README.md) for changes
to user-facing behavior and add a concise changelog entry.

## Distributing your own macOS build

A local build for your own use can skip notarization. If you distribute a
build outside the Mac App Store, you can sign and notarize it using your
Apple Developer account, a **Developer ID Application** certificate, and a
`notarytool` keychain profile.

1. Build Kosha on macOS from the source revision you intend to distribute.
   The repository packager can assemble a DMG with an ad-hoc seal.
2. Sign the assembled `Kosha.app` and standalone `kosha` executable with
   Developer ID, hardened runtime, and a secure timestamp. Create the final
   DMG from the signed payload and sign that disk image.
3. Submit the final DMG with `notarytool`. After Apple accepts it, staple
   and validate the ticket, then verify the signed DMG and its contents.
   Generate the download checksum from the stapled DMG.

See [Apple's notarization guide](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution/) for credential setup and command examples.

Report bugs through [Issues](https://github.com/coolnew/kosha/issues), with
version, platform, and a reproducible example. Do not attach private scan data,
credentials, or unredacted paths.
