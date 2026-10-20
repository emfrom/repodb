---
name: repodb-search-exact
description: Find literal identifiers and phrases in indexed RepoDB content.
---

# RepoDB exact search (`repodb_search_exact`)

Use exact search for known literal identifiers, symbols, and phrases. Run `repodb search 'QUERY'`; this is literal matching, not a regular-expression search. Searches default to scope `all`; pass a selected scope with `--scope`, for example `repodb search --scope source 'QUERY'`. Valid scopes are `source`, `repo-local-source`, `repo-local-docs`, `all-docs`, and `all`.

- `source` includes source-code from this repository and imported Git repositories.
- `repo-local-source` and `repo-local-docs` are restricted to this repository’s indexed Git history; the former includes source-code and the latter recognized documents.
- `all-docs` includes recognized documents from any origin, including imported documentation. Document recognition uses known path/format metadata; unknown formats are guaranteed only in `all`.
- `all` includes every result, including imported repositories and documentation, and is the default.

By default, scopes search indexed content only. For explicit live exact search, run `repodb search --worktree 'QUERY'`. This inspects staged/live changes and Git-visible untracked supported text relative to the active indexed HEAD, labels layer provenance, and reports coverage. Ignored, unsupported, and binary files are excluded. It is best-effort, does not refresh or persist live text, and errors when HEAD is stale/unindexed or an alternate history selector is used. For semantic mapping, use `repodb search --semantic --worktree 'QUERY'`: only indexed HEAD embeddings rank results; mapped live text is labeled separately from the matched HEAD lines and score basis. `--current-worktree` remains HEAD-only.

Preserve repository, commit/version, path, and line/page references actually returned; never invent provenance. If an indexed Git blob is missing, exact search fails with an error that includes the available blob ID; it does not return a partial match or a path/line reference for that missing content. Report the missing-object error and blob ID, without fetching or reconstructing the content. Ordinary output is limited to 5 results and 5,000 response bytes by default; use `--limit`, `--byte-budget`, `--cursor`, or `--fetch-reference ID` for bounded continuation/retrieval. Use `--explore` to page all eligible occurrences without text. For weak or empty results, report that and suggest checking the query and scope, rephrasing, or trying semantic/combined search. Search does not refresh the index; suggest `repodb refresh` when relevant, but do not refresh automatically.

Prefer filesystem tools to inspect the current working tree or when you already know which file to read. Open a known file directly rather than using RepoDB as a substitute for reading it.
