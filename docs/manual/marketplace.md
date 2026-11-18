# Marketplace formats and extended search recipes

The marketplace supplies optional declarative search packs. Sync/discovery and
downloads are explicit; loaded recipes work offline and do not execute scripts.
Plugins and other future package kinds may appear as **unavailable** metadata.
Kosha currently installs recipe packs only.

Authors should follow [Author and test recipe packs](recipe-authoring.md) for a
complete package, manifest and JSONL corpus, plus matching pitfalls, field/type
compatibility, exact limits and a validation workflow. That guide applies to
personal recipes and to contributions in a standalone marketplace checkout.

## Discover, install and remove

```sh
kosha marketplace sync
kosha marketplace search database
kosha marketplace show coolnew/windows-essentials
kosha marketplace get coolnew/windows-essentials
kosha marketplace list
kosha marketplace uninstall coolnew/windows-essentials
```

Use the qualified ID printed by discovery/listing. A short ID works when it
identifies one package. An exact legacy publisher-less ID identifies that old
pack; conflicting aliases between namespaced packs report an error.
Desktop users can use the command palette to sync, find compatible download
actions, or manage installed packs. Unsupported entries show why they are
unavailable. Another platform's metadata can be inspected without installing it.

New installed copies live under configuration
`recipe-packs/PUBLISHER/pack-ID.json`; older `recipe-packs/pack-ID.json` files
still load. Your personal `search-recipes.json` remains independent. Syncing
catalog metadata does not change installed packs or automatically update them.

## Catalog and package layout

The root `catalog.json` references hashed JSONL indexes. Each record describes
one package kind, publisher, metadata, supported platforms, engine requirements,
artifact path, exact byte length and SHA-256. Artifacts use folders such as:

```text
packages/recipes/coolnew/windows-essentials/1.0.0/pack.json
```

One sync pins the root, indexes and subsequent artifact URLs to the same Git
commit. All shard hashes, counts and lengths are validated before replacing the
saved catalog. Errors preserve the previous saved catalog. Recipe installation
also validates the artifact hash, size, metadata, schema and required features.

Catalog schema, pack schema, package version and engine capabilities are
independent. This pre-release format replacement retains schema 1 and existing
package versions 1.0.0. After publication, changing a package's contents should
use a new package version; adding a package does not require an app version bump.
Legacy inline catalogs and personal recipe arrays remain readable.

The maintained seed is only `marketplace/`; there is no top-level `recipes/`
mirror. Its small `catalog.json` is an index manifest, not a flat package list.
Growing catalog records live in `indexes/*.jsonl`. Individual versioned
`manifest.json`/`pack.json` documents remain bounded JSON objects. JSONL is for
the catalog shards and test cases, not a requirement to stream every small file.

## Rule forms

The `recipes` array accepts the following forms. All match complete phrases.
Exact aliases and template literal words are case-insensitive; regexes retain
the author's case rules (use `(?i)` to opt in). Packs validate as a whole.

| `type` | Fields | Behavior |
| --- | --- | --- |
| `exact` (default) | `phrase`, `dsl`, optional `aliases` | Existing exact mapping and alternate names |
| `template` | sample `phrase`, `template`, parameterized `dsl` | Extract typed values from named slots |
| `regex` | sample `phrase`, `pattern`, `bindings`, parameterized `dsl` | Extract explicitly typed named captures |
| `fuzzy-suggestions` | `phrase`, `dsl`, optional `aliases` | Exact names execute; typos only suggest |

Each parameterized rule needs a working sample `phrase`. The example picker
shows that phrase and its resolved sample DSL. A versioned pack can declare
`requires`: `recipe.exact`, `recipe.template`, `recipe.regex`, and
`recipe.fuzzy-suggestions`. Declare every non-exact rule capability you use.
Unsupported required features fail explicitly instead of dropping rules.

```json
{
  "schema": 1,
  "kind": "recipe-pack",
  "metadata": {
    "publisher": "example",
    "id": "review-tools",
    "title": "Review tools",
    "description": "Parameterized document review",
    "version": "1.0.0"
  },
  "requires": ["recipe.exact", "recipe.template", "recipe.regex"],
  "recipes": [
    {"phrase": "review notes", "aliases": ["check notes"], "dsl": "file and ext = md"},
    {
      "type": "template",
      "phrase": "documents larger than 10 MB",
      "template": "documents larger than {minimum:size}",
      "dsl": "file and ext in (pdf,docx,xlsx) and size > {minimum}"
    },
    {
      "type": "regex",
      "phrase": "tickets for KOSHA",
      "pattern": "tickets for (?P<project>[A-Za-z][A-Za-z0-9_-]{0,31})",
      "bindings": {"project": "text"},
      "dsl": "file and name ~ {project}"
    }
  ]
}
```

## Typed extraction and interpretation

Supported slot/capture types are `size`, `duration`, `text`, and `path`.
For example, `{minimum:size}` recognizes supported size wording and produces an
integer byte quantity; `{age:duration}` supplies an elapsed age condition.
Text slots bind name/extension/owner values; path slots bind path values.
Dates and categories continue to use the built-in translator; they are not
currently extra custom binding types.

DSL placeholders must be complete **unquoted values**, such as `size > {minimum}`
or `path ~ {folder}`. The engine parses the query structure and binds typed AST
values. Captured text cannot add query operators. Literal text/path quoting is
generated for editable DSL; unrepresentable input reports an error.
Bound queries have a conservative 16 KiB output budget, checked before repeating
captured values. Oversized expansion fails rather than truncating the query.
Interpretations expose the recipe plus extracted values and their UTF-8 byte
spans. Unknown timestamps retain the normal search semantics.

Exact overrides preserve personal-before-installed precedence. Distinct typed
rules that both match produce an ambiguity error, rather than selecting an
arbitrary interpretation. DSL and Name modes bypass recipe matching entirely.
Typo suggestions appear after ordinary interpretation fails or reaches an
unquoted literal-name fallback, and never silently
modify a size, operator, path or query. Type the intended complete phrase, or
choose Name mode to search the original typo literally. Explicit quoted literals
retain their literal meaning.

Regex patterns are compiled when loading a pack, use Rust's bounded regex
engine, and match the whole input. They are limited to 4 KiB patterns, 32-level
nesting and bounded compiled automata. Each rule has at most 16 capture groups
in total, including unnamed groups; use `(?:...)` for grouping without capture.
Named captures must have declared types;
one document may contain at most 64 template/regex rules combined, within its
256-rule total. Exact rules retain the existing 256-rule limit.
Look-around and backreferences are unsupported. Regex extraction applies to the
search phrase; it does not scan file contents or provide a new filename regex
operator. Existing file-name and path query operators retain their meaning.

## Performance and trust boundaries

The active book (personal recipes plus installed packs) permits at most 8,192
rules, 128 template/regex rules, 32,768 total phrases/aliases and 1,024 fuzzy
phrases/aliases. Rules suppressed by earlier identical primary phrases do not
consume additional active budget. Installation checks the prospective book in
its actual load order before saving a pack; over-budget additions fail without
changing existing packs. A manually edited file can still exceed these limits;
the error identifies the problem rather than silently ignoring rules.

Exact phrases use a lookup index. Typed patterns compile once per load and run
only against query text, never against each indexed file. Fuzzy candidates are
prepared at load time; bounded Unicode edit distance suggests at most five
phrases and cannot execute a correction. Complex resulting DSL queries still
have their normal arena-search cost. Recipe counts and regex limits bound work,
but do not promise a fixed latency for every permitted pattern or device.

Hashes prove that bytes match a pinned catalog, not that an author or recipe is
trustworthy. Publisher names are catalog identities, not verified signatures.
Install reviewed packs and inspect their interpretation. Recipes can select a
broad set of files or override natural phrases, but cannot execute code, read
file contents, make network calls or grant write access. Explicit DSL/Name mode
remains available. Recipe removal also works if the stored JSON is corrupt or
no longer supported: `kosha marketplace uninstall PUBLISHER/ID` removes only the
managed regular file with that identity, leaving personal recipes independent.

Package versions are single directory components: an ASCII letter/digit followed
by letters, digits, `.`, `+` or `-`, up to 64 bytes. For example,
`1.0.0+build.7` is supported; slashes and encoded separators are rejected.
Required capabilities cover the entire artifact, including other OS rules.

The format can grow through new kinds and capability names. That does not make
future execution features automatically safe or supported: new binding types,
rule operators or runtimes require engine support and review. Unknown fields
and unsupported execution capabilities fail explicitly. Native/WASI plugins,
arbitrary scripts, recursive recipe expansion and file-content extraction are
not enabled by adding catalog metadata.

## Author and validate packages

The public Git source includes a `marketplace/` seed and `xtask-marketplace.py`.
Maintain per-package manifests and artifacts, then generate/check the indexes:

```sh
python xtask-marketplace.py --root marketplace
python xtask-marketplace.py --root marketplace --check --cli /path/to/kosha
```

In a standalone marketplace checkout use `--root .` instead. The generator writes
only the indexed layout; it does not recreate retired flat compatibility files.
The tool never bumps versions, performs Git operations, publishes, or deletes
old indexes. Review and remove retired indexes deliberately. `listed:false` in
a manifest retains an older package folder without listing it; only one version
per publisher/ID may be listed at a time.

An optional manifest `cases` path refers to a package-local UTF-8 JSONL corpus.
Each line has `input` and either an exact expected `dsl` or `error:true`, with
optional `os`. `--cli` checks applicable cases via describe without scanning.
Include ordinary examples, boundary values, malformed captures, injection-like
text, ambiguous rules and typos. The Search Power Tools seed includes examples.
Optional seed data is available from Git/marketplace, not bundled in application
release archives. See [custom recipes](search-recipes.md) and
[stored files](settings-and-files.md).
