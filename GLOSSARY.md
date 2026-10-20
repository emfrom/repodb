# RepoDB

A repository search context for finding source and documentation by exact text or semantic similarity, with references precise enough to locate the matched content.

## Searchable content

**Indexed unit**:
A paragraph or syntactic source construct treated as one searchable piece of content.
_Avoid_: chunk (when referring to the domain concept)

**Segmentation kind**:
The path-selected policy for dividing a blob into indexed units: document
paragraphs, Rust syntax, or heuristic fallback. Search chooses one eligible
occurrence before selecting that kind's units for the blob.

**Source reference**:
The identity and location of content in its source, such as a Git commit, file path, and line range, or an imported document and page number.
_Avoid_: citation (when referring to the stored source identity and location)

**Imported source**:
A source explicitly added to RepoDB from outside the current repository's tracked Git history, such as a document, URL, or external Git repository.
_Avoid_: external file (when the source may be a URL or repository)

**Native Git content**:
The tracked history of the repository in which RepoDB is operating, distinct from an explicitly added external Git repository.

**Website snapshot**:
The extracted searchable text obtained from a URL at a recorded retrieval time, identified by the source URL and the content hash of that text.
_Avoid_: web page (when referring to the retained searchable version)

**Current worktree scope**:
A search scope representing the repository's checked-out commit, excluding staged, unstaged, and untracked changes.
_Avoid_: working-tree search (when referring to this narrower scope)

**Worktree augmentation**:
A best-effort search view that relates the checked-out commit to staged and live changes, including eligible untracked files, without making those changes part of committed history.

## Search

**Exact search**:
A case-insensitive literal substring search that returns content only when the query text occurs in it.
_Avoid_: keyword search

**Embedding search**:
A search that finds content by semantic similarity using generated embeddings and ranks results by embedding distance.
_Avoid_: vector search (when referring to the user-visible search capability)
