# Query language reference

Use **DSL** in the desktop search bar, `kosha PATH --query "QUERY"`, or the
`dsl` value in a [custom recipe](search-recipes.md). Queries search recorded
metadata, not file contents. The scan root itself is excluded from results.

```text
file and size > 100mb
ext in (pdf, xlsx) and apparent > 10mb
dir and items > 10000
name startswith report and name endswith .pdf
created >= 2026-01-01 and created < 2027-01-01
```

## Syntax

### Fields

| Field | Meaning |
| --- | --- |
| `name` | Entry name |
| `path` | Full path |
| `ext` | Extension without a dot |
| `size` | Bytes on disk |
| `apparent` | Logical file length |
| `items` | Files anywhere below a directory |
| `children` | Direct children |
| `depth` | Levels below the scan root |
| `age` | Time since modification |
| `modified` | Modification date |
| `created` | Creation date, when recorded |
| `wasted` | On-disk bytes beyond logical length |
| `owner` | Recorded account name, identifier, or `me` |

### Flags

`dir`, `file`, `link`, `empty`, `sparse`, `partial`, `hardlink`, `unreadable`.

### Operators

Compare with `>`, `>=`, `<`, `<=`, `=`, or `!=`. Combine conditions with `and`,
`or`, and `not` (also `&&`, `||`, and `!`). Parentheses group conditions.
Text fields support `~` (contains), `!~`, `startswith`, and `endswith`.
These are case-insensitive literal comparisons, not regular expressions or globs.
`in (pdf, xlsx)` selects from a list. Quote text containing spaces or operators.
Quote the complete query when passing it through a shell.

### Literals

- Sizes: `100`, `10k`, `1.5mb`, `2gb`, `500mib`. Bare numbers are bytes;
  size units use powers of 1024. A space before the unit is accepted.
- Ages: `30d`, `12h`, `1w`, `6mo`, `2y`; a bare age number means days.
- Dates: `2026-01-01` means the beginning of that day in the local timezone.
- Exact instants: `@1795003200` means Unix seconds, independent of timezone.
- Text: `"budget report.xlsx"` or an unquoted simple word such as `invoice`.

Unknown timestamps never become modification or creation dates and do not
satisfy date predicates, including through negation. Creation data availability
depends on scan method, filesystem, and snapshot version. Relative `age` queries
use the captured search clock; exact instants and date literals remain fixed.

Directory sizes include their contents: adding every directory and file row
double-counts bytes. A file can have a nonzero logical size and zero on-disk
allocation. `partial` and `unreadable` indicate recorded coverage limitations;
inspect the scan's incomplete/partial status before relying on an empty result.

Use the **?** help button or `kosha --query-help` for additional examples and
`kosha search --describe --json "PHRASE"` to inspect natural-language translation.
