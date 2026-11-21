# Using Kosha with agents: access and privacy

**Kosha is not an agent sandbox.** The current CLI uses the filesystem access
available to its process. It does not distinguish a person from an agent running
under the same account, and it has no enforced per-agent folder allowlist.

Scanning and querying do not intentionally rewrite the scanned file contents,
but the CLI as a whole can create, replace and remove files through other
operations. It is not a read-only filesystem sandbox. Names, paths, sizes, dates
and owners can also reveal personal or confidential information. Consider both
what an agent may learn and which write-capable operations it can invoke.

## The supported privacy boundary

Starting with **0.12.0**, Kosha provides an owner-approved metadata-sharing
workflow. The trusted owner selects a recorded subtree, excludes private
branches and chooses which date fields to disclose. Publication creates a fresh
`.koshaview`: unshared entries, original root paths and owners are absent from
the artifact itself. A reviewed SHA-256 binds confirmation to that preview;
publication never replaces an existing destination.

The view consumer and its fixed-source session inspect/query that approved
artifact. Session requests cannot scan, choose another source, read live-source
file contents, load recipes, manage packs or write output files. These restrictions
apply to that interface; an agent with other tools can still use those tools.
See [the commands, protocol and limits](metadata-views.md).

**You control the execution boundary.** Run the entire agent in an environment
that exposes only the intended inputs and tools. Kosha does not set up that
environment, distinguish AI from humans, or restrict other processes running
under your account. The normal CLI retains its OS permissions and write-capable
commands. An unrestricted agent can invoke it differently or bypass it entirely.

GUI publication, confined live traversal and comparison between metadata views
are not included. They are potential enhancements, not required steps for using
the supported artifact-sharing workflow. A view is plaintext metadata, not
encryption, anonymization or automatic secret detection; its recipient can read
every included byte. Metadata already disclosed cannot be recalled.

## Recommended metadata-only handoff

1. As the trusted owner, prepare a snapshot outside the agent's environment.
2. Use `kosha view create` to select the subtree, exclusions, alias and optional
   dates. Review the preview, confirm its digest, and inspect the new artifact.
3. Give the isolated agent only that approved `.koshaview` and the permitted
   consumer interface. Keep the original snapshot, live folders, settings,
   credentials and other records outside its accessible environment.
4. For repeated queries, use `kosha view session --input FILE` with one fixed
   artifact. The controller should impose timeouts and close unwanted sessions.
   Exposing this session does not itself remove other agent tools or shell access.
5. For changed data, the owner reviews and publishes a new artifact. There is no
   automatic live refresh or silent expansion of the shared scope.

## Choose the access you mean to give

| Your intention | What to consider |
| --- | --- |
| Let an agent inspect a live workspace, including its file contents | Isolate the entire agent and its tools using your runtime or OS controls; expose only the intended workspace |
| Share selected recorded metadata without exposing live file contents | Prefer an owner-published `.koshaview`; expose only the approved artifact and consumer interface to an isolated agent |
| Keep an unrestricted host agent away from other folders | Kosha settings cannot enforce this; change the agent's execution permissions or environment |
| Share a whole snapshot deliberately | Every recorded entry remains readable; use the separate snapshot handoff below only when that entire record is appropriate to share |

An unrestricted host shell can run another program or invoke Kosha differently.
A prompt saying “stay in this folder,” a command wrapper or a user-editable policy
does not prevent that. Sandboxing only the Kosha child also leaves an unrestricted
parent agent free to use its other tools. The boundary must cover the agent,
its children and any host tools or services it can reach.

## Sharing a whole snapshot deliberately

1. As the trusted owner, choose a small folder whose recorded metadata you are
   willing to share. Check the names and paths, not just the total size.
2. Scan it and keep a snapshot using the desktop or CLI. Use the exact saved
   path printed by Kosha; `--label` names the record, not its output filename.
3. Review the record before sharing it. It contains the scanned tree's metadata
   and can include its original absolute root, names, owners and timestamps.
   A saved record is **not anonymized, encrypted or automatically redacted**.
4. Provide a copy of only that approved record to an agent environment that
   cannot access your other files, live workspace, shared GUI cache or snapshot
   store. An agent that already has unrestricted host access remains unrestricted.
5. Search the supplied record using an explicit path. For example, after replacing
   the placeholder with the copied artifact's path inside that environment:

```sh
kosha search --load "/path/to/approved.kosha" "files greater than 10 MB" --export json
```

This searches historical metadata without rescanning the original folder.
It does not restrict the agent to the matching rows: **the agent can read every
entry in the artifact you supplied**. Do not supply a whole-drive record when
you intend to share only one folder. No sandbox setup or isolation guarantee is
created by this command. See [snapshots](snapshots.md) and the
[command-line guide](command-line.md) for recorded dates and coverage.

Use an explicit path such as `./approved.kosha` rather than a bare filename if
you want to avoid the normal saved-snapshot filename fallback. Avoid
`--snapshots`, `--caches`, `--cache` and `--since` unless the additional records
they can inspect are also deliberately available to that agent.

## What the existing controls do

| Control | Useful for | Does not provide |
| --- | --- | --- |
| An explicit scan path | Choosing what this invocation scans | A permanent limit on later agent invocations or other tools |
| `--query`, search or `--export-min` | Selecting reported results | Preventing the scan from reading other metadata in its source |
| `--depth` | Reducing traversal depth | A security boundary or an otherwise complete scan |
| Ordinary link and mount-boundary behavior | Avoiding routine traversal into linked or mounted trees | A general sandbox against path replacement, aliases or a hostile filesystem |
| A `.kosha` snapshot | Sharing a historical metadata record without live scanning | Redaction of unselected rows, or permission enforcement based on its recorded root |
| Ask AI's reviewed prompt | Choosing which local metadata to copy, with names and paths omitted by default | Containing the assistant you paste it into |

The CLI has no general-purpose delete or Trash command for scan results. It
does have filesystem-changing operations:

- `--export FORMAT --out FILE` creates an export and can **replace an existing
  destination file** when permissions and filesystem behavior allow it. Choosing
  a valuable file as the output can destroy its previous contents, even if that
  file is also part of the scan or comparison input. There is no GUI Read-write
  mode confirmation for CLI exports.
- `--snapshot` (also implied by `--label`) writes a saved record and may create
  its storage directories.
- `marketplace sync` writes the local catalog; `get` and `install` write installed
  pack files; `uninstall` **removes an installed pack file**. Installation and
  export writers also use lock/temporary files and may remove temporary files
  when an operation fails.
- Shell redirection such as `> report.json` can create or truncate a file before
  Kosha runs; that write is performed by the shell.

Do not authorize arbitrary CLI arguments on the assumption that no filesystem
changes are possible. A delegated interface needs separate write restrictions
as well as read/disclosure limits. `--duplicates` reads file contents.
Snapshot recognition and rejected-file image diagnostics can read file headers;
image scans read their backing files, and an elevated NTFS MFT folder scan reads
volume metadata before selecting the subtree. These are reasons to review the
actual operations and permissions, rather than equating “read-only” with
“metadata-only access confined to this folder.”

## Isolation needs platform-specific care

Use a maintained agent runtime or OS isolation mechanism whose actual filesystem
and host-tool restrictions you understand. Do not run the agent as administrator
or root for routine workspace analysis. A VM or separate restricted account can
provide a different authority boundary; its shared folders and permissions still
need review.

A read-only shared **live folder** can still expose file contents to any reader
inside that environment. For metadata-only sharing, expose the approved record
instead. Also review clipboard sharing, inherited handles, network access,
credentials and bridges to host commands. Choose any AI-provider access
deliberately; isolating files does not prevent an agent from sharing data it is
allowed to read with a service you enabled.

Platform details matter:

- [Windows Sandbox configuration](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-configure-using-wsb-file)
  provides shared-folder, network and clipboard settings. Defaults should not be
  assumed to implement a narrow metadata-sharing policy.
- [Linux Landlock](https://docs.kernel.org/userspace-api/landlock.html) can restrict
  operations, but currently leaves some metadata operations such as `stat`
  outside its coverage. Landlock alone is not a complete metadata-privacy claim.
- [macOS App Sandbox](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox)
  uses explicit resource grants. A normal CLI invocation or ad-hoc-signed app
  does not by itself establish that sandbox around an agent.

These are background references, not tested Kosha launch recipes or a promise
that every agent works in a particular sandbox. Verify the exact environment's
behavior before relying on it to protect sensitive data.

## Scoped metadata tools in 0.12.0

[Issue #10](https://github.com/coolnew/kosha/issues/10) tracks this work. Kosha
0.12.0 implements the first CLI slice; earlier binaries do not include it.
Check `kosha view --help` in your installed build.

- **Owner-approved metadata views:** select a recorded subtree and date fields, remove excluded
  data from the actual artifact, replace private root paths with a chosen label,
  and recalculate totals from shared entries only. Publication previews the
  artifact, requires its reviewed SHA-256 and never replaces an existing file.
  Searching a view does not reopen live paths or discover other records.
- **Explicit scope for cooperative automation:** a fixed-view stdio session accepts
  only inspect/describe/query/close requests and returns structured refusals for
  other operations or unshared fields. It does not contain an unrestricted host agent.
- **Clear scope and freshness:** results identify an approved
  historical subset, source interruptions/read failures, unknown mount contents
  and measured-versus-estimated allocation sizes.

See [the metadata-view workflow](metadata-views.md) for commands, limits and
the distinction between the owner-side publisher and read-only consumer.
Live traversal controls, GUI publication and cross-view comparison are separate
potential enhancements. No general live-folder sandbox has been implemented.

The intended direction preserves normal human CLI use. It does not rely on
guessing whether a caller is AI. Stronger delegated workflows need both limited
disclosure from Kosha and an execution boundary the agent cannot remove.

See also the [privacy policy](../../PRIVACY.md),
[stored-file locations](settings-and-files.md) and
[reviewed AI handoff](cleanup-guide.md#ask-an-ai-about-a-tile-or-entry).
