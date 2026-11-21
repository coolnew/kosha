# Cloud storage and other machines

This page explains what Kosha can show you today about files that live in cloud
storage or on another computer, and what it cannot. Kosha scans what this
computer can read locally, and it opens saved snapshots. It has no feature that
signs in to an online account or connects to another machine.

## Cloud storage

Kosha does not scan online accounts and sends nothing over the network for this.
To see what your cloud storage holds on this computer, scan the folder that the
provider's own app keeps in sync:

- Google Drive for desktop
- OneDrive
- Dropbox
- iCloud Drive (macOS)

Kosha reads that folder like any other local folder. It sees only files that
have a local entry. The provider's app may contact its service while the folder
is read; that is the app's behavior, not a request made by Kosha.

Some providers leave a **placeholder** for a file whose data is not stored on
this computer. Kosha recognizes a placeholder only from a marker the operating
system sets on the entry, never from a name:

- A placeholder's full length is shown as its apparent size. This is an upper
  bound, because most of the data is not on this disk.
- Kosha never opens a placeholder, so scanning does not download it.
- Duplicate searches and fragmentation checks skip placeholders and report how
  many they skipped.
- Linux has no standard marker, so no file there is recognized as a placeholder.

[Cloud-synced folders](scanning.md#cloud-synced-folders) lists each marker and
how far it has been tested. Google Drive for desktop has not been verified with
Kosha. If its streamed files carry no marker, Kosha treats them as local, so do
not run a duplicate search on a streamed Drive folder.

### Deleting in a synced folder

Deleting a synced file usually deletes the online copy too. From Kosha 0.12.2,
a Trash or permanent-delete plan that includes placeholders shows a warning with
the count and offers **Skip cloud-only items**. See
[cloud placeholders](safety.md#cloud-placeholders).

The warning has limits. A file that is fully downloaded carries no marker, so
it gets no warning even though deleting it may still delete the online copy.
Versions before 0.12.2 do not warn. Keep Kosha in Read-only mode for synced
folders you do not intend to clean, and use the provider's app to free space.

### Warning: mounted cloud filesystems

Some tools present an online account as a mounted filesystem, for example
`rclone mount`, a FUSE filesystem, or a network drive made with WinFsp.

- Reading a file through such a mount downloads it.
- Kosha cannot flag those files as placeholders, so it treats them as ordinary
  local files.
- Do not run a duplicate search, or any exact or content-reading feature, on a
  mount. Those features read file contents.

By default a scan does not descend into a mounted filesystem below the folder
you scan. On Linux and macOS Kosha compares device IDs, and on Windows it
recognizes volume mount points. It shows the mount point as a boundary and does
not enter it unless you turn on crossing filesystems (`--cross-fs` on the
command line). If you choose a mount itself as the scan folder, Kosha lists its
contents, and the mount software may contact the service to do so. Kosha has
not been tested against a live rclone, FUSE or WinFsp mount on any system, so
treat what happens there as untested.

## Other machines

Kosha cannot scan another machine from inside Kosha. The workflow that works
today uses Kosha on both machines and a snapshot file. From Kosha 0.13.0, a
listing made by ncdu on the other machine can be imported instead; see
[without Kosha on the other machine](#without-kosha-on-the-other-machine).

1. Install Kosha on the other machine and scan there, keeping a snapshot:

   ```sh
   kosha /srv --snapshot --label "build-box /srv"
   ```

   The report ends with a `kept at` line, and `kosha --snapshots` lists saved
   snapshots with their paths. Kosha writes the snapshot to its own data
   directory; the command has no option to choose another location.
2. Copy the `.kosha` file to this computer with any tool you already use, such
   as `scp`, a shared folder or a USB drive. Snapshots are in the `snapshots`
   folder of the data directory:

   | System | Snapshot folder |
   | --- | --- |
   | Windows | `%LOCALAPPDATA%\coolnew\kosha\snapshots` |
   | Linux | `$XDG_DATA_HOME/kosha/snapshots`, or `~/.local/share/kosha/snapshots` |
   | macOS | `~/Library/Application Support/app.coolnew.kosha/snapshots` |

3. Open it on this computer:

   ```sh
   kosha ./build-box.kosha --stats
   kosha search --load ./build-box.kosha "files over 1 GB"
   kosha-gui ./build-box.kosha
   ```

   Use `./` or a full path so Kosha opens the file you copied. To see how a
   tree changed, compare two snapshots with `--since`; see
   [snapshots](snapshots.md).

A snapshot is a historical record of the other machine's tree at the time it
was taken. Opening it does not touch that machine, and it is not the disk as it
is now. File actions are not available on it, and it holds no file contents,
so scan again before changing files. An older Kosha
refuses a snapshot written by a newer one, so keep versions aligned.

A snapshot can be opened on a different operating system from the one that
recorded it. From 0.12.2, opening a Linux snapshot on Windows replaces file
names that Windows cannot represent with the replacement character and reports
how many; see [snapshots from another system](snapshots.md#snapshots-from-another-system).

### Without Kosha on the other machine

From Kosha 0.13.0, run [ncdu](https://dev.yorhel.nl/ncdu) there and import its
JSON export here:

```sh
ssh build-box ncdu -co- /srv | kosha import --from ncdu - --label "build-box /srv"
kosha --snapshots
```

You open the SSH session and run ncdu; Kosha only reads what arrives on its
standard input (or a file you name), starts no program and makes no network
request. The record says everywhere that it was imported and not measured by
Kosha. See [imported listings](snapshots.md#imported-listings).

## What is not available yet

- Scanning online accounts directly.
- Scanning other machines from inside Kosha.
- Importing listings from tools other than ncdu, such as rclone or QDirStat.
  Importing ncdu JSON exports is available from Kosha 0.13.0.

## Tell us what is missing

If your files live somewhere Kosha cannot reach today, tell us with the
[scanning sources issue form](https://github.com/coolnew/kosha/issues/new?template=scanning_sources.yml).
Say which provider or tool you use, your operating system, the rough size of the
tree and what you tried. The request is public, so leave out private paths,
account names and file names.
