# Using Kosha with agents: access and privacy

**Kosha is not an agent sandbox.** The current CLI uses the filesystem access
available to its process. It does not distinguish a person from an agent running
under the same account, and it has no enforced per-agent folder allowlist.

Read-only scanning prevents changes to your source files; it does not make their
metadata private. Names, paths, sizes, dates and owners can reveal personal or
confidential information. Giving an agent a command-line tool is a decision
about what that agent may learn, even when the tool has no delete command.

## Choose the access you mean to give

| Your intention | What to consider |
| --- | --- |
| Let an agent inspect a live workspace, including its file contents | Isolate the entire agent and its tools using your runtime or OS controls; expose only the intended workspace |
| Share recorded metadata without exposing live file contents | Prepare and review a snapshot yourself, then supply only that approved artifact to an isolated agent |
| Keep an unrestricted host agent away from other folders | Kosha settings cannot enforce this; change the agent's execution permissions or environment |
| Share selected entries or fields from a broad scan | Current snapshots are not redacted views; do not share the broad record and assume a query hides its other data |

An unrestricted host shell can run another program or invoke Kosha differently.
A prompt saying “stay in this folder,” a command wrapper or a user-editable policy
does not prevent that. Sandboxing only the Kosha child also leaves an unrestricted
parent agent free to use its other tools. The boundary must cover the agent,
its children and any host tools or services it can reach.

## A practical snapshot handoff today

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

The CLI has no cleanup/delete command, but requested exports, snapshots and
marketplace management can write files. `--duplicates` reads file contents.
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

## What Kosha can improve

[Issue #10](https://github.com/coolnew/kosha/issues/10) tracks the design. The
following are **proposals, not features in 0.11.0**:

- **Owner-approved metadata views:** select entries and fields, remove excluded
  data from the actual artifact, replace private root paths with a chosen label,
  and recalculate totals from shared entries only. Searching a view would not
  reopen live paths or discover other records.
- **Explicit scope for cooperative automation:** consistently reject unapproved
  operations and report clear machine-readable refusals. This could reduce
  mistakes through Kosha; it would not contain an unrestricted host agent.
- **Clear scope and freshness:** make it clear whether a result covers an approved
  subset, is historical, was interrupted or encountered unreadable entries.

The intended direction preserves normal human CLI use. It does not rely on
guessing whether a caller is AI. Stronger delegated workflows need both limited
disclosure from Kosha and an execution boundary the agent cannot remove.

See also the [privacy policy](../../PRIVACY.md),
[stored-file locations](settings-and-files.md) and
[reviewed AI handoff](cleanup-guide.md#ask-an-ai-about-a-tile-or-entry).
