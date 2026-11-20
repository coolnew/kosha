# Changelog

## [Unreleased]

- Require full trusted CI for the exact tagged commit before release builds.
  Reuse compatible native Rust validation reports while retaining separate
  optimized compilation and all native package checks. Local/manual builds
  remain fully validated; missing or invalid CI approval blocks tagged releases.

## [0.10.7] — 2026-11-20

### Security

- Update rustls to 0.23.46, addressing the four reported TLS advisories
  GHSA-2mjx-qc3c-rqvc, GHSA-h2v8-394m-p7vv, GHSA-c8m5-c952-6cm4 and
  GHSA-w44r-93j7-qv28. Upgrade from earlier builds for the corrected dependency.

### Windows downloads

- Portable ZIP, offline Setup EXE and Store-upload MSIX now use the same
  verified static-CRT binaries: no separate Visual C++ Redistributable needed.
- New offline Setup offers per-user/all-users installation, optional CLI PATH
  integration, upgrades and uninstall that preserves user data. Windows
  downloads remain unsigned; the MSIX is for Partner Center upload, not sideloading.

### macOS

- Add the native application menu and working Command+Q, including with search
  or Settings focused. Native Apple Silicon tests verified both quit paths,
  scanning/search, dark/light themes and saved window geometry (Refs #2, #3).
- Seal the complete app bundle and companion CLI with ad-hoc signatures,
  verified before packaging and after DMG extraction. The downloaded rc.2 DMG
  also passed checksum/image/strict bundle checks on macOS 14.4.1.
- macOS remains experimental. Ad-hoc signatures do not provide Developer ID
  trust or Apple notarization; Gatekeeper can block a download. See the
  [macOS installation guide](https://coolnew.github.io/kosha/manual/macos/)
  for supported first-launch approval and warning guidance. Issue #1 remains
  open for download-trust acceptance; no automatic security bypass is installed.

### Website and support

- Redesigned homepage with GUI/CLI showcases, easier issue-report links and
  guided bug/feature forms, a public privacy policy and clearer download guides.
- Clarify the separate `coolnew/kosha-marketplace` catalog and recipe source.

This regular release follows the 0.10.7 release candidates. Existing releases
and preview assets are unchanged. Issues #4–#9 are outside this release's scope.

- Update rustls to 0.23.46 for TLS security fixes covering handshake record
  boundaries, certificate decompression, session Debug redaction and key-exchange
  prediction state (GHSA-2mjx-qc3c-rqvc, GHSA-h2v8-394m-p7vv,
  GHSA-c8m5-c952-6cm4, GHSA-w44r-93j7-qv28).

- Make website issue reporting easy to find, with navigation/footer links and
  guided GitHub forms for bugs and feature requests.

- Build Windows portable ZIP, Setup and Store MSIX from verified static-CRT
  binaries, removing the separate Visual C++ Redistributable prerequisite.
- Add an offline NSIS Setup EXE with per-user/all-users installation, optional
  CLI PATH integration, upgrades and uninstall that preserves user data.
- Document release download formats and Windows installation behavior.

## [0.10.7-rc.2] — 2026-11-20

### Release candidate publication correction

This candidate includes the macOS changes from rc.1 and fixes the final preview
publication step. All three rc.1 host builds passed, but its draft was not
published because the verifier used a tag lookup for an unpublished draft.
Draft verification and publication now use the upload action's release ID,
while retaining exact tag, channel, inventory and server SHA-256 checks.

For native validation, test the fresh Apple Silicon DMG download, application
launch (Refs #1), Command+Q (Refs #2), and Kosha → Quit Kosha (Refs #3).
The application is ad-hoc signed for integrity, not Developer ID signed or
notarized; Gatekeeper may still block downloaded apps. Please report your
macOS version and exact result. These issues remain open pending validation.
Issue #4 is not included. See the rc.1 notes for the complete test focus.

This is a testing pre-release, does not replace Latest, and omits Store MSIX.

## [0.10.7-rc.1] — 2026-11-20

# macOS validation candidate

This is the first release candidate for 0.10.7. Please test the fresh Apple
Silicon DMG download and report results on the linked issues. It is a testing
preview and does not replace the latest regular release.

### What to validate

- Refs #1: the complete macOS app bundle and companion CLI now receive ad-hoc
  signatures, checked before packaging and after DMG extraction. Test opening
  the downloaded DMG and copied application. These signatures verify integrity;
  they are not Developer ID signing or Apple notarization. Gatekeeper may still
  block a fresh download. Please report the exact message and macOS version;
  this issue remains open until native download testing establishes the result.
- Refs #2: test Command+Q from the main window, including after a scan.
- Refs #3: test the native Kosha menu and Quit Kosha item. Both quit paths should
  save window geometry before exiting.

Native interactive macOS acceptance remains pending. Hosted packaging and
signature checks do not prove that a browser-downloaded app passes Gatekeeper
or that interactive shortcuts work. Issue #4 (context-menu deletion) is not
included in this candidate.

Windows ZIP, Linux archive, macOS DMG, sources and SHA-256 sidecars are included
after successful hosted validation. Preview builds omit Microsoft Store MSIX.
Please attach the version, operating system and steps/results to the relevant
issue, avoiding personal paths or file names.

- Added a public privacy policy covering local filesystem data, saved records,
  optional marketplace requests, website services and support. It is available
  from the website footer and included with offline release documentation.

- Added numbered alpha/beta/rc pre-releases that publish after verified host
  builds and uploaded SHA-256 checks, without replacing Latest. Regular releases
  remain drafts; previews retain the Windows ZIP and omit Store-upload MSIX.

- Added a native macOS application menu and Command+Q quit binding. Native
  interaction validation is pending (references issues #2 and #3).
- Seal the complete macOS app bundle and CLI with ad-hoc signatures, verify
  them before DMG creation and again after extraction. This addresses bundle
  integrity, not Developer ID trust or notarization (references issue #1).

- Redesigned the website homepage with a custom product layout, GUI and CLI
  showcases, clearer navigation, and copyright/license links.
- Clarified marketplace ownership: production catalogs and packs are maintained
  in `coolnew/kosha-marketplace`; Kosha keeps reusable validation and offline tests.

## [0.10.6] — 2026-11-19

### Windows Store package acceptance

Correct the MSIX application layout that triggered Partner Center's headless-app
waiver requirement. The package has one visible KoshaApp desktop entry and a
console execution alias for the companion CLI.

Microsoft approval of runFullTrust, allowElevation and unvirtualizedResources
remains a separate certification requirement. Submit for Windows 10/11 Desktop
only. The unsigned Store-upload MSIX is not a consumer sideload installer.

- Fix Store MSIX acceptance by declaring one visible desktop app and routing
  the terminal alias directly to the companion CLI, without a hidden app entry.
- Check desktop-only targeting and execution alias routing before release;
  document Partner Center device-family selection and capability approval.

## [0.10.5] — 2026-11-19

### Distribution updates

- Add an unsigned Windows MSIX upload package for Microsoft Partner Center,
  alongside the portable Windows ZIP. The package uses the registered KoshaApp
  identity and includes the author, support email and project website.
- Verify MSIX checksums, source commit, manifest identity/capabilities and exact
  agreement with the executables in the Windows ZIP before drafting a release.
- Package macOS downloads as a compressed DMG with an Applications shortcut,
  companion CLI and offline manuals.

The MSIX asset is for Store submission, not direct consumer installation.
Store certification and approval of the elevation/unvirtualized capabilities
remain pending. Windows/macOS downloads remain unsigned; macOS support is
experimental. No Microsoft Store submission or publication is automated.

## [0.10.4] — 2026-11-19

Kosha 0.10.4 is the first public release, incorporating the reviewed CLI feedback.
It includes the desktop explorer, CLI, manual website and verified multi-platform
release packaging.

### CLI and release highlights

- Ship the updated user manual and concise agent guide with the new CLI behavior.

- Classify draft release channels by the actual SemVer prerelease suffix;
  document optional public preview publication and reviewed promotion instead
  of treating every `0.x` release as a pre-release.

- List implicit drive snapshots with `kosha --caches`, including age, source
  root, record paths, partial/incomplete indicators and JSON/JSONL inventory.
- Open exact saved snapshot filenames in browsing, comparison and search;
  existing local paths take precedence and selected-file diagnostics identify
  the historical observation. Expand help examples for comparisons and duplicates.

Windows/macOS binaries are unsigned, and macOS remains experimental. Read the
installation guide and release notes for platform prerequisites and limitations.

## [0.10.3] — 2026-11-19

- Fix multi-platform release assembly by producing shared source archives once on Linux and keeping each host manifest limited to its published artifacts.
- Verify the complete release inventory, SHA-256 digests, archive sizes, versions, host targets and source commit before creating a draft release.
- Set consistent source ZIP producer metadata while retaining the existing Windows and macOS release-path fixes.

## [0.10.2] — 2026-11-19

- Fix release validation for Windows short-path aliases and macOS canonical temporary-directory paths, while retaining repository and build-output confinement checks.
- Add a searchable manual website with a dedicated measured test-coverage section, preserving coverage provenance and badge links.

## [0.10.1] — 2026-11-19

Kosha 0.10.1 is the corrected preview candidate following the initial 0.10.0 source tag.
It includes the portability fixes and CI improvements reviewed after that tag.

### Release notes

- Detect home directories using the same canonical paths as recorded scans, so
  cache findings work when an OS supplies an alternate path spelling.
- Keep the Windows closed-pipe handling while allowing strict non-Windows linting.
- Validate native path, filesystem-allocation and export cases without assuming
  Windows short paths, macOS /var aliases or Linux directory blocks are identical.
- Include the Linux native build prerequisites and the current CI/coverage tools.

Windows/macOS binaries are unsigned; macOS remains experimental. Verify the
SHA-256 sidecars for downloaded archives.

- Conservative CI job selection, a stable final check, cumulative PR detection,
  weekly full validation and removal of duplicate Rust test runs. Linux Rust
  coverage produces offline HTML, LCOV, provenance and an informational badge;
  public Pages deployment is opt-in.

- Update workflow actions to Node.js 24 runtimes. Resolve Windows safety-test
  fixture entries by their exact scanned names, independent of short temp paths.

- Use the recorded canonical root in absolute-path prefix query tests, including
  macOS temporary-directory aliases and Windows short paths.

- Convert macOS du block counts to bytes for allocated-size comparisons, use
  canonical image paths in scan tests, and test zero-byte details separately
  from empty directories that occupy storage.

- Add Linux GUI font development prerequisites to CI/release builds and the
  manual. Make export fixtures independent of temp-path aliases and filesystem
  restrictions on invalid UTF-8 filenames.

- Normalize detected home paths for cache findings and canonicalize test
  expectations across path aliases. Measure deletion-test allocation rather
  than assuming a filesystem's block size. Remove the CLI's unused anyhow dependency.

- Fix non-Windows strict Clippy validation of closed-pipe handling. Install and
  explicitly select the musl C compiler required by native TLS dependencies in CI.

## [0.10.0] — 2026-11-19

Kosha 0.10.0 is a preview release for people and coding agents investigating disk usage.
It combines a native visual explorer, deterministic natural-language/DSL search,
ranked findings, historical scans and snapshots, and structured CLI exports.

### Release status and limitations

- Windows and Linux are the primary platforms; macOS remains experimental.
- Windows/macOS binaries are unsigned. Verify the supplied SHA-256 checksums.
- Results describe a recorded scan, not a continuously updated filesystem index.
  Permission-limited scans and interrupted scans report different coverage states.
- Recipes are declarative; installed packs do not execute scripts or ML models.
- This preview is awaiting hosted builds and final platform acceptance before publication.

- Recipe packages use the indexed marketplace and versioned folders throughout;
  retired flat examples are no longer generated or maintained.

- Faster exact recipes and bounded Unicode typo suggestions, aggregate recipe
  budgets, stricter validation and recovery removal for unsupported packs.

- Scalable marketplace indexes and publisher namespaces; unsupported kinds and
  capabilities remain discoverable with explicit unavailable reasons.
- Extended recipe aliases, typed templates, regex captures and typo suggestions.
  This pre-release format refresh preserves current schema and package versions.

- Security reporting policy, public-source release gates, full hosted candidate
  validation, strict build stamps, and verified SHA-256 download checks.
- Source archive build stamps no longer inherit an unrelated enclosing Git
  repository; normal checkouts and linked worktrees keep their actual stamp.

- Illustrated GUI manual and dark-theme screenshots showing map navigation,
  quick look, natural-language search, and exports.
- Protected release-candidate scripts for source/binary archives, validations,
  version/changelog preparation, and checksums.
- Windows GUI console detachment so closing the launching terminal does not
  close a normal app window; terminal help/version output remains available.

- Natural-language and DSL search, including date, size, type, path and folder
  conditions, custom phrase recipes, and inspectable interpretations.
- Search exports as full-path TXT, CSV and JSONL, including every match beyond
  the display limit; CLI JSON exports include coverage metadata.
- Automatic drive records, explicit snapshots, comparisons, and original scan
  age indicators. Interrupted scans preserve the previous completed drive record.
- Shared creation timestamps in snapshot format 2, with older records readable.
- Searchable settings, scrollbars, graph visibility controls, and Windows
  maximize/restore fixes.
- CLI fresh scans by default, explicit shared/custom cache reads, improved help,
  `search --describe --json`, and clean machine-readable output.

## 0.9.0 source baseline

The original development baseline identified itself as 0.9.0; it was not a
published GitHub release. A source version is not evidence that a GitHub release has been published or that launch checks passed
on every platform. Release tags must match the workspace and lockfile versions;
see [Releasing](RELEASING.md). macOS remains experimental.

[0.10.0]: https://github.com/coolnew/kosha/releases/tag/v0.10.0

[Unreleased]: https://github.com/coolnew/kosha/compare/v0.10.7...HEAD

[0.10.1]: https://github.com/coolnew/kosha/releases/tag/v0.10.1

[0.10.2]: https://github.com/coolnew/kosha/releases/tag/v0.10.2

[0.10.3]: https://github.com/coolnew/kosha/releases/tag/v0.10.3

[0.10.4]: https://github.com/coolnew/kosha/releases/tag/v0.10.4

[0.10.5]: https://github.com/coolnew/kosha/releases/tag/v0.10.5

[0.10.6]: https://github.com/coolnew/kosha/releases/tag/v0.10.6

[0.10.7-rc.1]: https://github.com/coolnew/kosha/releases/tag/v0.10.7-rc.1

[0.10.7-rc.2]: https://github.com/coolnew/kosha/releases/tag/v0.10.7-rc.2

[0.10.7]: https://github.com/coolnew/kosha/releases/tag/v0.10.7
