# Privacy policy

**Effective date: 19 November 2026**

This policy covers the official Kosha CLI (`kosha`), desktop app (`kosha-gui`,
also named **KoshaApp** in Microsoft Store packaging), and the Kosha website.
Kosha is maintained by Neeraj Sharma; its Microsoft Store publisher name is
**Quick Effective Apps**.

**Kosha processes your selected filesystem data locally. It does not automatically
upload your files, scan results, searches, or snapshots to the developer.**
File metadata can still be personal information; local processing does not make
it anonymous. Optional marketplace downloads and visiting the website involve
third-party network services, explained below.

## Information the app accesses and uses

Kosha reads the drives, folders, snapshots, or disk images you choose to inspect.
It uses file and folder names, paths, sizes, timestamps, filesystem attributes,
and ownership information when available or enabled. Names, paths, and owner
identifiers can identify people or reveal sensitive information about them.

Features such as duplicate detection and disk-image inspection may also read
file data locally. Search interpretation and analysis run locally without an
ML model or a cloud search service. There is no Kosha account or sign-in.

Windows desktop capabilities support filesystem access, user-requested elevation
for NTFS analysis, and application-data storage. Administrator access can expose
more local metadata. These capabilities do not mean scan data is uploaded.
Scanning is read-only by default; file-changing actions require the app's
explicit Read-write mode and confirmation. See [safe cleanup](docs/manual/safety.md).

## Information saved on your device

Kosha can save the following in its application directories or locations you select:

- Preferences, pinned paths, saved queries, and custom search recipes.
- Explicit snapshots and automatic saved drive scans containing filesystem
  metadata, including names, paths, sizes, timestamps, and collected ownership data.
- Exported results or comparisons and an audit log of file-changing operations,
  which can include affected paths and metadata.
- Downloaded marketplace catalogs and recipe packs.

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

## Optional marketplace network requests

When you request a marketplace sync or download a recipe pack, Kosha makes HTTPS
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
