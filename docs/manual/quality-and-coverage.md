# Tests and coverage

Kosha is checked on Windows, Linux and macOS. The
[CI runs](https://github.com/coolnew/kosha/actions/workflows/ci.yml) show the
exact commit, selected jobs and their outcomes. A passing documentation-only
run means its selected checks passed; it does not mean native tests ran again.
Tagged releases require a successful full native/musl CI run for the exact
source commit. Compatible release hosts can reuse its Rust validation results;
optimized binaries are built separately and native package checks always run.
If the compiler or runner image differs, Rust validation runs again. Local and
manual release rehearsals retain the full validation path.

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
commit. Download `coverage-native-linux-COMMIT`, `coverage-native-windows-COMMIT`, or
`coverage-native-macos-COMMIT` and open `site/PLATFORM/index.html` for an offline
report and annotated source. See `raw/PLATFORM/` for the LCOV, LLVM summary and
test log.
Artifacts are retained for 30 days; GitHub requires signing in to download them.
Access to runs and artifacts follows the repository's visibility and permissions.

The [Kosha website](https://coolnew.github.io/kosha/) hosts this manual and a
separate [coverage report](https://coolnew.github.io/kosha/coverage/). The report
links to annotated source, measured totals and their source commit for
Linux, Windows and macOS. All three measurements use the same commit.
Historical coverage artifacts may contain only Linux results. The
[Linux coverage badge](https://coolnew.github.io/kosha/coverage.svg) is also
available for repository links. No external coverage account is required.

Website updates reuse the latest retained successful main-branch measurement;
editing ordinary documentation does not rerun Rust coverage. The manual follows
current main, while coverage can describe an earlier commit: always check its
commit and date. If no retained report is available, the website says so rather
than presenting an invented percentage. Actions artifacts remain available
independently of Pages publication.

## What the numbers mean

The reports cover **native Rust workspace tests on Linux, Windows and macOS**, using
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov). Tests run once with
instrumentation; the report formats reuse that measurement. The baseline is
informational, without an arbitrary percentage threshold.

Check the report's commit and date: docs-only pushes do not rerun coverage and
failed measurements do not publish a new badge. A current report is evidence
that lines executed, not that every behavior was asserted correctly.

Each platform measures only the code compiled and exercised on its runner.
Windows includes the Fast NTFS worker feature. The reports exclude Python
automation, branch coverage and doctest coverage. Regular CI still runs
doctests. The tool's default exclusions remove standalone test files and
dependencies; inline test modules may contribute to the totals. Production code is not excluded just to improve a score.

## Scenario coverage

Choose **Fast NTFS drive scan** on the coverage page to see the expected drive
menu, consent, fallback and cancellation behaviors. Each scenario links to the
relevant source line and shows any mapped passing component test. Review rows
marked **Needs end-to-end evidence** before making a claim about the whole
workflow. An LLVM line hit can come from any test in the run. Windows UAC,
desktop interactions and snapshot preservation need separate acceptance tests.

To extend the report, add a reviewed workflow case to `xtask-coverage.py` with
a unique source line and, where applicable, the exact Rust test name. The
script resolves the current line from the checked-out source and compares it
with LLVM LCOV and named results from the same run. A missing or ambiguous
source anchor asks the maintainer to review the mapping.

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

For the Windows scenario report, enable
`--features kosha-cli/fast-ntfs-worker,kosha-gui/fast-ntfs-worker` on the
instrumented test command and save its `-- --format pretty` output as a log.
After rendering the Windows landing page, run:

    python xtask-coverage.py --scenarios --input target/coverage/lcov.info --test-log tests.log --output target/coverage/site --commit FULL_40_CHARACTER_SHA --platform windows --scope workspace

Use a separate Cargo target for coverage so instrumentation does not disrupt
ordinary builds. Reports belong under ignored `target/`, not in source commits.
