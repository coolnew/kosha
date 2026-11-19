# Define your own search recipes

Custom recipes translate a phrase you choose into kosha's query language (DSL).
Exact aliases, typed templates, named regex captures and typo suggestions are
described in [marketplace formats and extended recipes](marketplace.md).
They work in the GUI's **Natural** mode and in `kosha search`. No model, network
service or script is involved. Typed rules can use bounded regexes to extract
values from the query phrase. Saved searches remain a separate
way to keep an already useful query.

For a complete pack/manifest example, test corpus and authoring pitfalls, see
[Author and test recipe packs](recipe-authoring.md).

## Explore built-in recipes

On Linux, open **More examples** in Natural search or press **Ctrl+/** and
search for a term such as **codex**, **CMake**, or **model**. An example chip
runs the phrase in Natural mode. Query help shows the DSL and inserts that
expression when clicked. List the platform catalog in the CLI, then inspect
or run a phrase.

```sh
kosha search --list-recipes
kosha search --describe "rust build files"
kosha search --path . "cmake build files"
kosha search --path "$HOME" "old coding agent sessions"
```

| Group | Examples |
| --- | --- |
| Linux logs and caches | system logs, journals, dumps, distribution package caches |
| Build and development files | Cargo build artifacts and dependency caches, CMake output markers, Go build and module caches, Python bytecode and tool caches, npm and ccache |
| Coding agents and editors | project instructions, settings, sessions or logs for Claude Code, Codex, Gemini CLI, Copilot CLI, Pi, and VS Code |
| Local models | Hugging Face, Ollama, LM Studio, and vLLM caches or models, plus large GGUF and safetensors files |

These built-ins are available when the executable runs on Linux, including
a Linux executable running in WSL. Other platforms retain the general search
examples. The recipes match names, paths, sizes, and ages in the selected
scan. They do not inspect transcript or model contents. A path match is a
hint for investigation, so review matches before acting or sharing exports.
Session and settings filenames can reveal private information.
[Claude Code](https://code.claude.com/docs/en/settings),
[Codex](https://github.com/openai/codex/blob/main/codex-rs/config/src/config_toml.rs),
[Gemini CLI](https://geminicli.com/docs/cli/session-management/),
[Copilot CLI](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference),
[Pi](https://upd.dev/badlogic/pi-mono/src/branch/main/packages/coding-agent/docs/settings.md),
and [VS Code](https://code.visualstudio.com/docs/configure/settings).

The examples use conventional tool layouts. Variable overrides such as
`CARGO_TARGET_DIR`, `CODEX_HOME`, `HF_HOME`, `OLLAMA_MODELS`, and
`VLLM_CACHE_ROOT` may put files elsewhere. Scan that location and adapt
the DSL or define a custom recipe. In particular, the CMake recipe finds
`CMakeCache.txt` and files under `CMakeFiles`, so it works with different
build folder names and may omit outputs elsewhere in the build tree.
For an Ollama system install, choose a scan that includes the service's
model directory, often `/usr/share/ollama/.ollama/models`.
[Cargo](https://doc.rust-lang.org/stable/cargo/reference/build-cache.html),
[CMake](https://cmake.org/cmake/help/latest/manual/cmake.1.html),
[Hugging Face](https://huggingface.co/docs/hub/en/local-cache),
[Ollama](https://docs.ollama.com/faq), [LM Studio](https://lmstudio.ai/docs/app/advanced/import-model),
and [vLLM](https://docs.vllm.ai/en/latest/configuration/env_vars/).

## Copy a recipe pack from Git

Copy the Linux or Windows recipe pack from the
[versioned recipe packages in Git](https://github.com/coolnew/kosha-marketplace/tree/main/packages/recipes/coolnew).
Choose the package/version folder and copy its `pack.json`. The standalone
marketplace is the maintained source; the application repository
does not carry a duplicate catalog or production packs.
The downloadable archives omit these optional packs. Use a pack directly
with the CLI, or copy entries into your shared `search-recipes.json` and
restart the GUI. You can add, edit, or remove entries with any text editor.

Pack files use a JSON object with `schema: 1`, `kind: "recipe-pack"`,
a `metadata` object, and a `recipes` array. Metadata includes a stable
lowercase ID, title, description, version, and optional tags. Copy the
whole file to see the pack title and description in the GUI's examples.
If you already have personal recipes, copy the individual entries from
the pack's `recipes` array into your own file. Older Kosha versions can
read arrays limited to their supported entry types; richer rules require a
current executable.

Run these local-file examples from the directory containing your
`kosha-marketplace` checkout. Adjust the path to your downloaded file, or use
`kosha marketplace sync` and `kosha marketplace get` below instead.

```sh
kosha search --list-recipes --recipes kosha-marketplace/packages/recipes/coolnew/linux-essentials/1.0.0/pack.json --filter "postgresql wal"
```

The listing shows the pack metadata. It filters active recipes against their
phrase, DSL, and pack metadata without opening a scan. In the GUI, Query Help
uses the same filter for your loaded custom recipes.

| What you want to find | Example phrases from the packs |
| --- | --- |
| Developer files | `project build definitions`, `git object packs`, `terraform provider cache files`, `large source maps` |
| Server and database storage | `postgresql wal files`, `innodb tablespaces and recovery files`, `mariadb aria tables and logs`, `redis persistence candidates`, `mongodb wiredtiger data files` |
| Office documents | `excel workbooks`, `presentation files`, `large pdf documents`, `recent office documents`, `office lock files` |
| CAD and design | `autocad drawings`, `mechanical cad projects`, `cad exchange files`, `kicad design files`, `blender version backups` |
| Photo and video projects | `adobe photoshop projects`, `premiere auto save projects`, `davinci resolve projects and timelines`, `davinci resolve project archives`, `camera cinema footage` |
| Audio production | `daw sessions`, `ableton live packs and devices`, `vst3 plugin bundles`, `clap plugin files`, `audio plugin presets` |

The packs combine simple extension matches with path, age, and size filters.
Use `--describe` to see each rule and adjust it for your scan. For example,
the Resolve archive recipe finds `.dra` directories, while the project and
timeline recipe finds files. VST3 bundle directories and standalone files
also have separate recipes. Extension matches work on files stored on the
scanned filesystem, regardless of where the authoring app ran.
[DaVinci Resolve reference manual](https://documents.blackmagicdesign.com/UserManuals/DaVinciResolveReferenceManual.pdf),
[VST3 plug-in structure](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical%2BDocumentation/Locations%2BFormat/Plugin%2BFormat.html).

Database and server recipes are for inventory. Configurable data directories,
symlinked tablespaces, and custom backup names can require a different scan
root or expression. A matching name does not establish the file format or
whether it can be removed. Manage live data, WAL, redo logs, and backups
through the service's own maintenance and retention tools.
[PostgreSQL file layout](https://www.postgresql.org/docs/current/storage-file-layout.html),
[MySQL data directory](https://dev.mysql.com/doc/refman/8.4/en/data-directory.html),
[SQLite WAL](https://www.sqlite.org/wal.html).

Start with the pack for the OS running kosha. If you combine both full packs,
trim entries you do not use before adding many of your own. The 256-entry
limit applies to the entire file, including entries for other platforms.

```sh
kosha search --describe --recipes kosha-marketplace/packages/recipes/coolnew/linux-essentials/1.0.0/pack.json "large rotated system logs"
kosha search --path . --recipes kosha-marketplace/packages/recipes/coolnew/linux-essentials/1.0.0/pack.json "davinci resolve project archives"
```

```powershell
kosha search --describe --recipes kosha-marketplace/packages/recipes/coolnew/windows-essentials/1.0.0/pack.json "windows update logs"
```

## Find packs in the public marketplace

```sh
kosha marketplace sync
kosha marketplace search database
kosha marketplace show linux-essentials
kosha marketplace get linux-essentials
```

`sync` refreshes the catalog from the public GitHub repository. `search`
and `show` work from the saved catalog, including offline. Search matches
the pack ID, publisher, title, description, kind and tags. Other-platform or
unsupported entries remain visible with unavailable reasons. `show` also accepts
packs for other platforms. `get` refuses unavailable entries and downloads/validates
the selected pack from the saved catalog revision, then installs it locally.
It refuses to overwrite an installed pack with the same ID. An installed pack
continues to work offline, and a catalog sync leaves installed packs alone.

In Kosha GUI, run **Sync recipe marketplace** from the command palette. Search
for **marketplace recipe packs**, or enter a pack's title or ID. Choose **Get
recipe pack …** to download and install it. The local install and removal
actions also work offline.

## Create a recipe file

### Install a pack you downloaded or wrote

Install a versioned pack from a local JSON file. Use the pack ID shown by
`list` to remove it.

```sh
kosha marketplace install kosha-marketplace/packages/recipes/coolnew/linux-essentials/1.0.0/pack.json
kosha marketplace list
kosha marketplace uninstall linux-essentials
```

In the GUI, open the command palette and choose **Install a recipe pack…**.
Select the JSON file in the picker. To see what is installed, type
`installed recipe packs` in the command palette. Each result shows the pack
title, ID, and version. Choose the pack's **Remove recipe pack** action and
confirm **Remove** to uninstall it. The GUI reloads recipes after its own
installs and removals. Restart an already-open GUI after changing packs through
the CLI. Installed recipes are available offline and appear among custom
recipes in **Examples** and **Query Help**.

Both tools keep installed files under `recipe-packs/` in the shared
configuration directory. Kosha copies the source file during install. A second
install with the same ID fails, and uninstall removes only the managed copy.
Up to 32 installed packs are supported, with per-file and aggregate execution
limits described in the [authoring guide](recipe-authoring.md#limits-and-performance-choices).
GitHub packs can be downloaded separately and installed this way.

### Keep personal recipes

Create `search-recipes.json` beside `settings.json` in the
[configuration directory](settings-and-files.md#application-directories).
On Linux the default is `~/.config/kosha/search-recipes.json`. An absolute
`XDG_CONFIG_HOME` moves it to `$XDG_CONFIG_HOME/kosha/search-recipes.json`.
Create the `kosha` directory if this is your first time using the app.
Use a JSON array containing `phrase` and `dsl` for each recipe:

```json
[
  {
    "phrase": "my build leftovers",
    "dsl": "file and ext in (obj,pdb,ilk)"
  },
  {
    "phrase": "large invoices",
    "dsl": "file and name ~ invoice and apparent > 10mb"
  },
  {
    "phrase": "old downloads",
    "dsl": "file and path ~ Downloads and age > 30d"
  },
  {
    "os": "windows",
    "phrase": "nuget global packages",
    "dsl": "file and path ~ \"/.nuget/packages/\""
  }
]
```

Restart the GUI after editing. Open **More examples**/**Examples** to find
**Your recipes**, or type a phrase directly. The interpretation identifies it
as a **Recipe**. Click **DSL** to inspect or edit the expression actually used.

The CLI reads the same file on each invocation. Use `--recipes FILE` to select
another recipe file for a command, including `--describe`. Explicit selection
bypasses the personal file and installed packs.

```powershell
kosha search --describe --json "my build leftovers"
kosha search --path . --recipes ./team-recipes.json "my build leftovers" --export jsonl
```

## Matching and validation

Exact matching uses the **complete phrase**, ignoring case and repeated whitespace.
`MY BUILD LEFTOVERS` therefore selects the first recipe. Adding `over 1 GB`
does not substitute into it: define another complete recipe or edit its DSL.
A matching custom recipe takes precedence over a built-in Natural phrase.
The personal file wins over installed packs. If installed packs share an exact
phrase, the pack with the first qualified ID in alphabetical order supplies
the query. Conflicting aliases on retained rules are ambiguous. Multiple matching
typed rules, or an exact rule plus a typed match, report an error instead of
choosing a match. Regex case/whitespace rules and sample-phrase collisions are
explained in the [authoring guide](recipe-authoring.md#matching-overlaps-and-platform-rules).
Explicit **DSL** and **Name** modes bypass custom recipes.

The file is validated as a whole. Invalid DSL, unknown JSON fields, and duplicate
phrases after normalization are errors; the app never loads just part of it.
Fix the file and restart the GUI. While it is invalid, Natural searches report
the problem; DSL and Name search remain available. The CLI reports the error
before scanning. An absent default file simply enables built-in search.

Use the optional `os` field to limit an entry to `linux`, `windows`, or
`macos`. Omit it for a recipe available everywhere. A single file can give
the same phrase different DSL on different platforms. A platform-independent
entry and a platform-specific entry with the same phrase conflict.
The whole file is checked, including entries for other platforms.

Personal files can still be plain JSON arrays. Pack files use the same entry
format inside their `recipes` array. Limits are 1 MiB per file, 256 recipes,
256 UTF-8 bytes per phrase, and 16 KiB
per DSL expression. Phrases cannot contain control characters. JSON comments
and trailing commas are not supported.

Pack `schema` versions the file layout. The current pre-release contract keeps
schema `1`, adds optional publisher/capabilities and richer rule types, and
retains the Essentials package versions `1.0.0` by an explicit pre-release
replacement decision. Existing exact arrays and packs remain readable.
After publication, use a distinct package version for changed contents and keep
its publisher/ID stable. Catalog schema is independent: the current schema-1
writer emits a small root with JSONL indexes, while readers also accept the old
inline catalog shape. Engine requirements advertise rule support separately
from schema and package version. Unknown fields remain errors, so misspelled
rules are not silently ignored. See [the full format](marketplace.md).

Relative DSL such as `age > 30d` uses the captured search clock each time the
query runs; `age` refers to modification time. Literal dates and `@seconds`
remain fixed. In particular, copying the translated DSL for “created last
week” saves those exact dates, rather than a phrase that moves to the next week.
Unknown creation times remain unknown and never fall back to modification time.

Recipes select metadata from the loaded index. Typed extraction can recognize
custom query phrasing; it binds existing query fields and does not search file
contents. See the [query reference](query-language.md)
for operators and fields.
