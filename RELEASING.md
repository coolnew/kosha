# Releasing kosha

## CI approval and production builds

The workflow supplies its own `GITHUB_TOKEN` with Actions read permission; no
personal token or private `.env` is needed. Job tokens are treated as opaque
header-safe credentials rather than constrained to a personal-token format.

Tagged releases require successful **full CI for the exact tagged commit** on
trusted main, including Linux, Windows, macOS, musl and the final gate. CI keeps
native validation reports for 14 days. Docs-only runs, PR merge commits, old
SHAs and mixed-attempt partial reruns do not qualify. Manually run full CI on
main when native jobs were skipped; wait for all reports before advancing main.

The tag gate verifies GitHub run/job state and report provenance before starting
release builds. Compatible compiler/runner images reuse CI formatting, Clippy
and Rust tests. Changed/unknown environments run those checks locally. Invalid
approval blocks release. Optimized compilation retains production flags,
including Windows static CRT. Native Python/repository checks, package smoke,
installer, MSIX/DMG and hash/provenance checks remain mandatory. Host manifests
record the CI run and reuse decision; debug CI executables are not shipped.

Local builds and manual release rehearsals keep complete validation. The
`--ci-evidence` option is reserved for the public tag workflow and rechecks live
approval; it is not a local skip-tests switch. For a timing/expired-report
failure, rerun all CI jobs for the candidate and rerun the tag workflow. Do not
move the tag. Source corrections require a new version.

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

Keep [third-party source notices](THIRD-PARTY-NOTICES.md) aligned with the
locked dependency versions. The packagers include the notice in offline
documentation and inside the macOS app bundle.

The hosted release workflow accepts reviewed `coolnew/kosha` source only and
uses `xtask-release.py --execute` for each host. Verified CI evidence covers
compatible Rust validation; repository/Python gates and production build/package
checks precede packaging. A build alone cannot create a draft.
It retains source ZIP/TAR.GZ, host binary archives, SHA-256 sidecars and per-host
manifests. Manual rehearsals need committed, dated candidate release notes too.
Linux is the sole producer of the shared source ZIP/TAR.GZ. Windows and macOS
use `--binary-only`, so their manifests list their own host archive only.
Host packages are Windows ZIP, Linux TAR.GZ and macOS DMG. On macOS, the native
`hdiutil` tool creates a compressed, read-only HFS+ image containing `Kosha.app`,
an Applications shortcut, the companion CLI and offline manuals. No Finder
automation or additional packaging dependency is required. Packaging verifies
the image, mounts it read-only, copies its payload to a disposable directory,
detaches it, then runs the same binary-stamp and CLI scan checks as other hosts.
The DMG has a SHA-256 sidecar and appears in its host manifest and final asset
verification. After assembling Info.plist and resources, the app bundle and
loose CLI receive ad-hoc signatures. Strict `codesign` verification must pass
before creating the DMG and again on the extracted payload before smoke tests.
These signatures seal the bundle but provide no Developer ID trust. The image
is unsigned and neither app nor image is notarized; this does not satisfy
Gatekeeper or interactive macOS acceptance by itself.
Source archive compression and ZIP metadata can differ across host/toolchain
environments; do not compare one producer's compressed archive against another
producer's manifest. The draft step verifies the exact three host manifests,
all five published archives, sizes, SHA-256 sidecars and source commit before
uploading anything. No digest mismatch is ignored.
Check downloaded SHA-256 sidecars with `sha256sum -c FILE.sha256` on Linux or
compare `Get-FileHash FILE -Algorithm SHA256` on Windows. Hashes verify the bytes;
they do not provide code signing or replace platform acceptance.

## Tag the public candidate

### Partner Center upload MSIX in GitHub releases

The release workflow can attach `kosha-vVERSION-x64-store-upload.msix`, its
SHA-256 sidecar and `msix-store-upload.json` alongside the Windows ZIP. This is
an **unsigned Store submission package**, not a consumer sideload installer.
Download the MSIX and sidecar from the reviewed release, verify the digest,
then upload the MSIX to Partner Center. Microsoft signs approved Store packages.
Restricted-capability approval and listing/certification are separate steps.

Enable the repository Actions variable `KOSHA_MSIX_STORE_UPLOAD=true` and set
`KOSHA_MSIX_IDENTITY_NAME`, `KOSHA_MSIX_PUBLISHER`,
`KOSHA_MSIX_PUBLISHER_DISPLAY_NAME`, `KOSHA_MSIX_AUTHOR_NAME`,
`KOSHA_MSIX_SUPPORT_EMAIL` and `KOSHA_MSIX_WEBSITE` to the reviewed non-secret
identity/contact values. No production PFX or Microsoft API key is needed to
build an upload package. Missing fields fail before the Windows build. With
the switch absent/false, release packaging continues without an MSIX. The
current numeric package-version policy supports suffix-free releases. The workflow
automatically omits MSIX for preview tags, even when the switch is enabled; the
portable Windows ZIP is still built. No Store flight/version mapping is implied.

The Windows pipeline invokes `xtask-release.py --msix-store` after the same
host validations and builds used for the ZIP. It uses the exact registered
identity, `KoshaApp` display name, terminal CLI alias and the experimentally
tested unvirtualized/elevation capabilities. `CONTACT.txt` holds the reviewed
author, publisher, support email and website. Publisher identity remains the
Partner Center-assigned value; personal authorship does not replace it.

Final assembly verifies MSIX identity/runtime/capabilities, source commit,
SHA-256 and size, and byte-for-byte agreement of both executables with the
Windows ZIP. Development identities, diagnostic probes and test signatures
are rejected. The selected mode must match the exact final release inventory.
Regular releases remain drafts for review; numbered previews publish after the
checks below. Nothing is submitted to Microsoft automatically. Existing release
tags/workflows are unchanged; these rules apply to newly prepared tagged source.

Store packages advertise one visible **KoshaApp** desktop entry. The companion
`kosha.exe` console alias is an extension of that application with an explicit
CLI executable and console subsystem. A separate hidden CLI application
(`AppListEntry="none"`) triggers Partner Center's headless-app waiver gate and
must not be included. Windows permits only one appExecutionAlias extension per
application, so the GUI is launched from Start; there is no additional GUI alias.
Release verification enforces the visible app, desktop-only targeting and CLI
alias routing. Local SDK validation does not replace Partner Center acceptance.

On Partner Center's Packages page, select **Windows 10/11 Desktop** only;
uncheck other device families if selected. This x64 package targets
`Windows.Desktop`, not Xbox, Mobile, Team or Holographic. An acceptance failure
can also produce a generic device-family message before availability is resolved.
The restricted-capability warning requires certification explanations/approval
for runFullTrust, allowElevation and unvirtualizedResources. Keep these declared
for the tested full-featured runtime; removing them changes execution/MFT
behavior. Store approval remains Microsoft's decision.

### Development MSIX rehearsal on Windows

`xtask-msix.py` creates an unsigned development package from existing x64
Windows release binaries. Supply the exact Partner Center identity fields:

```powershell
python xtask-msix.py --identity-name YOUR.IDENTITY.NAME --publisher 'CN=YOUR-ASSIGNED-PUBLISHER' --publisher-display-name 'Your publisher' --binary-dir target/release
```

The default is a read-only plan; add `--execute` to package with Windows SDK
MakeAppx. Use `--allow-elevation` only for restricted-capability/UAC testing.
The default `packagedClassicApp` runtime is virtualized. Local Windows testing
found that it redirects new AppData files and blocks the raw volume reads used
by the MFT scanner, even when elevated. `--unvirtualized` selects experimental
`win32App` behavior with `unvirtualizedResources`; this preserved ordinary
AppData storage and passed an elevated packaged MFT scan. This additional
restricted capability needs its own Store policy review; a sideload success
does not imply Store eligibility. `--probe-binary PATH` can explicitly include
a local diagnostic executable in a development package; never treat that
diagnostic package as a production upload.
Output stays in a fresh `dist/` child. The package name has `.Development`
appended and its console alias is `kosha-msix-dev.exe`, protecting the Store
identity and regular CLI alias. Both binaries must report the same workspace
version/commit/target. The evidence file retains their actual stamp; development
packaging does not certify that their commit matches current HEAD.

This command does not create certificates, install packages, submit to the
Store or add MSIX to GitHub releases. Installation requires a separately signed
copy and certificate trust on a controlled test machine. Keep self-signed test
packages out of consumer downloads. Production signing, Store approval and
installed-app data/elevation testing remain separate acceptance gates.

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
The three successful builds stage archives and SHA-256 checksums in a draft,
then verify the uploaded inventory and server SHA-256 digests. A numbered preview
is published automatically as a pre-release; a final version stays a draft for
review. Only tag pushes can publish; manual workflow dispatch retains artifacts.
Never move a published tag to a new commit; use a new version for fixes.

## Review, preview publication, and promotion

Draft status and the pre-release label are independent. The validated SemVer
version determines the channel, not major version zero:

| Version/tag | Default release label |
| --- | --- |
| `v0.10.7` or `v1.0.0` | Regular release, stays a draft for review |
| `v0.10.7-rc.1`, `v1.0.0-beta.2` | Published pre-release after verified uploads, never Latest |
| `v0.10.4+build-7` | Regular release; build metadata does not select the channel |

A regular `0.x` release remains initial-development software: it does not
promise the API compatibility associated with 1.0. The GitHub label describes
the intended audience for this particular build.

Use `X.Y.Z-alpha.N`, `X.Y.Z-beta.N`, or preferably `X.Y.Z-rc.N`, where N is a
positive integer up to 99,999 without leading zeroes. Multiple previews can precede the same
final version: `v0.10.7-rc.1`, `v0.10.7-rc.2`, then a separately built `v0.10.7`.
Unnumbered/custom preview suffixes and preview build metadata are refused.
Mac bundle version fields use ordered numeric values; the full preview SemVer
remains in `KoshaVersion`, informational metadata and `--version`. Finder can
show the target numeric version, so use `--version` for an exact bug-report ID.
Plan the next counter from fetched public tags using
`python xtask-release.py --next-preview 0.10.7` in the public checkout; it never
creates a tag or edits source. Prepare matching workspace/lockfile versions and
dated notes using `--prepare`. Never reuse or move a preview tag.

Pushing a reviewed numbered preview tag opts into public preview publication.
All host checks, local artifact verification, and uploaded names/sizes/SHA-256
checks must succeed first. A temporary draft hides incomplete uploads; failures
leave it unpublished for inspection. Already-published releases are never
overwritten by reruns. Previews omit Store MSIX automatically. They exist for
field testing and may still need native interaction/issue confirmation. Plain
`Refs #N` references keep issues open; do not use closing keywords prematurely.

Before publishing a regular draft or manually promoting a release:

1. Review the exact tag and commit, successful native jobs and final assembly
   verification. Confirm all archives, SHA-256 sidecars and host manifests are
   uploaded and match the reviewed release inventory.
2. Test downloaded CLI/GUI packages on the supported systems. Cover scanning,
   search, snapshots/comparison, exports, partial/incomplete results and essential
   window behavior. Record any platform-specific limitation rather than treating
   a successful compile as interactive validation.
3. Review release notes, installation instructions, known issues and user-manual
   accuracy. State Windows signing status, macOS ad-hoc versus Developer ID
   signing and notarization status, and experimental macOS support.
4. Obtain explicit maintainer approval for the selected release and channel.
   A regular-release commit/tag push is separate from approval to publish its
   draft. Numbered preview tags use the explicit automatic-preview policy above.

After review, choose either path:

- **Manual public preview:** publish the draft with **This is a pre-release** enabled.
  Users can download it for wider testing. Do not mark it as Latest. A version
  without a suffix may also be deliberately published this way when more field
  feedback is wanted; explain that decision in the release notes.
- **Regular release:** for a version without a prerelease suffix, publish with
  the pre-release label cleared. Set Latest when this is the recommended current
  release; a maintenance release for an older line need not replace Latest.

A publicly tested preview whose version has no suffix can later be promoted
by clearing its GitHub pre-release label and, when appropriate, setting Latest.
Recheck known issues and obtain approval for promotion. Preserve the exact tag,
commit, archives and digests; metadata promotion alone needs no rebuild.

A version containing `-rc`, `-beta` or another prerelease identifier retains that
label. Moving to the final `X.Y.Z` version requires new version metadata, a new
tag, build and full validation so the binaries report the final version. Never
rename a candidate's tag or relabel candidate binaries as final-version binaries.
Any source or asset correction also requires a new version; do not replace a
published build silently.

After publication or promotion, verify anonymous release/download access, the
pre-release/Latest state, version stamps and hashes, and website links. Keep
Unreleased open for development. Repository rules and required status checks
must remain configured; tag creation alone does not establish release acceptance.

See [GitHub release management](https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository)
and [Semantic Versioning](https://semver.org/).
