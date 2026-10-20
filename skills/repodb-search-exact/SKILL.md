---
name: repodb-search-exact
description: Find literal identifiers and phrases in indexed RepoDB content.
---

# RepoDB exact search (`repodb_search_exact`)

Use exact search for known identifiers, symbols, or literal phrases. Run `repodb search 'QUERY'`. Searches cover all indexed sources by default, including repository history and imported documentation; they do not cover unindexed or uncommitted filesystem content. Use filesystem tools to inspect current files or read a known file.

Preserve repository, commit/version, path, and line/page references actually returned; never invent provenance. If source content is missing, report the available reference and disclose that limitation without fetching or reconstructing it. For weak or empty results, report that and suggest rephrasing or semantic/combined search. Search does not refresh the index; suggest `repodb refresh` when relevant, but do not refresh automatically.
