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

Report bugs through [Issues](https://github.com/coolnew/kosha/issues), with
version, platform, and a reproducible example. Do not attach private scan data,
credentials, or unredacted paths.
