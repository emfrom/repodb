---
name: repodb-help
description: Explain RepoDB capabilities, availability, search choices, references, and limitations.
---

# RepoDB help (`repodb_help`)

Use this skill to answer questions about RepoDB based on this repository's documented behavior. Do not claim a capability that is not documented or available in the current environment.

## When RepoDB helps

RepoDB can complement or replace `rg` / `git grep` for discovery across content it has indexed, including repository history and semantically related code or documentation. It does not inspect unindexed or uncommitted content in the current filesystem. Use ordinary filesystem tools to inspect the current filesystem or open a known file directly.

The agent chooses whether RepoDB is useful unless the repository's policy, for example `AGENTS.md`, requires its use. Do not force a search for every code task.

## Availability

Use RepoDB only when the `repodb` executable and an initialized database for this repository are both available. The default database is `repodb.sqlite` in the repository's Git directory, though a database may be selected explicitly with `--database PATH`.

A missing database can be initialized with `repodb init`; initialization and indexing may take time. Do not initialize without the agent's decision and any user approval required by applicable policy. If the executable is missing, report that separately; initializing a database does not provide the executable.

An initialized database can be stale. Search does not refresh it. Suggest an explicit `repodb refresh` if relevant, but do not refresh automatically.

## Choosing a search

- Exact search is appropriate for a known identifier or literal phrase.
- Semantic search is appropriate for concepts, questions, code behavior, or documentation and guidance such as standards, README content, and `AGENTS.md`.
- Combined search is useful when either literal or conceptual matches may find the answer.

Search covers all indexed sources by default. It does not imply that every repository file or external source has been imported.

If semantic search is unavailable, report the limitation; do not silently substitute exact search. For weak or empty results, report that and suggest rephrasing the query or trying another search mode. Scoped search options are not provided by the current interface described here.

## Source references and missing content

Preserve the source references RepoDB actually returns, such as repository, commit/version, path, and line or page location. Do not fabricate missing provenance. If a result reference exists but its source content cannot be retrieved, report the available reference and disclose that the content is missing. Do not fetch or reconstruct it automatically.
