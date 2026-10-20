---
name: repodb-enabler
description: Discover RepoDB when repository search may help; route status, history/explore, augmented search, imports, and errors to focused guidance.
---

# RepoDB enabler

When this skill is invoked, check whether RepoDB is usable for the current repository before advertising its helper skills:

1. Check that `repodb` is on `PATH` (for example, `command -v repodb`).
2. Check for the repository's database. By default it is `repodb.sqlite` in the repository's Git directory; RepoDB can also be configured to use a different `--database PATH`. If the active database location cannot be established, say so rather than assuming one exists.

Do not initialize or refresh automatically. Do not run searches automatically for ordinary code tasks. The agent chooses whether RepoDB is useful unless repository policy (such as `AGENTS.md`) requires it.

If the executable is missing, explain that RepoDB is unavailable because the executable is missing. A database cannot make the missing executable usable; do not imply initialization alone will solve this.

If the executable exists but the repository database is missing, explain that it can be initialized with `repodb init`, and that indexing/embedding may take time. The agent decides whether to initialize; follow applicable policy about asking for user approval before initialization.

If both are available, explain that indexed results can be stale after repository changes. Suggest `repodb refresh` when relevant, but do not refresh automatically. Availability does not establish freshness or semantic-search readiness.

When RepoDB is available, route only the matching task to one helper and read it on demand:

- `repodb-status` — database state, freshness, and capability readiness; read `../repodb-status/SKILL.md`.
- `repodb-history` — committed selectors, rationale, and reference-only `--explore`; read `../repodb-history/SKILL.md`.
- `repodb-search-worktree` — staged/live exact search or mapped semantic results; read `../repodb-search-worktree/SKILL.md`.
- `repodb-import` — preview/register one selected source and explicitly refresh it; read `../repodb-import/SKILL.md`.
- `repodb-errors` — command failures, stale/index states, missing objects, and output limits; read `../repodb-errors/SKILL.md`.
- `repodb-help` — general RepoDB usage/capabilities; read `../repodb-help/SKILL.md`.
- `repodb-search`, `repodb-search-exact`, `repodb-search-semantic` — combined, literal, or conceptual discovery; read the correspondingly named sibling file.

For a direct RepoDB question, use `repodb-help`; route status, history, augmented search, imports, and failures to their focused helpers. These sibling definitions ship with the package, but only the enabler is registered globally.

## Distribution

This skill is distributed as the RepoDB Pi package. Install it globally with `pi install git:github.com/emfrom/repodb`. The package manifest registers only this enabler, keeping helper skill definitions out of the always-loaded skill list. The helper Markdown files are siblings in the package and are read on demand. The underscored identifiers above are logical capability names; Pi skill directories use portable hyphenated names.
