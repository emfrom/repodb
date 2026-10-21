---
name: repodb-search-worktree
description: Search staged and live working-tree text or map indexed semantic results forward.
---

# RepoDB augmented worktree search

Use `repodb search QUERY --worktree` only when the task explicitly needs staged or live working-tree content. This now defaults to combined exact-plus-semantic matching when semantic search is available. Add `--exact` for exact-only live search or `--semantic` for semantic mapping only. It requires the active HEAD to be present in the index; stale, missing, or alternate HEADs fail with an error and may include `repodb refresh` guidance. Search never refreshes automatically.

Exact worktree search inspects staged/live tracked changes and Git-visible untracked supported text, removes matches for deleted HEAD files, and reports best-effort coverage and provenance. Ignored, binary, unsupported, unreadable, and otherwise excluded files are not silently treated as searched.

With `--semantic --worktree`, only indexed HEAD embeddings rank results. Output distinguishes matched HEAD ranges from mapped staged/live ranges, reports mapping state and coverage gaps, and labels similarity as based on indexed HEAD text. Live text is shown only for unambiguous mappings; deleted or ambiguous mappings retain the HEAD reference. RepoDB does not embed or persist uncommitted text. `--current-worktree` is a compatibility name for HEAD-only and is not the same as `--worktree`.

Use `--json` when you need structured provenance. The common response budget can omit oversized text with a reference; use its `reference_id` with `--fetch-reference ID` and a larger `--byte-budget` if needed. Do not infer that an absent result proves the working tree is clean; consult Git and the reported coverage.
