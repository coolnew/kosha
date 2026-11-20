# Settings, stored files, and privacy

The [privacy policy](../../PRIVACY.md) explains local filesystem information,
optional marketplace requests, website services, and support information.

## Preferences

Open the gear control or find **Settings** in the palette. Preferences include
units, theme, accent, panels, saved searches, pinned folders, columns, and scan
and analysis options. Scan-option changes apply on the next scan; rescan to
update results already on screen.

Theme and accent are independent. **Theme → System** follows the desktop's
light/dark appearance; **Accent → System** uses its accent where supported,
falling back to Kosha's blue when no desktop accent is available. You can
switch either preference back to System without resetting the other.

![macOS Settings showing separate System choices for Theme and Accent in a synthetic workspace](images/gui-macos-settings-dark.png)

| Group | What to look for |
| --- | --- |
| Display | Units, light/dark/system appearance, accent and window tint |
| Scanning | Mount boundaries, image reading, ownership collection, and platform-specific allocation, hardlink, or NTFS options |
| Advanced | Analysis thresholds in the JSON settings file and the version to include in bug reports |

To pin the current folder, open the command palette and choose **Pin … to the
sidebar**. Select the pinned location in the sidebar to scan it. Use the
palette's **Unpin** action or manage pins in Settings.

The fixed **Search settings** field filters option names, explanations, and
keywords, plus saved searches and pinned paths. Multiple words narrow the
results together: try `scan owners`, `dark`, `sparse`, or `thresholds`. The count
and clear button stay visible while the options scroll. Filtering never changes
a preference. The panel remembers its search until you clear it or close the app.
Drag the scrollbar to reach options below the fold in a short window.

The settings panel names the settings file. Advanced users can edit its JSON
while kosha is closed. Back it up first. Invalid settings are reported instead
of silently being overwritten with defaults.

## Application directories

The `kosha` CLI and `kosha-gui` resolve the same per-user configuration and
data directories when run in the same user and installation/package context.
A snapshot saved by either executable
appears in the other's snapshot list. Both read `search-recipes.json`; the CLI
can also opt into the GUI's saved drive records with `--cache`.

| Platform | Configuration | Data |
| --- | --- | --- |
| Windows | `%APPDATA%\coolnew\kosha` | `%LOCALAPPDATA%\coolnew\kosha` |
| Linux | `$XDG_CONFIG_HOME/kosha`, or `~/.config/kosha` | `$XDG_DATA_HOME/kosha`, or `~/.local/share/kosha` |
| macOS | `~/Library/Application Support/app.coolnew.kosha` | Same directory |

Linux uses the fallback when its XDG variable is unset, empty, or relative,
following the [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/).
Automatic drive records live in the data directory because they retain the
last completed observation across sessions. Windows scan
data stays in the local profile rather than roaming with settings. macOS
interactive coverage is less extensive than Windows and Linux.

The Windows paths above describe the portable ZIP installation. Development
MSIX packages can retain those logical paths while Windows redirects new data
to per-package storage. An experimental unvirtualized package can instead use
the ordinary AppData folders directly. Do not assume a packaged installation
and the ZIP share every record, or that records survive package reset/uninstall.
Back up explicit snapshots and export important results outside package-managed
storage before changing installations. Development MSIX builds are test artifacts;
there is no production MSIX distribution yet.

| File or folder | Directory | Purpose |
| --- | --- | --- |
| `settings.json` | Configuration | GUI preferences, saved queries, pins, columns, window placement |
| `search-recipes.json` | Configuration | Optional shared phrase-to-DSL [search recipes](search-recipes.md) |
| `recipe-packs/` | Configuration | Managed copies of explicitly installed recipe packs, shared by the CLI and GUI |
| `marketplace-catalog.snapshot` | Data | Last synced public marketplace catalog and its GitHub commit, shared by the CLI and GUI |
| `rules.d/` | Configuration | Optional JSON rule packs for findings |
| `snapshots/` | Data | Explicit `.kosha` records saved by either executable |
| `drive-cache/` | Data | Automatic historical records written by the GUI and read explicitly by the CLI |
| `audit.jsonl` | Data | Append-only history of file-changing actions from the GUI |

Publisher-qualified packs use `recipe-packs/PUBLISHER/pack-ID.json`. Older flat
`recipe-packs/pack-ID.json` copies remain valid. Personal recipe files are not
merged with downloaded packs. The saved catalog is a validated snapshot of
the complete root/index set; a failed sync preserves its previous contents.
See [marketplace formats](marketplace.md) for identity and capability rules.

Exports go to the location you select, or the CLI's `--out` path. Automatic
drive-snapshot filenames are internal identifiers; the app displays their root
and age instead.

## Back up, reset, or remove records

Close kosha before managing its files. Back up `settings.json`,
`search-recipes.json`, `rules.d/`, and `snapshots/` to retain preferences,
recipes, rule packs, and explicit history. Copy exports you want to preserve.
Snapshots can be large on large trees.

Removing `drive-cache/` discards remembered drive scans; the next drive click
scans again. Removing an explicit snapshot discards that observation. Renaming
`settings.json` out of the configuration directory lets the app use defaults
while preserving the original for recovery. Removing the audit log discards
history and does not undo operations.

## Privacy and rules

Scanning, analysis, and record storage run locally. kosha has no telemetry or
cloud-sync feature. Your OS backup or sync software may still copy these files.

Snapshots and exports contain names, paths, sizes, timestamps, and any collected
ownership data. Audit entries can contain affected paths. These records do not
contain copies of scanned file contents, but their metadata can be sensitive.
Review them before sharing.

Optional rule packs in `rules.d/` extend findings and are validated before use.
Their suggestions still follow normal confidence, mode, and confirmation rules.
Rescan after editing a pack to refresh findings.
