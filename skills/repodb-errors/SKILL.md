---
name: repodb-errors
description: Diagnose RepoDB command failures and distinguish missing, stale, unsupported, and empty states.
---

# RepoDB errors and limits

For JSON-mode search failures, preserve `error.code`, the message, and any `refresh_guidance`; do not convert an error into a no-match claim. Use `repodb status --json` to distinguish an unavailable database or capability from a genuinely empty result set. Status is read-only.

For a stale or unindexed HEAD, inspect the reported state and refresh explicitly when appropriate. For an exact-search missing-object error, preserve the reported blob ID; RepoDB does not return a partial result or substitute another object. Unsupported/binary/excluded content and worktree coverage gaps are not evidence that the content was searched.

A `result_limit` error means even the minimum reference could not fit; raise `--byte-budget` (reducing `--limit` cannot make that minimum reference smaller). If text was omitted, use the returned `reference_id` with `--fetch-reference ID`. A stale/invalid cursor belongs to a changed or invalid page snapshot; restart the same query to obtain a fresh page. Do not refresh, fetch, initialize, or install tools automatically while diagnosing an error.
