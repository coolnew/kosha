# Author and test recipe packs

This guide is for people and coding agents writing personal or marketplace
recipes. Start with a narrow, explainable query and prove its interpretation
before distributing it. Recipes search recorded metadata, not file contents.
See [marketplace formats](marketplace.md) for distribution and trust boundaries,
and [the query reference](query-language.md) for the DSL they produce.

## Choose the simplest rule that expresses the intent

| Need | Use | Avoid |
| --- | --- | --- |
| A fixed query with several familiar names | `exact` plus `aliases` | Several overlapping regexes for the same names |
| A phrase with a size, elapsed age, literal name or path | `template` with typed slots | Capturing an arbitrary DSL fragment |
| A constrained identifier or alternate phrase syntax | `regex` with named, typed captures | An unrestricted `.*` that claims unrelated phrases |
| Suggestions for a misspelled fixed phrase | `fuzzy-suggestions` | Automatically correcting quantities, operators or paths |

Templates and regexes translate whole phrases. They are not a way to teach
arbitrary conversational grammar, compose recipes recursively, or run scripts.
For example, an exact recipe `review notes` does not automatically accept
`review notes over 10 MB`. Define that complete parameterized phrase separately.

## A complete example pack

Save this as `packages/recipes/example/review-tools/1.0.0/pack.json` in a
marketplace checkout. For local experiments, any JSON filename is sufficient.

```json
{
  "schema": 1,
  "kind": "recipe-pack",
  "metadata": {
    "publisher": "example",
    "id": "review-tools",
    "title": "Review tools",
    "description": "Examples of bounded, inspectable search translations",
    "version": "1.0.0",
    "tags": ["documents", "builds"]
  },
  "requires": ["recipe.exact", "recipe.template", "recipe.regex", "recipe.fuzzy-suggestions"],
  "recipes": [
    {
      "phrase": "review notes",
      "aliases": ["check notes"],
      "dsl": "file and ext = md"
    },
    {
      "type": "template",
      "phrase": "invoice files over 10 MB",
      "template": "invoice files over {minimum:size}",
      "dsl": "file and ext = pdf and name ~ invoice and size > {minimum}"
    },
    {
      "type": "template",
      "phrase": "build objects older than 2 days",
      "template": "build objects older than {age:duration}",
      "dsl": "file and ext in (obj,o,pdb) and age > {age}"
    },
    {
      "type": "template",
      "phrase": "files beneath documents",
      "template": "files beneath {folder:path}",
      "dsl": "file and path ~ {folder}"
    },
    {
      "type": "regex",
      "phrase": "tickets for KOSHA",
      "pattern": "tickets for (?P<project>[A-Za-z][A-Za-z0-9_-]{0,31})",
      "bindings": {"project": "text"},
      "dsl": "file and name ~ {project}"
    },
    {
      "type": "fuzzy-suggestions",
      "phrase": "database reports",
      "aliases": ["db reports"],
      "dsl": "file and name ~ database"
    }
  ]
}
```

The `example` publisher is illustrative, not a verified identity. Choose your
own stable publisher and ID. For generated packages, use lowercase ASCII
letters/digits/hyphens, starting with a letter or digit, up to 80 bytes each.
Versions are one ASCII path component, up to 64 bytes: start with a letter or
digit and continue with letters, digits, `.`, `+` or `-`. Paths, artifact metadata
and manifest metadata must agree exactly. Unknown JSON fields are errors;
comments and trailing commas are not accepted.

Metadata titles are at most 120 UTF-8 bytes, descriptions 1,024 bytes, and tags
32 entries of at most 64 bytes each. These values must be nonempty and contain
no control characters. Keep descriptions factual; do not claim that a matching
extension or path proves a file's contents, ownership or safe removal.

Declare every capability used anywhere in the pack, including rules for another
OS. Keep `requires` identical in the artifact and author manifest. Unknown
capabilities prevent installation; a declaration does not enable an unsupported
engine feature. A personal JSON array can use the same rule objects without a
metadata wrapper; explicit CLI `--recipes FILE` bypasses default installed packs.

## Typed slots are values, not query fragments

| Type | Compatible DSL fields | Nuance |
| --- | --- | --- |
| `size` | `size`, `apparent`, `wasted` | Produces integer bytes; supported units use powers of 1024, including `MB` |
| `duration` | `age` | Elapsed time since modification, not creation time or a calendar-date range |
| `text` | `name`, `ext`, `owner` | Literal text, not a regex, glob or list of extensions |
| `path` | `path` | Literal path text; the DSL operator determines contains/prefix/equality semantics |

Slot names start with a lowercase ASCII letter, continue with lowercase letters,
digits or underscores, and have at most 48 bytes. A template declares types in
`{minimum:size}`; its DSL uses `{minimum}` as a complete, unquoted value. The
same slot may be referenced more than once, within the output budget. It cannot
stand for a field name, operator, boolean clause or arbitrary list.

For example, `name ~ "prefix-{label}"` is not supported: this is string building
inside a DSL literal. Capture the complete wanted value, or write separate
literal predicates whose combined meaning you have tested. Every declared slot
must be used in a compatible field. You cannot bind a size slot to `items`, or
a duration slot to `created`.

Text/path values are quoted automatically. Surrounding input quotes can keep a
space-containing value readable, as in `files beneath 'build outputs'`. Empty
quoted values fail. Values containing both single and double quotes cannot be
represented by the current editable DSL and fail explicitly. Do not invent
backslash escaping to work around this limitation. In JSON source, backslashes
still need normal JSON escaping, such as `C:\\Example\\Reports`.

The example's `path ~ {folder}` means a literal substring of the full path.
`documents-old` and even a filename containing `documents` may therefore match.
It is not a directory-membership test. For a narrower expression, consider
`path startswith {folder}` with a complete directory prefix and its trailing
separator, and test both platforms' path conventions. Paths and environment
variables are not expanded by recipes. Include `file` or `dir` when that matters;
directory size includes descendants. Use `apparent` if logical length is intended
instead of allocated disk bytes.

Dates/categories are available through the built-in language and fixed DSL;
they are not custom slot types. Copying the DSL produced by `created last week`
freezes those date boundaries. `age > 2d` remains relative to each query's clock.
Unknown creation timestamps stay unknown, including under negation. Describe
tests prove translation; they do not prove that a scan recorded those fields.

## Matching, overlaps and platform rules

Exact phrases/aliases normalize case and repeated whitespace. Template literal
words ignore case, accept flexible whitespace and allow surrounding whitespace.
Regexes keep the author's case/whitespace rules; use `(?i)` when desired. Kosha
anchors regexes to the complete input, so no extra `^`/`$` is necessary.

Templates need at least one slot. Separate adjacent slots with a literal word
or punctuation; whitespace alone does not identify where one value ends and
the next begins. For example, prefer `between {low:size} and {high:size}` over
`{low:size} {high:size}`. Delimiters may also occur inside captured text: choose
clear wording and test those cases. The matcher does not enumerate alternative
slot splits looking for one whose types happen to validate.

Regexes declare every named capture in `bindings`; names/types follow the same
rules as templates. Use `(?:...)` for grouping that does not extract an entity.
All named captures must be present and valid in the sample phrase. A runtime
match with a missing/invalid capture reports an error. Look-around and
backreferences are unavailable. Regexes may extract from query text only; they
do not provide filename-regex or file-content search. Exact/fuzzy rules cannot
declare `pattern`, `template` or bindings. Template/regex rules do not accept
aliases: write distinct, nonoverlapping phrases or one constrained regex.

The `phrase` on a parameterized rule is a working example shown in the picker;
it is also the rule's primary identity when combining sources. Use distinctive
sample phrases, not placeholders or vague labels such as `find files`.

- Within one file, normalized phrases/aliases must not collide on overlapping
  platforms. `os` can be `linux`, `windows` or `macos`; omit it for all OSes.
  Separate OS-specific definitions can share a phrase; a universal definition
  conflicts with each of them. The whole file validates, even inactive rules.
- Across sources, personal recipes load first, then installed packs by qualified
  ID. An identical primary phrase suppresses the entire later rule, including
  a parameterized rule whose sample phrase collides. Publisher IDs do not
  namespace the actual phrases users type.
- Conflicting aliases on retained rules are ambiguous. So are multiple matching
  patterns, or an exact match plus a matching pattern. A typed match counts even
  if its extracted value is invalid. Reordering patterns does not resolve this.
- A pattern that does not match falls through to built-ins/general language,
  which can interpret the input as a literal filename search. A regex nonmatch
  is not automatically a CLI error. Test the fallback as well as the recipe.

The GUI's DSL/Name modes bypass recipes. The CLI can run explicit DSL through
`kosha PATH --query "QUERY"`. These escape hatches help diagnose an override.

## Fuzzy suggestions have intentionally narrow scope

An exact `fuzzy-suggestions` phrase or alias executes its declared DSL. Typos
can suggest up to five primary phrases, ordered by edit distance then phrase.
They never execute a corrected query. Suggestions apply after normal translation
fails or reaches an unquoted literal-name fallback; they do not replace a valid
semantic interpretation. Explicit quoted literals retain literal-name fallback.

Matching uses at most two Unicode-character edits and the same number of words.
Eligible normalized phrases are 4..256 UTF-8 bytes. Numeric wording, digits,
operators, quoted/path-like text and common predicate words are excluded from
correction. A long alias or one containing `in`, `older`, `name` or `path` may
still work exactly while being ineligible for typo suggestions. This is not a
spellchecker for the entire query language or an entity-extraction mechanism.

## Limits and performance choices

| Scope | Limit |
| --- | --- |
| JSON file | 1 MiB; 256 rules including inactive OS rules |
| Phrase or alias | 1..256 UTF-8 bytes, no control characters; at most 32 aliases per rule |
| DSL source | 1..16,384 bytes |
| DSL structure | Nesting 64; at most 1,024 boolean joins; use `in (...)` for lists of alternatives |
| Template | 1..1,024 bytes after outer trimming |
| Regex | 4,096-byte pattern; nesting 32; 256 KiB compiled-size and DFA-cache limits |
| Captures | 16 total groups per regex, including unnamed groups |
| Patterns per file | 64 template/regex rules combined |
| Query text | 16,384-byte recipe-matching ceiling |
| Bound output | Conservative 16,384-byte rendering budget, checked before repeating values |
| Installed store | 32 packs |
| All active sources | 8,192 rules; 128 patterns; 32,768 phrases/aliases; 1,024 fuzzy phrases/aliases |

UTF-8 bytes are not character counts. Declared fuzzy aliases count toward the
budget even when safety rules exclude them from suggestions. Rules suppressed
by an earlier identical primary phrase do not consume additional active budget.
Installation checks the prospective load order before saving. Exceeding a limit
fails; it does not silently discard the last rules or truncate a query.

A short recipe can repeat one large capture many times and exceed the bound
output budget. Simplify repeated predicates rather than splitting packs to
evade limits: aggregate budgets apply across packs. The conservative estimate
can reject output slightly below 16 KiB. Pattern-size limits are independent;
a short regex with large repetitions/Unicode classes can still fail compilation.

Prefer exact aliases and narrow literal prefixes before regex captures. Put
`file`/`dir` and intended extension constraints in the resulting query. Avoid
`.*` catch-all patterns and overlapping intent families. Translation occurs
once per query, not once per indexed file, but a broad path/name query can still
scan the full arena. Neither the format nor its limits promise fixed latency on
every device. A fast interpretation does not prove the resulting search is fast.

## Add the author manifest and executable examples

Save this beside the example `pack.json` as `manifest.json`:

```json
{
  "schema": 1,
  "kind": "recipe-pack",
  "publisher": "example",
  "metadata": {
    "publisher": "example",
    "id": "review-tools",
    "title": "Review tools",
    "description": "Examples of bounded, inspectable search translations",
    "version": "1.0.0",
    "tags": ["documents", "builds"]
  },
  "platforms": [],
  "requires": ["recipe.exact", "recipe.template", "recipe.regex", "recipe.fuzzy-suggestions"],
  "path": "packages/recipes/example/review-tools/1.0.0/pack.json",
  "cases": "packages/recipes/example/review-tools/1.0.0/cases.jsonl"
}
```

Empty `platforms` means all platforms. Otherwise list supported OSes. Per-rule
`os` controls active rules; manifest `platforms` controls catalog availability.
Keep them consistent and include a usable recipe for each advertised platform.
Do not edit generated digests or counts. The generator derives them from actual
artifact bytes. `listed:false` retains an older folder without advertising it;
only one version per publisher/ID can be listed. After publication, changed
contents need a new package version; follow the repository's release policy.

Save the following as `cases.jsonl`, one JSON object per line, without a header:

```jsonl
{"input":"check notes","dsl":"file and ext = md"}
{"input":"  CHECK   NOTES  ","dsl":"file and ext = md"}
{"input":"invoice files over 20 MB","dsl":"file and ext = \"pdf\" and name ~ \"invoice\" and size > 20971520b"}
{"input":"build objects older than 2 days","dsl":"file and ext in (\"obj\", \"o\", \"pdb\") and age > 172800s"}
{"input":"files beneath 'build outputs'","dsl":"file and path ~ \"build outputs\""}
{"input":"tickets for ABC-123","dsl":"file and name ~ \"ABC-123\""}
{"input":"invoice files over nonsense","error":true}
{"input":"invoice files over 10 MB or all files","error":true}
{"input":"files beneath ''","error":true}
{"input":"databse reports","error":true}
{"input":"tickets for ABC; execute","dsl":"name ~ \"tickets for ABC; execute\""}
```

These cases assert exact rendered DSL, including automatically added quotes.
The last one deliberately proves a safe literal fallback, not a successful
ticket extraction. `error:true` requires exit 1; it does not assert a particular
diagnostic or prove that an ambiguity/fuzzy error was the intended cause. Inspect
that diagnostic separately. Add `os` to OS-specific cases; omitted cases run on
every host. A package corpus is at most 256 KiB and 1,024 lines. JSONL records
accept `input`, exactly one of `dsl`/`error:true`, and optional `os`.

## Validate in layers before sharing

1. Use a current Kosha executable and describe each intent without scanning:

   ```sh
   kosha search --describe --json --recipes ./packages/recipes/example/review-tools/1.0.0/pack.json "invoice files over 20 MB"
   ```

   Check the DSL, Recipe entity and captured entities/byte spans. Without the
   Recipe entity, a successful translation may only be a built-in/literal
   fallback. Verify the quantity, operator and file/folder scope match the words.

2. In a standalone marketplace checkout containing `xtask-marketplace.py`, run:

   ```sh
   python xtask-marketplace.py --root . --cli /path/to/kosha
   python xtask-marketplace.py --root . --check --cli /path/to/kosha
   ```

   The first command generates indexes; the second verifies they are fresh.
   From the application's Git checkout pass `--root ../kosha-marketplace` (or
   the actual path to your standalone marketplace checkout). Kosha does not
   carry production packs or a second catalog. Include
   `--cli`: without it, generation validates structure/metadata and corpus shape,
   but does not execute recipe translations or expected DSL checks. Neither
   mode publishes, commits, bumps versions or scans user files.

3. Run real searches on a small synthetic fixture using `--path` and the same
   `--recipes` file. Include intended hits, near misses, misleading path/name
   substrings, empty results and missing timestamp coverage. Confirm exact
   matches and totals; `--describe` alone cannot verify file selection.
4. Test malformed quantities, optional/missing captures, whitespace/case,
   Unicode, quotes, both path separators where relevant, extra trailing clauses,
   neighboring rule intents and the longest values you intend to support.
   Install into an isolated test profile with other likely packs to catch
   cross-pack collisions and combined limits; `--recipes` tests one file only.
5. Validate OS-specific cases on those OSes. Review public metadata/examples for
   personal paths, credentials and private names. Use synthetic identifiers.
   Review the complete generated diff, then follow the marketplace's publication
   process. A hash is byte verification, not an author signature or safety proof.

If installation is rejected, fix the underlying rule/capability/budget problem
and regenerate. Syncing does not update installed packs; installation does not
overwrite an existing identity. Preserve personal recipes before removing a
managed old version. A corrupt/unsupported managed pack can still be removed
with `kosha marketplace uninstall PUBLISHER/ID`. See
[stored files](settings-and-files.md) for configuration locations.

Maintainers of the application source can validate this page's complete worked
example directly with the tracked helper:

```sh
python scripts/check-recipe-authoring-manual.py --cli /path/to/kosha
```

It extracts the pack, manifest and corpus above into temporary fixtures,
generates/checks indexes, validates expected DSL and UTF-8 entity spans, and
confirms describe opens no source. It does not build, install, scan, publish or
modify personal recipes. The helper lives in Git under `scripts/`; only its
generated fixtures belong under ignored `target/`.
