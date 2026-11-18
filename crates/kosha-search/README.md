# kosha-search

Deterministic natural-language translation and cancellable indexed search over
kosha's immutable file metadata. No model, network, or filesystem reads run in
the translator. The caller supplies the clock and local calendar boundaries.

Supported searches include sizes, file categories/extensions, paths and names,
creation/modification dates, and folder counts. Explicit DSL and literal-name
modes bypass natural-language translation. User recipes map exact normalized
phrases to validated DSL. Invalid or unresolved input does not execute a weaker
query.

See the [search guide](../../docs/manual/results-and-search.md),
[recipe guide](../../docs/manual/search-recipes.md), and
[query reference](../../docs/manual/query-language.md). Tests include an
extensible JSONL corpus and independent metadata fixtures.
