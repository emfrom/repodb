---
name: repodb-enabler
description: Discover RepoDB for repository code and documentation work; use when asked whether RepoDB skills are available or how to use them.
---

# RepoDB enabler

When this skill is invoked, check whether RepoDB is usable for the current repository before advertising its helper skills:

1. Check that `repodb` is on `PATH` (for example, `command -v repodb`).
2. Check for the repository's database. By default it is `repodb.sqlite` in the repository's Git directory; RepoDB can also be configured to use a different `--database PATH`. If the active database location cannot be established, say so rather than assuming one exists.

Do not initialize or refresh automatically. Do not run searches automatically for ordinary code tasks. The agent chooses whether RepoDB is useful unless repository policy (such as `AGENTS.md`) requires it.

If the executable is missing, explain that RepoDB is unavailable because the executable is missing. A database cannot make the missing executable usable; do not imply initialization alone will solve this.

If the executable exists but the repository database is missing, explain that it can be initialized with `repodb init`, and that indexing/embedding may take time. The agent decides whether to initialize; follow applicable policy about asking for user approval before initialization.

If both are available, explain that indexed results can be stale after repository changes. Suggest `repodb refresh` when relevant, but do not refresh automatically. Availability does not establish freshness or semantic-search readiness.

When RepoDB is available, these helper instructions are usable in this session: read the corresponding sibling `SKILL.md` when the task calls for it. Only the enabler is registered as a Pi skill; the helpers are files it reads, not separate registered skills. When asked whether RepoDB agent skills are available, distinguish *registered in Pi* from *usable through the enabler*; do not describe the helpers as unavailable merely because they are not separately registered or the repo-local `.agents/skills/` copies are absent.

Choose the helper for the task:

- `repodb_help` — answer RepoDB usage and capability questions; read `../repodb-help/SKILL.md` relative to this skill file.
- `repodb_import` — preview and import one explicitly selected local documentation file or directory; read `../repodb-import/SKILL.md` relative to this skill file.
- `repodb_search` — combined exact and semantic discovery; read `../repodb-search/SKILL.md` relative to this skill file.
- `repodb_search_exact` — literal matching; read `../repodb-search-exact/SKILL.md` relative to this skill file.
- `repodb_search_semantic` — conceptual search; read `../repodb-search-semantic/SKILL.md` relative to this skill file.

For a direct question about RepoDB, read `../repodb-help/SKILL.md` relative to this skill file and answer from that guidance. For other tasks, describe the available helpers and let the agent decide whether one is useful. Read a helper before following its guidance; the sibling files ship with this package.

## Distribution

This skill is distributed as the RepoDB Pi package. Install it globally with `pi install git:github.com/emfrom/repodb`. The package manifest registers only this enabler, keeping helper skill definitions out of the always-loaded skill list. The helper Markdown files are siblings in the package and are read on demand. The underscored identifiers above are logical capability names; Pi skill directories use portable hyphenated names.
