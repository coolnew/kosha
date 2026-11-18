# Scanning drives, folders, and images

## Choose a scope

The sidebar offers personal folders, pinned folders, volumes, and, where
available on Windows, WSL distributions. A drive click opens its last saved scan
if one exists. Choose **Scan again** or **Ctrl+R** to refresh it. **Choose a
folder** starts a fresh scan of any accessible folder.

By default, directory walks do not descend into other mounted filesystems.
Scan settings can change this. Symbolic links are measured without following
their targets. A scan describes its chosen scope, not necessarily an entire disk.

## Speed and remaining time

The scan view shows entries found and queued directories. An entry can be a file
or a directory. Speed uses a sliding window of up to 30 seconds, including time
spent waiting for filesystem reads.

When a previous finished scan of the same scope is available, kosha estimates
remaining time from its entry count and the current entries per second. The
view identifies that reference count. The estimate starts after enough
measurements are available and changes as scanning proceeds.

The total is historical: files may have been added or removed. If the new count
exceeds it, or progress stalls, kosha stops showing a countdown. A first scan
with no prior total shows throughput and **total entries unknown**. Some index
and disk-image strategies report different work units, so they show progress
without using a tree-entry countdown.

## Stop and completeness indicators

Use **Stop scan** or **Escape**. If a modal panel is open, Escape dismisses it
first. **Stopping scan…** acknowledges the request while current filesystem reads
return. Network storage or a device waiting on hardware can delay that return.

When usable results remain, the view shows **Incomplete scan** persistently.
Stopping does not replace the previous automatic drive snapshot. Some scan
strategies cannot return a usable tree after cancellation and return to idle.

| Indicator | Meaning |
| --- | --- |
| Incomplete scan | The scan was stopped, or a depth limit left folders unscanned. |
| Partial / unreadable | Some entries could not be accessed during the scan. |

Both conditions can occur together. Totals are lower bounds whenever coverage
is incomplete or entries were unreadable. A finished scan with access errors
can still be saved and keeps its unreadable indicators.

## Faster strategies and images

On Linux, a normal scan uses a parallel directory walk. Shift-click a volume
to bypass its saved observation and scan it again. The scan still respects
your account's filesystem permissions and the configured mount boundaries.

On Windows, **Shift-click** a location to request the filesystem's index for that
scan and bypass its saved view. NTFS Master File Table reading needs administrator
privileges. Reading a WSL distribution's image is another strategy. Settings can
request these strategies for future scans. Unavailable strategies fall back
where possible, with the reason reported. Administrator and Read-write are
separate choices.

Use **Scan a disk image** for a filesystem image. Readers include ext2/3/4, NTFS,
XFS, FAT12/16/32, exFAT, ISO 9660, and UDF, with supported VHDX and partition
layouts. Images are read without mounting or modifying them. A live VM's image
reflects what was written to the image, which can differ from the VM's current
view. A multi-filesystem image reports which filesystem was selected.

Next: [Results and search](results-and-search.md).
