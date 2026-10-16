# RepoDB

A repository search context for finding source and documentation by exact text or semantic similarity, with references precise enough to locate the matched content.

## Searchable content

**Indexed unit**:
A paragraph or syntactic source construct treated as one searchable piece of content.
_Avoid_: chunk (when referring to the domain concept)

**Source reference**:
The identity and location of content in its source, such as a Git commit, file path, and line range, or an imported document and page number.
_Avoid_: citation (when referring to the stored source identity and location)

**Imported source**:
A document or other supported source added to RepoDB from outside the repository's Git history, such as a local file or URL.
_Avoid_: external file (when the source may be a URL or another source type)

**Current worktree scope**:
A search scope representing the repository's checked-out commit, excluding staged, unstaged, and untracked changes.
_Avoid_: working-tree search (when referring to this narrower scope)

## Search

**Exact search**:
A case-insensitive literal substring search that returns content only when the query text occurs in it.
_Avoid_: keyword search

**Embedding search**:
A search that finds content by semantic similarity using generated embeddings and ranks results by embedding distance.
_Avoid_: vector search (when referring to the user-visible search capability)
