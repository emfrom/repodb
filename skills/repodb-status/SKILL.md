---
name: repodb-status
description: Check RepoDB index freshness, database state, and readiness by capability.
---

# RepoDB status

Use `repodb status --json` when you need to distinguish a missing/uninitialized database, stale or dirty index, and a genuinely empty searchable snapshot. Status reports indexed/live HEAD, worktree state, searchable-unit counts, and readiness for exact search, semantic search, refresh, and document import.

Status is read-only: it does not initialize, refresh, fetch URLs, or change the index. If the database is missing, explain that before suggesting `repodb init`; do not create it just to inspect readiness. Search results of zero do not by themselves prove that the database is missing or empty.

Use `--database PATH` before `status` when the active database is explicitly configured. See `repodb status --help` for the current interface.
