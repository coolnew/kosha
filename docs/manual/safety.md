# Cleaning up safely

For data access, local records and optional network services, see the
[privacy policy](../../PRIVACY.md).

## Modes and permissions

kosha starts in **Read-only** mode. Scanning, searching, and reviewing findings
do not modify scanned files. The app can still save its own settings, snapshots,
and automatic drive history.

Use the mode control or **Ctrl+Shift+M** to request Read-write mode. Read the
confirmation before enabling it. Administrator privileges improve access; they
do not automatically enable Read-write mode.

## Review the plan

An actionable finding offers an appropriate operation. Review the named paths,
estimated effect, and plan before confirming. The app checks file identity again
before changing files and refuses protected system locations, filesystem roots,
and the home directory itself. Operations can fail when files move, permissions
change, or other processes keep files open.

Trash is preferred to permanent removal where supported. Trash is not a backup:
recoverability depends on the OS and destination. Permanent actions cannot be
undone by kosha. Some findings are review-only and have no destructive action.

After an action, kosha rescans rather than treating the old measurements as
current. There is no continuous filesystem watcher. Ordinary browsing stays at
the state observed during its scan until you rescan.

## Historical and incomplete views

A saved drive scan or explicit snapshot describes the past. File-changing
actions require a fresh scan. An incomplete scan covers only the part reached;
partial results may also include access errors. Review these indicators before
relying on totals or findings.

## Audit history

File-changing operations are recorded in `audit.jsonl` in the application-data
directory. The palette can show the log's location. It is an operation record,
not a backup or undo mechanism, and can contain affected paths and filesystem
metadata. See [settings and stored files](settings-and-files.md).
