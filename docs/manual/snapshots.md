# Saved drive scans, snapshots, and comparisons

Both kinds of record store sizes and metadata, not copies of file contents.

New snapshots use format 2 and retain filesystem creation dates where the scan
recorded them. Format 1 records still open, with creation dates marked unknown.
Keeping an older automatic record preserves its original metadata; it does not
invent missing dates. Older kosha builds cannot open format 2 records.

| Record | Created by | In snapshot list | Retention |
| --- | --- | --- | --- |
| Automatic drive scan | Finished drive-root scan in the desktop app | No | One replaceable slot per drive root |
| Explicit snapshot | **Keep a snapshot**, or CLI `--snapshot` | Yes | Kept until removed |

The CLI lists these separately: `kosha --snapshots` shows explicit snapshots;
`kosha --caches` shows automatic drive records and their age without rescanning.
Cache listings read headers only. See the [command-line guide](command-line.md#explicitly-use-the-gui-cache)
for JSON/JSONL inventory and error statuses.

## Automatic drive scans

A finished drive scan is written in the background. Selecting that drive again
can display the finished record immediately in the same session. After an app
restart, the drive click reads the saved record from disk.

**Saved scan** beside the breadcrumb shows its age. **Scan again** or **Ctrl+R**
refreshes the disk view. Shift-clicking the drive requests a new scan using an
available filesystem-index strategy. Missing or unreadable saved records trigger
a scan.

A new finished scan replaces the automatic record. Cancelled and depth-limited
scans do not. Finished scans with inaccessible entries can be kept and retain
their partial indicators. Folder scans are not saved automatically.

History follows the drive's root path. A replacement removable drive or changed
drive-letter assignment can therefore open a record of the previous disk. Check
the age and choose **Scan again** before relying on it.

## Explicit snapshots

After a fresh scan, choose **Keep a snapshot**. It appears in the sidebar with
its root, age, and storage size. Explicit snapshots do not overwrite one another.
You can also open a `.kosha` snapshot directly.

In the CLI, use its full/relative path or exact stored filename, including
`.kosha`. Existing local files take precedence over a stored copy with the same
name. `./NAME.kosha` requires that local path; the full store path printed by
`--snapshots` selects the stored copy explicitly. The same rules apply to
`--since` and search `--load`. Corrupt local files fail without substituting a
different saved record; labels and partial filenames are not shortcuts.

While viewing an automatic **Saved scan**, choose **Keep saved scan as snapshot**
to keep that historical observation without rescanning. The note under the
button shows the original date; the command palette offers the same action.
The explicit snapshot keeps that date, including its original timestamp precision,
and the scan's measurement metadata. The confirmation notice repeats that the
original scan date was preserved.

The snapshot file's filesystem creation time reflects when you kept it; kosha's
displayed scan date remains the original observation date.

The kept copy appears in the snapshot list and survives future replacement of
the automatic drive record. Keeping a cached scan does not make it current:
its age and historical-view safeguards still apply. When you open an explicitly
kept snapshot, it is already in the list and does not offer another keep action.

A newly saved stopped scan retains **Incomplete scan** when reopened. Older
snapshot formats may lack that specific cancellation detail. Prefer a finished
scan when making a baseline with full traversal coverage.

## Snapshots from another system

A `.kosha` file can be opened on a different operating system from the one
that recorded it. Names are kept exactly as recorded wherever the opening
system can represent them. Linux allows file names that are not valid UTF-8
text, and Windows cannot represent some of them. When such a snapshot is opened
on Windows, those names are shown with the replacement character � and the
app's notice (or a `note:` line on the CLI's standard error) says how many names
were affected. Sizes and the folder structure are unchanged. A comparison may
report those entries as gone and new, and a [metadata view](metadata-views.md)
refuses to share a replaced name.

To bring a snapshot from another machine, see
[cloud storage and other machines](cloud-and-other-machines.md#other-machines).

## Compare observations

The snapshot sidebar offers **All** and **This location**. This location shows
explicit snapshots whose drive or folder root matches the open scan's root;
drilling into a subfolder does not change that root. The choice is remembered.
With no scan open, All is available so you can still open any saved snapshot.

Use the palette's comparison action to compare the current tree with a kept
snapshot. Comparisons describe recorded observations, not continuous activity.
Rescan to refresh the current side.

**What changed** compares the tree already in memory with the selected snapshot.
It does not scan again. This also works when the current view came from an
automatic drive record or another explicit snapshot. Reading the selected record
and computing the comparison run in the background; large records can still take
time. The older observation is always the Before side.

The comparison shows both roots and observation dates, separate Before and After
sizes, and a net change across files. Directory totals overlap their children and
are excluded from the net. Filter directories/files and All, New, Grew, Shrank,
or Gone. Hover for a full path; click a row to copy its relative path.

![Kosha on Linux showing a snapshot comparison for a synthetic studio workspace, with change filters, before and after sizes, and export controls](images/gui-linux-comparison.png)

Choose **Markdown** or **JSONL** above the comparison, or the matching command
palette export action. Exports capture the visible filters when invoked and
include the roots, observation timestamps, threshold, and warnings. Markdown
separates directory and file tables. JSONL starts with a comparison metadata
record followed by one change record per visible row, with exact byte sizes and
signed deltas. The overall net includes all file changes, including changes
below the threshold; it is not the sum of filtered rows. Exporting does not rescan.

Use the same root and comparable scan settings. Differences in mount boundaries,
allocation measurement, hardlink rules, access, and completeness can look like
changes in files. Read comparison warnings.

Historical views retain snapshot safety guards: scan again before file-changing
actions. A record of yesterday's file cannot establish its current identity or
contents. See [file locations](settings-and-files.md) for backup and removal.
