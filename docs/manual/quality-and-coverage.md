# Tests and coverage

Kosha is checked on Windows, Linux and macOS. The
[CI runs](https://github.com/coolnew/kosha/actions/workflows/ci.yml) show the
exact commit, selected jobs and their outcomes. A passing documentation-only
run means its selected checks passed; it does not mean native tests ran again.
Release candidates always undergo the separate full release validation.

## Which checks run

Every change runs repository hygiene, Python automation tests, generated-document
checks, and offline Markdown file/image link validation. Rust, build inputs,
workflow changes, marketplace data and unfamiliar paths run the native matrix
and musl check. Ordinary prose and screenshots skip those builds. Manuals that
are executable test inputs run focused reference tests even without code changes.
Manual and weekly CI runs exercise the full matrix.

## Find a coverage report

Open the latest successful relevant
[coverage run](https://github.com/coolnew/kosha/actions/workflows/coverage.yml).
Its job summary lists measured lines, functions and regions and the source
commit. Download the `coverage-linux-COMMIT` artifact and open `site/index.html`
for an offline report and annotated source. It also contains LCOV and JSON totals.
Artifacts are retained for 30 days; GitHub requires signing in to download them.
Access to runs and artifacts follows the repository's visibility and permissions.

The [Kosha website](https://coolnew.github.io/kosha/) hosts this manual and a
separate [coverage report](https://coolnew.github.io/kosha/coverage/). The report
links to annotated source, measured totals and their source commit. The
[Linux coverage badge](https://coolnew.github.io/kosha/coverage.svg) is also
available for repository links. No external coverage account is required.

Website updates reuse the latest retained successful main-branch measurement;
editing ordinary documentation does not rerun Rust coverage. The manual follows
current main, while coverage can describe an earlier commit: always check its
commit and date. If no retained report is available, the website says so rather
than presenting an invented percentage. Actions artifacts remain available
independently of Pages publication.

## What the numbers mean

The initial report covers **Rust workspace tests on Linux**, using
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov). Tests run once with
instrumentation; the report formats reuse that measurement. The baseline is
informational, without an arbitrary percentage threshold.

Check the report's commit and date: docs-only pushes do not rerun coverage and
failed measurements do not publish a new badge. A current report is evidence
that lines executed, not that every behavior was asserted correctly.

Linux cannot exercise Windows-only code, interactive GUI behavior or macOS
APIs. This report also excludes Python automation, branch coverage and doctest
coverage. Regular CI still runs doctests. The tool's default exclusions remove
standalone test files and dependencies; inline test modules may contribute to
the totals. Production code is not excluded just to improve a score.

## Reproduce the report

Install `llvm-tools-preview` for the active Rust toolchain and the pinned
`cargo-llvm-cov` version from the
[coverage workflow](../../.github/workflows/coverage.yml). Follow the platform's
GUI build prerequisites before measuring the full workspace.

    cargo llvm-cov --locked --workspace --no-report
    cargo llvm-cov report --html --output-dir target/coverage/site/coverage
    cargo llvm-cov report --lcov --output-path target/coverage/lcov.info
    cargo llvm-cov report --json --summary-only --output-path target/coverage/llvm-summary.json

`xtask-coverage.py` adds the landing page, provenance and badge. Pass the measured
full Git commit and actual platform, for example:

    python xtask-coverage.py --input target/coverage/llvm-summary.json --output target/coverage/site --commit FULL_40_CHARACTER_SHA --platform linux --scope workspace

Use a separate Cargo target for coverage so instrumentation does not disrupt
ordinary builds. Reports belong under ignored `target/`, not in source commits.
