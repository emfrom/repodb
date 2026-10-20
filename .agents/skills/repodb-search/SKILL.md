---
name: repodb-search
description: Search indexed RepoDB content with combined exact and semantic matching.
---

# RepoDB combined search (`repodb_search`)

Use combined search when a literal phrase and a concept may both find the answer. Searches cover all indexed sources by default, including repository history and imported documentation; they do not cover unindexed or uncommitted filesystem content. Use filesystem tools to inspect current files or read a known file.

Run `repodb search --combined 'QUERY'`. Preserve repository, commit/version, path, and line/page references actually returned; never invent provenance. If source content is missing, report the available reference and that limitation without fetching or reconstructing it.

If results are weak or empty, report that and try rephrasing or another search mode. If semantic capability is unavailable, state that limitation and use exact search only if appropriate, clearly labeling it as exact (never as a semantic fallback). Search does not refresh the index; mention possible staleness and suggest `repodb refresh` when relevant, but do not refresh automatically.
