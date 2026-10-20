---
name: repodb-search-semantic
description: Find concepts, code behavior, and documentation guidance by meaning.
---

# RepoDB semantic search (`repodb_search_semantic`)

Use semantic search for conceptual questions about code behavior and documentation, including standards, README content, and `AGENTS.md`, even when the wording is unknown. Run `repodb search --semantic 'QUERY'`. Searches cover all indexed content by default, including repository source, imported repositories, and imported documentation; they do not cover unindexed or uncommitted filesystem content. Use filesystem tools for current files and known-file reading.

If semantic search is unavailable, report that limitation; do not silently fall back to exact search. For weak or empty results, report that and suggest rephrasing the query, explicitly importing/indexing a relevant source if it is missing, or trying combined/exact search when appropriate. Preserve only provenance actually returned (repository, commit/version, path, line/page); disclose missing source content without fetching or reconstructing it. Search does not refresh the index; suggest `repodb refresh` when relevant, but do not refresh automatically.
