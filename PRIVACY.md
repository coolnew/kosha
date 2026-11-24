# Privacy policy

**Effective date: 21 November 2026**

This policy covers the official Kosha CLI (`kosha`), desktop app (`kosha-gui`,
also named **KoshaApp** in Microsoft Store packaging), and the Kosha website.
Kosha is maintained by Neeraj Sharma; its Microsoft Store publisher name is
**Quick Effective Apps**.

**Kosha processes your selected filesystem data locally. It does not automatically
upload your files, scan results, searches, or snapshots to the developer.**
File metadata can still be personal information; local processing does not make
it anonymous. Optional marketplace downloads and website visits involve
third-party network services. Source builds with SSH scanning can connect
to a machine you choose.

## Information the app accesses and uses

Kosha reads the drives, folders, snapshots, or disk images you choose to inspect.
From Kosha 0.13.0 it also reads listings written by another tool (ncdu JSON
exports) that you choose to import; it reads only that file or standard input,
and never opens the paths named inside it.
It uses file and folder names, paths, sizes, timestamps, filesystem attributes,
and ownership information when available or enabled. Names, paths, and owner
identifiers can identify people or reveal sensitive information about them.

Features such as duplicate detection and disk-image inspection may also read
file data locally. Search interpretation and analysis run locally without an
ML model or a cloud search service. There is no Kosha account or sign-in.
Fragmentation checks query local allocation metadata and skip
links and cloud placeholders. They do not read file contents.

Windows desktop capabilities support filesystem access, user-requested elevation
for NTFS analysis, and application-data storage. Administrator access can expose
more local metadata. These capabilities do not mean scan data is uploaded.
Scanning does not intentionally rewrite scanned file contents. Desktop cleanup
actions require explicit Read-write mode and confirmation. CLI exports can
replace their destination files, snapshots and imports write records, and marketplace
management writes local state or removes installed packs without that desktop
mode switch. See [safe cleanup](docs/manual/safety.md) and
[CLI write-capable operations](docs/manual/agent-privacy.md#what-the-existing-controls-do).

## Optional cleanup guide and AI handoff

The cleanup guide matches known application locations in recorded metadata.
It does not read browser history or conversations or infer your identity.
On Windows, completed whole-distribution WSL scans save a local metadata
record in the automatic drive cache. The optional native WSL2 mode starts the
chosen distribution and invokes its installed Linux Kosha CLI as the default
Linux user. It transfers scan metadata to the Windows app over local process
output. The resulting record is available for read-only browsing.

The optional desktop uv cache-prune action invokes your trusted installed uv
offline with a specific cache path, after Read-write and exact-plan confirmation.
An operation-owned temporary HOME/workspace isolates default profile lookups.
No API keys or inherited tool configuration are passed; audit records include
the executable, target and outcome. External executable behavior is governed
by that tool; Kosha does not sandbox it. Other cleanup guidance does not run
commands. Official-documentation links open only when you
choose them, and the destination receives ordinary web connection information.

Ask AI prepares a local metadata prompt without contacting an AI provider.
Names and paths default off; you can explicitly preview them before copying.
Clipboard contents may be accessible to other apps or sync through your OS.
If you paste the prompt into another service, that service receives the data
under its own policy. Kosha does not send it, store an AI key or read contents
for this feature. Inspect even name-free metadata before sharing.

## Information saved on your device

Kosha can save the following in its application directories or locations you select:

- Preferences, pinned paths, saved queries, and custom search recipes.
- Explicit snapshots, imported listings and automatic saved drive scans
  containing filesystem metadata, including names, paths, sizes, timestamps,
  and collected ownership data.
- Exported results or comparisons and an audit log of file-changing operations,
  which can include affected paths and metadata.
- Downloaded marketplace catalogs, recipe packs and review-only insight packs.

Snapshots and exports do not contain copies of scanned file contents, but their
metadata can be sensitive. Kosha has no built-in cloud sync, advertising,
usage telemetry, or automatic crash-report upload feature.

Records normally remain until replaced or deleted. An automatic drive record
can be replaced by a later completed scan. You can inspect, back up, or remove
saved files using the [stored-file locations and removal instructions](docs/manual/settings-and-files.md).
Uninstalling an app does not guarantee that every record, export, backup, or
unvirtualized application-data file is removed.

Kosha does not separately encrypt or password-protect its saved records. Protect
them with your operating-system account permissions, device encryption, and
backup controls. Other users or software with access to those locations may be
able to read them. Saving to a shared drive, or using OS backup/cloud-sync
software, can copy data elsewhere under your chosen provider's policies.

## Agents and access boundaries

Kosha uses the OS permissions available to its process. It does not identify AI
callers or enforce per-agent folder grants. An agent running with unrestricted
access under your account can use Kosha or other tools to inspect accessible
files. Read-only scanning does not make names, paths or other metadata private.
Query filters select results; they do not confine the underlying scan.

If you give an agent a snapshot, every recorded entry in that artifact is
available to it; existing snapshots are not automatically redacted. Stronger
privacy requires controlling the agent's execution environment and sharing only
approved inputs. See [agent access and privacy](docs/manual/agent-privacy.md) for
current behavior, snapshot handoff and the limits of scoped metadata tools.

Kosha 0.12.0 adds owner-published metadata views. Their new
artifacts contain only the selected recorded entries, a chosen root alias,
mandatory sizes/types and source observation/coverage qualifiers; file dates
are optional and owners/original source paths are omitted. The publisher reads
the explicitly supplied snapshot before projection and writes a new artifact
only after preview-digest confirmation. View consumers and their fixed-source
session operate on the supplied artifact without reopening live source paths,
loading personal recipes or contacting an AI provider. All included metadata
remains readable to the recipient. This does not sandbox the agent or change
ordinary CLI write capabilities. See [metadata views](docs/manual/metadata-views.md)
for current-source availability, limits and retention/sharing responsibilities.

## SSH scanning in opt-in builds

SSH scanning requires an explicit build feature and is off in Settings by
default. When enabled and requested, the client reads local SSH configuration,
host keys and selected authentication material. It connects to the named host
and sends the requested folder path and scan options over SSH. If you select a
key held by an SSH agent, the agent signs the authentication request locally.
Private key material stays on the client.

On the scan host, Kosha walks the folder with the SSH account's permissions.
It returns a read-only snapshot containing paths, sizes and available
metadata. The scan sends no copies of file contents. The selected SSH host and
its operator can receive ordinary connection and authentication information
and can apply their own logging policies. See the
[opt-in SSH instructions](docs/manual/cloud-and-other-machines.md#ssh-scanning-in-opt-in-builds).

## Optional software update checks

On request, or at launch if you turn on daily checks, Kosha queries the public
GitHub Releases API for the latest stable release. The request includes a fixed
Kosha user agent. It carries no files, scan paths, search terms, local version,
account credentials or installation identifier. GitHub receives ordinary
connection information such as the IP address. A local state file records the
preference, the daily check reservation, the last result and the version shown.
Structured output, export and internal remote scan helpers never make launch
update checks. Selecting a download or installation link opens the relevant
service in the browser or system application.

When you request package verification, Kosha fetches the selected release's
asset metadata and checks the local file without uploading it. The optional
Linux direct download also contacts GitHub's release delivery service and
writes the checked archive into the directory you select. A managed Linux
install also retrieves public attestation records by package digest, stages
the download locally, and updates Kosha's per-user executable files and
launchers after verification.
On Windows, requesting a Setup update retrieves the same public provenance
records and opens the verified installer interactively. The downloaded
installer stays in Kosha's local update directory so Setup can relaunch
itself if needed. The macOS update action uses the same GitHub requests and
opens the checked image in macOS DiskImageMounter. The image stays in local
update storage while you finish installation.

GitHub can return an attestation download link to its Azure Blob Storage
endpoint. Kosha follows that link to retrieve the signed bundle without
sending account credentials or local files.

## Optional marketplace network requests

When you request a marketplace sync or download a recipe or insight pack, Kosha makes HTTPS
requests to GitHub's API and raw-content services for
[coolnew/kosha-marketplace](https://github.com/coolnew/kosha-marketplace).
GitHub receives ordinary connection information such as your IP address,
request time, requested catalog/package paths, and the `kosha-marketplace`
User-Agent. Requested paths identify the selected public package and revision.

These requests do not include your scanned file paths, file contents, queries,
snapshots, or exports. Marketplace catalog searches and installed recipes work
locally. You can avoid these download requests by not using sync/get/download
actions, or by installing a recipe file you already have. GitHub handles its
service data under its [privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

Scanning a network share or cloud-mounted drive can also contact the service
you selected through your operating system; that is separate from marketplace access.

## Website, downloads, and platform services

The website is hosted by GitHub Pages. GitHub
[logs visitors' IP addresses for security](https://docs.github.com/en/pages/getting-started-with-github-pages/what-is-github-pages#data-collection),
including when visitors are not signed in. Release downloads, repository visits,
and issue discussions also use GitHub services and its privacy statement.

The website does not add advertising trackers or project-operated analytics
scripts. Its theme can store your light/dark preference in browser local storage;
manual search runs in your browser. The documentation theme loads syntax-highlighting
assets from `cdnjs.cloudflare.com`, which receives the usual browser request
information. See [Cloudflare's privacy policy](https://www.cloudflare.com/privacypolicy/).
You can clear website storage through your browser settings.

Microsoft Store installation, updates, and operating-system diagnostics are
separate Microsoft services, governed by the
[Microsoft Privacy Statement](https://www.microsoft.com/en-us/privacy/privacystatement).
Kosha does not control their retention or your OS diagnostic settings.

## Support and information you choose to share

If you email support or submit a GitHub issue, you choose what to send. The
maintainer and the relevant email/GitHub service receive that content and
associated contact/account information to handle the request. Public issues,
comments, and attachments are visible to others and may be copied or retained
by third parties. Redact personal paths and sensitive details; do not attach
complete snapshots or logs unless you intend to share their metadata.

Support correspondence is retained as needed to investigate and respond,
maintain issue history, and meet applicable obligations. For questions about
information you sent, or a request to access, correct, or delete it, contact
**[neerajsharma.9@outlook.com](mailto:neerajsharma.9@outlook.com)**.
We cannot remotely access or erase records stored on your device, or guarantee
removal of copies held independently by third parties.

## Changes and contact

This policy will be updated when Kosha's data practices change, with the effective
date shown above. It describes official builds; independently modified builds
may behave differently. Privacy questions can be sent to the contact email above.
