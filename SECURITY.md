# Security policy

## Report a vulnerability privately

Email **neerajsharma.9@outlook.com** with a description, affected version,
operating system, and the smallest reproducible example. Include the output of
`kosha --version` or `kosha-gui --version`. Do not attach credentials, complete
personal scan indexes, or unredacted personal paths. A synthetic directory tree
is preferable when it reproduces the issue.

Please avoid posting exploit details in a public issue before the maintainer
has reviewed the report. Ordinary bugs and feature requests belong in
[GitHub issues](https://github.com/coolnew/kosha/issues), with private paths removed.
There is no guaranteed response or remediation time.

## Scope and supported builds

Security fixes target the latest maintained release. Pre-release builds can
change behavior; experimental platform support is identified in release notes.
Report older affected versions too, so the maintainer can assess their impact.
Use the version and commit stamp to distinguish official artifacts from local
builds. SHA-256 sidecars check archive integrity; they are not digital signatures.
Windows and macOS release binaries are currently unsigned.

Kosha begins in read-only mode. File-changing actions require an explicit mode
switch and review; those safeguards do not make arbitrary cleanup risk-free.
Scan records and exported reports contain filesystem metadata, including paths.
Treat them as private data. See [safe cleanup](docs/manual/safety.md) and
[stored files](docs/manual/settings-and-files.md).
