---
name: repodb-history
description: Search selected committed history and enumerate all eligible references with RepoDB explore.
---

# RepoDB history and explore

Use `repodb search QUERY --history=...` when the question depends on earlier commits, a file's history, commit rationale, or duplicate occurrences. The default is `--history=off` (checked-out HEAD); bare `--history` means `all`. Select `--history=all`, a positive topological commit count, or a repository-relative file/directory path. `--commit ID` changes the traversal start; include/exclude filters take exact commit IDs. Local commit selectors do not filter imported sources.

Ordinary search may collapse duplicate occurrences. Add `--explore` to enumerate every eligible occurrence in the selected view as reference-only results. To cover all captured commits, combine it with `--history=all`. Explore defaults to 50 references and a 5,000-byte serialized response; tune `--limit` and `--byte-budget`, then continue with the returned `--cursor`. Keep the same query and selection for the cursor snapshot. Explore omits indexed and live text; use ordinary search and `--fetch-reference ID` when text is required.

Preserve returned commit, path, line/page, and source references. Use Git or filesystem inspection when the requested commit/object is unavailable, content is unindexed, or the task requires uncommitted working-tree state. Search does not refresh or fetch.
