# Share an approved metadata view

**Available starting with Kosha 0.12.0. Earlier binaries do not include these commands.**
Check `kosha view --help` in your installed build. These tools are currently CLI
only. They complement an isolated agent runtime; they do not create one. Read
[agent access and privacy](agent-privacy.md) before delegating filesystem tools.

A metadata view is a new, inert JSON artifact containing the entries you chose
to share. Unlike a query over a whole snapshot, excluded data is absent from the
artifact itself. Views have schema `kosha.view.v1`; use `.koshaview` for their
filenames. Open them with `kosha view`, not ordinary snapshot commands or the GUI.

## 1. Prepare a snapshot as its trusted owner

Take or choose a snapshot using the usual [snapshot workflow](snapshots.md).
The publisher reads that explicitly selected record, not the live folder. It
does not search the shared snapshot store or fall back to another source.

Publication can read a broad snapshot before selecting a narrow subset. That is
a limit on **what is disclosed**, not a promise that the publisher read only that
subset. If acquisition itself must be confined, prepare the source inside an
appropriately restricted environment first.

## 2. Preview the selected recorded subtree

Replace the example paths with your snapshot and desired output. The recorded
snapshot in this example contains `Projects/sample`, with a private subtree
inside it. Paths after `--root` and `--exclude` are exact, case-sensitive recorded
components separated by `/` on every OS. They are not live filesystem paths.

```sh
kosha view create --snapshot "/path/to/source.kosha" --root "Projects/sample" --alias workspace --exclude private --out "/path/to/workspace.koshaview"
```

This prints a JSON **preview** and writes no artifact. Use `--root .` to select
the snapshot's root deliberately. Repeat `--exclude` for other exact subtrees;
exclusions are relative to the selected root. Missing selections are errors;
they are never silently broadened. You cannot exclude the shared root itself.

Review the alias, fields, source date, coverage, entry count, totals, sample paths
and `sha256`. The preview shows up to 12 paths, not every shared row. The selection
and all entries in the resulting artifact remain your responsibility; after
publishing you can inspect/search it locally before giving it to anyone else.

### What is shared

- **Always:** a logical root alias, relative names, directory/file/link categories,
  recorded apparent/allocated sizes, unreadable/boundary markers and observation
  time. Coverage also records source limitations and allocation quality.
- **Opt-in:** `--modified` shares file modification times; `--created` shares
  creation times when known. Unknown dates remain unknown.
- **Excluded:** original absolute roots, original snapshot labels/filenames,
  owners, unselected nodes, unused source-name buffers and original scan
  statistics. A new name arena and tree are built from shared entries only.

An alias has 1–64 ASCII letters/digits/`.`/`_`/`-`, starting with a letter or
digit. Shared names must be portable UTF-8 components: at most 255 bytes, without
path separators, colons, control/directional-control characters or `.`/`..`.
Names that cannot be represented safely are refused, not silently rewritten.
Excluding their subtree may let you share the remainder.

Names and sizes can still be sensitive. This is selective disclosure, not
anonymization, encryption, a signature or an automatic secret detector.

## 3. Publish exactly the preview you reviewed

Copy the preview's lowercase SHA-256 into the following command. Keep the same
snapshot, selection, alias and field options as the preview.

```sh
kosha view create --snapshot "/path/to/source.kosha" --root "Projects/sample" --alias workspace --exclude private --out "/path/to/workspace.koshaview" --confirm --expect-sha256 PREVIEW_SHA256
```

The publisher recomputes the view and refuses with `view_changed` if its bytes
differ from the preview. It then creates the new artifact through a staged,
flushed no-clobber write. **An existing destination, including a destination
symlink, is not replaced.** Choose a new filename for another publication.
The parent directory must already exist. Its location is an owner-selected
write destination, not a confined directory sandbox.

The digest detects changes between preview and publication. It is not a secret
capability and does not prove a human approved the command. An unrestricted
agent can still invoke the publisher or other tools itself. Publish as the
trusted owner, then give an isolated agent only the approved artifact.

## 4. Inspect and search without live-file access

```sh
kosha view inspect --input "/path/to/workspace.koshaview"
kosha view query --input "/path/to/workspace.koshaview" --query "file and size > 10mb" --limit 100
kosha view query --input "/path/to/workspace.koshaview" --search "files ending with md" --limit 100 --offset 0
```

These consumers read only the explicitly supplied view as their data source.
They do not reopen its original snapshot or live paths, scan, refresh caches,
read personal/marketplace recipes, inspect owners, hash file contents or create
output files. Natural-language interpretation uses built-in rules only. No
fallback to ordinary CLI behavior occurs when a view or option is invalid.

Results are JSON on stdout. Errors are `kosha.error.v1` JSON on stderr with a
stable `code`, bounded message and `retryable` flag; an initial refusal exits 1
with no result on stdout. Errors do not repeat input file paths or parser values.
Shell redirection or a client saving stdout can still write files independently.

`rows` contains at most the requested limit, in recorded tree order, with
logical paths such as `workspace/src/main.rs`. These paths are **not executable
authority or host filesystem paths**. `matches` counts all matching non-root
entries, and `has_more`/`offset` support pagination. The root is reported by
inspect, not returned as a search match. A recipient can read every entry in
the artifact regardless of the particular query or page.

Owner predicates and flags not represented by the view are refused with
`operation_not_granted`. Modification/age or creation-date predicates require
the corresponding shared field, even under NOT/OR. Within an enabled date
field, missing values remain unknown through negation. Do not interpret a
refusal as zero matches.

## 5. Bind an automation session to one view

```sh
kosha view session --input "/path/to/workspace.koshaview"
```

The process loads and validates the view once, closes the input file, and prints
a JSON `session_ready` record. It then accepts one JSON request per stdin line.
For example, send these lines through a client or enter them interactively:

```jsonl
{"op":"inspect","id":"scope"}
{"op":"describe","query":"files ending with md","mode":"natural","id":"intent"}
{"op":"query","query":"file and ext = md","mode":"dsl","limit":20,"offset":0,"id":"results"}
{"op":"close","id":"done"}
```

Responses to valid request objects echo the bounded request ID; malformed or
unknown-field requests can return a null ID. `describe` returns the DSL without
executing a query; `uses_loaded_view` is true and `live_source_opened` is false.
Malformed requests and unknown fields are refused. No request can select another
input, supply a path/output, load recipes, scan, compare external snapshots,
manage packs or run cleanup. Session-request errors are JSON responses on
stdout and do not silently end up in another CLI command. A valid `close` or
EOF ends normally; a fatal input/budget error exits 1 with a JSON stderr error.

This is **cooperative operation scope over a loaded view**, not an enforced
per-agent policy for the whole machine. Your controller chooses the startup
artifact; your runtime must prevent bypass through another host command if that
is required. Closing the process stops future responses but cannot recall data
already shared or copies the recipient retained.

## Interpret the totals and coverage

`source` is `historical_metadata_view`, with the original `observed_unix`,
`age_seconds`, evaluation `query_clock_unix` and an `observed_in_future` warning
flag for an inconsistent source clock. Session queries use the session-start
clock for stable interpretation during its short lifetime.

`coverage.scope` is `owner_selected_metadata`. Directory totals are recomputed
from **shared non-directory entries only**. They exclude original directory
inode/allocation bytes and intentionally unshared descendants; they are not
whole-drive usage or promised recoverable space. Do not sum directory and file
rows together. Child/file counts also describe the shared tree.

`source_incomplete` records source cancellation/depth limits;
`source_access_limited` records known source read failures. `lower_bound` also
flags unknown/unvisited contents. A mount-boundary row does not mean an empty
directory, and neither `empty` nor `not empty` matches it as known contents.
Source flags describe the original record conservatively, including limitations
that may have occurred outside the chosen subtree; no hidden counts/names are
included. Intentional exclusions define the shared scope, not scan errors.

`allocation_exact` preserves the source's measured-versus-estimated allocation
qualifier. False means allocated/wasted figures can be estimates, particularly
for ordinary Windows scans. Publication refuses a selected file whose source
allocation was zeroed by hardlink folding: capture a `--no-hardlinks` snapshot
instead of inventing an allocation for that alias.

## Limits and unsupported operations

| Limit | Value |
| --- | --- |
| Encoded artifact | 64 MiB |
| Shared entries, including root | 250,000 |
| Total name bytes | 16 MiB |
| Tree depth / logical path | 128 levels / 4,096 UTF-8 bytes |
| Exact excluded subtrees | 256 |
| Query text / request ID | 2,048 / 128 UTF-8 bytes |
| Rows per response | 1–1,000; default 100 |
| Session request line / request count | 8 KiB / 1,000 |
| Request admission age | Ten minutes after session start |

Matching cooperatively checks a 30-second deadline. The age check occurs when
a request is received; it does not forcibly terminate an idle process or unblock
a stalled input/output pipe. Controllers should impose their own timeouts and
terminate sessions they no longer need. Responses and artifacts are untrusted
data, never commands or instructions to execute.

The current slice has no GUI publisher, live filesystem allowlist, cross-view
comparison, sandbox launcher or automatic sensitive-data classifier. Ordinary
Kosha exports can still replace their destination; no-clobber publication here
does not change other commands. Follow [issue #10](https://github.com/coolnew/kosha/issues/10)
for further work and [agent privacy](agent-privacy.md) for the execution boundary.
