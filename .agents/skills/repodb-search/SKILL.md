---
name: repodb-search
description: Search indexed RepoDB content with combined exact and semantic matching.
---

# RepoDB combined search (`repodb_search`)

Use combined search when a literal phrase and a concept may both find the answer. Use exact search for known literal identifiers, symbols, and phrases (`repodb search 'QUERY'`), and semantic search for conceptual questions about code behavior and documentation (`repodb search --semantic 'QUERY'`). Choose combined search when you are unsure whether literal or conceptual matching will find it; it runs both forms of discovery (`repodb search --combined 'QUERY'`).

Searches default to scope `all`; pass a selected scope with `--scope`, for example `repodb search --combined --scope source 'QUERY'`. Valid scopes are `source`, `repo-local-source`, `repo-local-docs`, `all-docs`, and `all`.

- `source` includes source-code from this repository and imported Git repositories.
- `repo-local-source` and `repo-local-docs` are restricted to this repository’s indexed Git history; the former includes source-code and the latter recognized documents.
- `all-docs` includes recognized documents from any origin, including imported documentation. Document recognition uses known path/format metadata; unknown formats are guaranteed only in `all`.
- `all` includes every result, including imported repositories and documentation, and is the default.

Scopes do not include unindexed or uncommitted filesystem content. RepoDB search complements `rg`/`git grep` for indexed content but does not replace filesystem inspection. Use filesystem tools to inspect current files or read a known file directly.

Preserve repository, commit/version, path, and line/page references actually returned; never invent provenance. If source content is missing, report the available reference and that limitation without fetching or reconstructing it. If results are weak or empty, report that and try rephrasing, checking the index/scope, or choosing exact or semantic search when one is a better fit. If semantic capability is unavailable, state that limitation; do not present exact results as semantic or combined coverage. Use exact search separately only if appropriate, clearly labeling it as exact.

Ordinary search returns at most 5 results within the default 5,000-byte response budget. Use `--explore` for reference-only occurrences (default 50), `--limit N` and `--byte-budget N` to tune a page, and `--cursor CURSOR` to continue the same snapshot. An oversized result is returned as a reference with `text_omitted_for_size`; use its `reference_id` with `--fetch-reference ID` and a larger budget. For staged/live content, route to the worktree-search skill. Search does not refresh the index; mention possible staleness and suggest `repodb refresh` when relevant, but do not refresh automatically.
