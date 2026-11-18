# Changelog

## [Unreleased]

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

[Unreleased]: https://github.com/coolnew/kosha/compare/v0.10.0...HEAD
