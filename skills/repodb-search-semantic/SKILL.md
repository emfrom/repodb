---
name: repodb-search-semantic
description: Find concepts, code behavior, and documentation guidance by meaning.
---

# RepoDB semantic search (`repodb_search_semantic`)

Use semantic search for conceptual questions about code behavior and documentation, including standards, README content, and `AGENTS.md`, even when the wording is unknown. Run `repodb search --semantic 'QUERY'`. Searches default to scope `all`; pass a selected scope with `--scope`, for example `repodb search --semantic --scope source 'QUERY'`. Valid scopes are `source`, `repo-local-source`, `repo-local-docs`, `all-docs`, and `all`.

- `source` includes source-code from this repository and imported Git repositories.
- `repo-local-source` and `repo-local-docs` are restricted to this repository’s indexed Git history; the former includes source-code and the latter recognized documents.
- `all-docs` includes recognized documents from any origin, including imported documentation. Document recognition uses known path/format metadata; unknown formats are guaranteed only in `all`.
- `all` includes every result, including imported repositories and documentation, and is the default.

Scopes do not include unindexed or uncommitted filesystem content. Use filesystem tools for current files and known-file reading. To map indexed results into staged/live edits, opt in with `repodb search --semantic --worktree 'QUERY'`. Only indexed HEAD text is embedded; output labels its similarity score, matched HEAD lines, mapping state, and any live text shown. Deleted or ambiguous ranges retain the HEAD reference instead of inventing a live location.

If semantic search is unavailable, report that limitation; do not silently fall back to exact search. For weak or empty results, report that and suggest rephrasing the query, explicitly importing/indexing a relevant source if it is missing, or trying combined/exact search when appropriate. Preserve only provenance actually returned (repository, commit/version, path, line/page); disclose missing source content without fetching or reconstructing it. Search does not refresh the index; suggest `repodb refresh` when relevant, but do not refresh automatically.
