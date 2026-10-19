---
name: repodb-enabler
description: Discover RepoDB for repository code and documentation work when it may help, and route RepoDB questions to the help skill.
---

# RepoDB enabler

When this skill is invoked, check whether RepoDB is usable for the current repository before advertising its helper skills:

1. Check that `repodb` is on `PATH` (for example, `command -v repodb`).
2. Check for the repository's database. By default it is `repodb.sqlite` in the repository's Git directory; RepoDB can also be configured to use a different `--database PATH`. If the active database location cannot be established, say so rather than assuming one exists.

Do not initialize or refresh automatically. Do not run searches automatically for ordinary code tasks. The agent chooses whether RepoDB is useful unless repository policy (such as `AGENTS.md`) requires it.

If the executable is missing, explain that RepoDB is unavailable because the executable is missing. A database cannot make the missing executable usable; do not imply initialization alone will solve this.

If the executable exists but the repository database is missing, explain that it can be initialized with `repodb init`, and that indexing/embedding may take time. The agent decides whether to initialize; follow applicable policy about asking for user approval before initialization.

If both are available, explain that indexed results can be stale after repository changes. Suggest `repodb refresh` when relevant, but do not refresh automatically. Availability does not establish freshness or semantic-search readiness.

When RepoDB is available, make these helper skills discoverable by name and load them only when needed:

- `repodb_help` — answer RepoDB usage and capability questions; load `.agents/skills/repodb-help/SKILL.md`.
- `repodb_import` — preview and import one explicitly selected local documentation file or directory; load `.agents/skills/repodb-import/SKILL.md`.
- `repodb_search` — combined exact and semantic discovery; definition is not included in this ticket.
- `repodb_search_exact` — literal matching; definition is not included in this ticket.
- `repodb_search_semantic` — conceptual search; definition is not included in this ticket.

For a direct question about RepoDB, load `repodb_help` and answer from that guidance. For other tasks, describe the available skills and let the agent decide whether one is useful. The `repodb_import` definition is available as a repo-local skill. Do not imply that the search skill definitions are available in this repository yet.

## Distribution limitation

This is a repo-local skill artifact under `.agents/skills/`; the repository does not currently provide a supported distribution or installation mechanism for making it globally available. Pi's naming and global-availability behavior cannot be verified or claimed by the repository's automated tests. The underscored identifiers above are the required skill names in the RepoDB specification; the project-local Pi skill directories use portable hyphenated names.
