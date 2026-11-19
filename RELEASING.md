# Releasing kosha

Versions are declared in workspace `Cargo.toml`, inherited by all crates, and
repeated on local path dependencies and in `Cargo.lock`. `python xtask-version.py`
checks that they agree. Binaries report version, commit and build target.

## Prepare a candidate

1. Update the workspace and local dependency versions together, refresh the
   lockfile, and document user-visible changes with the actual release date.
2. Pass workspace tests, strict clippy, formatting, version and repository
   checks on supported systems. Review partial/incomplete scans and cleanup
   behavior. macOS support remains experimental until interactively validated.
3. Commit the public working tree with sign-off. Build release binaries from
   that clean commit and run `python xtask-package.py --execute`. Without
   `--execute`, packaging is a read-only dry-run. It verifies matching
   binary version stamps, packages documentation, extracts the archive and
   exercises the CLI. Review GUI behavior separately on a clean machine.
4. Rehearse the GitHub release workflow with `workflow_dispatch`. This builds
   archives/checksums as workflow artifacts but does not create a release.

The hosted release workflow accepts reviewed `coolnew/kosha` source only and
uses `xtask-release.py --execute` for each host. Workspace tests/clippy and
repository/Python gates precede packaging; a build alone cannot create a draft.
It retains source ZIP/TAR.GZ, host binary archives, SHA-256 sidecars and per-host
manifests. Manual rehearsals need committed, dated candidate release notes too.
Linux is the sole producer of the shared source ZIP/TAR.GZ. Windows and macOS
use `--binary-only`, so their manifests list their own host archive only.
Source archive compression and ZIP metadata can differ across host/toolchain
environments; do not compare one producer's compressed archive against another
producer's manifest. The draft step verifies the exact three host manifests,
all five published archives, sizes, SHA-256 sidecars and source commit before
uploading anything. No digest mismatch is ignored.
Check downloaded SHA-256 sidecars with `sha256sum -c FILE.sha256` on Linux or
compare `Get-FileHash FILE -Algorithm SHA256` on Windows. Hashes verify the bytes;
they do not provide code signing or replace platform acceptance.

## Tag the public candidate

For an orchestrated candidate build, use `python xtask-release.py` to inspect
the plan and `python xtask-release.py --execute` to run host validations,
build release binaries, and retain source ZIP/TAR.GZ plus host binary archives,
checksums, and a release manifest. It defaults to the checkout containing the
script; `--public` or `--repo-path PATH` selects a different checkout. It never
commits, tags, pushes, uploads, or changes repository history.

To prepare a new version and changelog from reviewed notes, inspect
`python xtask-release.py --prepare --version X.Y.Z --notes-file NOTES.md`, then
add `--execute` to make only those worktree edits. Review and commit them before
building the candidate. Existing artifacts and unrelated edits are preserved.
Source archives always contain selected public files, even when built from a
private development checkout; private-built binaries are development candidates.
For a standalone host-only package, add `--binary-only`; `--source-only` produces
sources without host binaries. The two options are mutually exclusive. The
default local plan continues to include both source and host archives.

After reviewing the candidate, replace `X.Y.Z` with the exact workspace version:

```sh
python xtask-version.py --tag vX.Y.Z
git tag -a vX.Y.Z -m "kosha X.Y.Z"
git push origin main
git push origin vX.Y.Z
```

Push tags only from the public repository. The release workflow checks that
the tag matches Cargo before building Windows x64, Linux x64, and macOS ARM64.
The three successful builds attach archives and SHA-256 checksums to a **draft**
GitHub release. Review the downloads and release notes before publishing.
Never move a published tag to a new commit; use a new version for fixes.

A `v0.*` tag or a version with a SemVer prerelease suffix is marked pre-release. Unsigned Windows/macOS binaries and the
experimental macOS status must be stated in release notes. Repository rules
and required status checks must be configured in GitHub before launch; a tag
trigger alone does not prove that CI or platform acceptance passed.
