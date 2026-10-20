---
name: repodb-search
description: Search indexed RepoDB content with combined exact and semantic matching.
---

# RepoDB combined search (`repodb_search`)

Use combined search when a literal phrase and a concept may both find the answer. Use exact search for known literal identifiers, symbols, and phrases (`repodb search 'QUERY'`), and semantic search for conceptual questions about code behavior and documentation (`repodb search --semantic 'QUERY'`). Choose combined search when you are unsure whether literal or conceptual matching will find it; it runs both forms of discovery (`repodb search --combined 'QUERY'`).

Searches cover all indexed sources by default, including repository history and imported documentation; they do not cover unindexed or uncommitted filesystem content. RepoDB search complements `rg`/`git grep` for indexed content but does not replace filesystem inspection. Use filesystem tools to inspect current files or read a known file directly.

Preserve repository, commit/version, path, and line/page references actually returned; never invent provenance. If source content is missing, report the available reference and that limitation without fetching or reconstructing it. If results are weak or empty, report that and try rephrasing, checking the index/scope, or choosing exact or semantic search when one is a better fit. If semantic capability is unavailable, state that limitation; do not present exact results as semantic or combined coverage. Use exact search separately only if appropriate, clearly labeling it as exact.

Search does not refresh the index; mention possible staleness and suggest `repodb refresh` when relevant, but do not refresh automatically.
