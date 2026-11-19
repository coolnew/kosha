# Troubleshooting

## macOS says the app is damaged or cannot be verified

Some early DMGs, including 0.10.5, shipped without a complete app-bundle seal.
Development packaging after 0.10.6 ad-hoc signs the fully assembled bundle and
checks its signature before and after DMG packaging. This is an integrity fix,
not Developer ID signing or notarization; it does not guarantee Gatekeeper
will accept a fresh download. Check your version and the release's macOS notes.

Download only from the official release page, compare the archive's SHA-256
with its published sidecar, and report persistent launch failures with your
macOS version and exact message. A matching digest alone does not establish
publisher trust. Avoid disabling system security or treating an unexplained
“damaged” message as proof the app is safe. See [installation](getting-started.md).

## macOS has no Quit menu or Command+Q does not work

Development after 0.10.6 adds **Kosha → Quit Kosha** and **Command+Q**.
For older builds, open the command palette with **Command+K**, type
`Quit kosha`, and run it. If the updated controls still fail, include whether
focus was in the file list, a text field, or a modal in your report.

## The Linux desktop app or file picker does not open

See [Linux dependencies and launch diagnostics](linux.md#if-launch-or-a-dialog-fails)
for shared libraries, graphics drivers, and the desktop FileChooser portal.

## A drive shows old results

Look for **Saved scan** and its age. Drive clicks prefer their last saved
observation, including in the same session. Choose **Scan again** or **Ctrl+R**
for a fresh view. Check that you launched the updated executable if controls
differ from this manual.

## Escape appears slow

The view should change to **Stopping scan…**. An in-flight filesystem read must
return before kosha can finish stopping. Network drives, offline storage, and
devices waiting on hardware can delay this. Escape dismisses an open modal
panel first; **Stop scan** directly requests cancellation. Stopped results show
**Incomplete scan**.

## The remaining-time estimate is missing or changes

An unknown tree has no reliable total count. The estimate needs a previous
finished scan and enough current measurements. Added files, removed files,
stalls, and per-entry costs can change it. Entry counts and throughput remain
useful when a countdown cannot be shown.

## A drive scan was not remembered

Cancelled and depth-limited scans deliberately preserve the previous record.
Saving can also fail if the data directory is unavailable, permission is denied,
or storage is full. Read the app's notice. Saving happens in the background;
closing the process immediately can interrupt a write. See [file locations](settings-and-files.md).

## Totals differ from a drive's used space

Check incomplete/unreadable indicators, mount boundaries, apparent versus
allocated size, and scan strategy. Search `unreadable` to inspect access errors.
Administrator privileges can improve Windows coverage but do not guarantee
access to every file. Drive usage also includes overhead and reserved storage.

## An action is unavailable

Check Read-only mode, historical snapshot views, confidence, and protected-path
restrictions. A historical view needs a fresh scan before file-changing actions.
Some findings are informational and have no cleanup operation.

## Settings do not change the current results

Scan settings apply on the next scan. Rescan after changing scope, ownership,
allocation measurement, or strategy. If settings are invalid, close the app and
repair or rename that file. Retain the original for recovery.

## Report a problem

Include version, OS, scan strategy, relevant settings, and reproduction steps.
State whether results were fresh, historical, incomplete, or unreadable. Remove
private paths and account data from shared screenshots, exports, snapshots, and logs.

Return to the [manual contents](README.md).
