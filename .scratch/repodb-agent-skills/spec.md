Part of #37.

# RepoDB agent skills

## Problem Statement

Agents working in code repositories need RepoDB to be available as a code and documentation discovery capability, but must be able to decide when it is useful. Today there is no defined skill interface that makes RepoDB discoverable across code-oriented skills, explains when to use it instead of existing search tools, supports safe imports, or communicates limitations and source provenance consistently.

RepoDB should be broadly available to code-working agents without preloading every skill definition or forcing a search on every task. Imports must cover useful source types while making size and scope visible. Search guidance must help agents find both code and documentation, including standards and project guidance.

## Solution

Provide five RepoDB agent skills: `repodb_help`, `repodb_import`, `repodb_search`, `repodb_search_exact`, and `repodb_search_semantic`. A globally available enabler makes these skills discoverable without loading their definitions in every context. When invoked, it checks whether both the RepoDB executable and a RepoDB database are available for the repository. It then tells the agent which RepoDB skills are available and how to load them. For a direct question about RepoDB, it loads `repodb_help` and uses it to answer.

Agents choose whether to invoke the skills, except where a user or repository policy (for example, `AGENTS.md`) requires use. The enabler does not force a search for every code task. If RepoDB is unavailable, it does not load the helper skills; it explains what is missing and that initialization can take time. An agent may initialize when useful, subject to the applicable policy on user approval.

The import skill supports one explicitly selected source per invocation: a website page or bounded website crawl, local documentation file or explicitly selected directory, or local/remote Git repository. Search skills expose exact, semantic, and combined search, cover all indexed sources by default, explain relevant tool substitutions and use cases, and preserve source references and limitations.

## User Stories

1. As an agent starting work in a repository, I want to discover whether RepoDB is available, so that I know whether indexed code and documentation can help with the task.
2. As an agent in a repository without RepoDB, I want a clear explanation of what is missing and that initialization may take time, so that I can decide whether to initialize it under the applicable policy.
3. As an agent in a repository with an initialized but potentially stale database, I want to be told that its results may be stale and how to refresh it, so that I can make an informed decision without an automatic refresh.
4. As a user, I want RepoDB skill definitions to be available without having all of them loaded into every context, so that the agent context remains focused.
5. As an agent, I want the enabler to list the available RepoDB skills and how to load them, so that I can select the right capability for the task.
6. As a user asking a question about RepoDB, I want the enabler to load the RepoDB helper skill and answer from it, so that I receive useful RepoDB guidance directly.
7. As a user, I want repository policy to be able to require RepoDB use, so that teams can choose stricter usage without making it mandatory by default.
8. As an agent, I want to choose whether RepoDB is useful for a task, so that simple code tasks do not incur unnecessary searches.
9. As an agent exploring code, I want RepoDB to complement or replace `rg`/`git grep` when searching indexed history or searching semantically, so that I can find relevant past and conceptual matches.
10. As an agent, I want guidance to clarify that RepoDB does not replace filesystem inspection or reading a known file, so that I still use direct tools when they are more appropriate.
11. As an agent searching for a known identifier or exact phrase, I want an exact-search skill, so that I can find literal occurrences.
12. As an agent searching by a concept or question, I want a semantic-search skill, so that relevant code or documentation can be found even when it uses different words.
13. As an agent looking for project guidance, standards, coding standards, README content, `AGENTS.md`, or other documentation, I want semantic search to cover indexed documents, so that I can find relevant guidance by meaning.
14. As an agent uncertain whether a literal or semantic query is best, I want a combined-search skill, so that I can use both forms of matching.
15. As an agent, I want search to cover all indexed source types by default, so that I do not miss relevant imported or repository-local content.
16. As an agent, I want search results to retain available repository, version/commit, path, and line/page references, so that I can inspect and accurately report the source.
17. As an agent whose semantic-search capability is unavailable, I want the limitation reported instead of an undisclosed exact-search fallback, so that I do not mistake literal results for semantic coverage.
18. As an agent receiving weak or empty results, I want useful suggestions such as rephrasing, changing scope, or selecting another search skill, so that I can decide how to continue.
19. As an agent whose result references source content that is missing, I want that limitation reported alongside the available reference, so that I do not rely on content RepoDB could not provide.
20. As an agent, I want to import a website, local documentation, or local/remote repository when useful, so that relevant sources outside the repository's indexed Git history can become searchable.
21. As an agent importing a website, I want a bare URL to import only that page, so that the scope is predictable.
22. As an agent importing a website crawl, I want to explicitly bound the crawl to the same site and selected limits, so that imports do not expand unexpectedly.
23. As an agent importing local documentation, I want to select individual files or explicitly selected directories for recursive import, so that I can index untracked documentation collections without importing an unintended workspace.
24. As an agent, I want Markdown, plain text, PDF, HTML, and Org-mode documents supported, so that common documentation and standards can be imported.
25. As an agent importing a repository, I want HEAD to be the default and to be pinned to its resolved commit, so that the imported version is reproducible.
26. As an agent importing a repository, I want to select a branch, tag, or commit explicitly, so that I can pin the context relevant to my task.
27. As an agent, I want one source per import invocation, so that the interface and its size/provenance reporting remain simple; I can make multiple calls when needed.
28. As an agent importing a mutable website, I want each fetched snapshot identified by URL, content hash, and retrieval time, so that the indexed content has useful provenance.
29. As an agent about to make a large import, I want a warning when estimated net database growth is at least 50 MB or more than twice the existing database size, so that I can consider its storage impact.
30. As an agent receiving an import-size warning, I want to see both source size and estimated database growth, with source size used if growth cannot be estimated, so that I can judge the import with the available information.
31. As an agent, I want to decide whether to proceed after a size warning, with user approval requested only when applicable policy requires it, so that import remains possible at any size while respecting policy.
32. As an agent whose source cannot be imported, I want a clear failure and a supported alternative where available, so that content is not silently skipped or transformed unexpectedly.

## Implementation Decisions

- Create the five named RepoDB skills: `repodb_help`, `repodb_import`, `repodb_search`, `repodb_search_exact`, and `repodb_search_semantic`.
- Make only the enabler's availability present in a new context. Skill definitions are loaded only when needed. When invoked for a non-RepoDB question/task, the enabler communicates the availability of RepoDB skills and how to load them; for a RepoDB question, it loads the helper skill and answers.
- Check both the `repodb` executable and a database for the repository before exposing/loading RepoDB helper skills. An initialized database counts as available even if it may be stale. Mention potential staleness and suggest an explicit refresh when relevant; do not refresh automatically.
- When the database is missing, explain that it can be initialized and may take time. The agent decides whether to initialize and whether applicable policy requires user approval. Do not treat a missing executable as if the database can be initialized immediately.
- Users may require RepoDB use through repository instructions such as `AGENTS.md`; otherwise the agent decides whether RepoDB is useful.
- Implement `repodb_help` first, `repodb_import` second, then the search skills. No further order among the search skills is prescribed.
- Explain that RepoDB can replace `rg`/`git grep` for indexed-history and semantic discovery, but not for inspecting the current filesystem or opening a known file.
- Define `repodb_search` as combined search, `repodb_search_exact` as exact search, and `repodb_search_semantic` as semantic search. Provide use cases: literal identifiers/phrases for exact search; conceptual source and documentation/guidance questions for semantic search; combined search when either may find the answer.
- Searches cover all indexed content by default. Scoped searches (`source`, `repo-local source`, `repo-local docs`, `all docs`, and `all`, default) are tracked separately in follow-up issue #39.
- Preserve and report provenance fields RepoDB actually returns, such as repository, commit/version, path, and line/page reference. Never fabricate absent provenance.
- On unavailable semantic search, report the limitation without silently falling back to exact search. For weak/empty results, report that and suggest query/scope/mode alternatives. For missing source content, return the available reference and disclose the missing content; do not fetch or reconstruct automatically.
- `repodb_import` accepts one source per invocation. Agents may issue multiple requests themselves; batch import is explicitly out of scope.
- Minimum import source categories are websites, local untracked documentation, and local/remote Git repositories. Importing documentation/standards is a human/agent-selected operation, not automatic discovery.
- A supplied website URL imports one page by default. A crawl must be explicit, bounded by selected limits, and confined to the same site.
- Local documentation import accepts an explicitly selected file or directory; directory recursion is explicit and the selected content/scope is shown before import. Required documentation formats are Markdown (`.md`, `.markdown`), plain text (`.txt`), PDF, HTML, and Org mode (`.org`). If a source cannot be imported, report the reason and a supported alternative; do not silently skip it or perform surprising conversion.
- Repository import defaults to HEAD. The selected revision must be pinned/resolved to a commit identifier; users/agents may explicitly choose a branch, tag, or commit. Do not import full Git history by default.
- Website snapshot policy (resolved in #52/#54): each explicit full or import-only refresh fetches a registered URL; search never fetches. SHA-256 of response-body bytes is the version identity and JSON `blob_id`; unchanged bodies deduplicate and update `last_retrieved_at`, while changed bodies create versions. UTC RFC3339 retrieval timestamps are exposed in search JSON; migration leaves legacy timestamps null without fetching, and later refreshes update `last_retrieved_at` while preserving a null unknown `first_retrieved_at`. `latest` remains the default and keeps the latest successfully published version; `all` preserves distinct hashes. When `latest` prunes a hash, its first timestamp cannot be reconstructed if that hash later returns, so the new retained row starts a new `first_retrieved_at` lifetime. Explicit bounded crawling remains separately scoped to #55.
- Imports of any size remain possible. Warn when estimated net database growth is at least 50 MB or exceeds twice the existing database size. Show source size and estimated net growth; if growth cannot be estimated, use source size for the warning. The agent decides whether to proceed and whether user approval is required by policy. For an explicitly selected HTTP(S) page, `repodb import --preview URL` makes one GET without registering or indexing the page, reports response-body bytes and current database size, and uses source bytes when growth is unknown; the subsequent import fetches again and may observe different content.

## Testing Decisions

- Validate skill and enabler artifacts statically: verify the distributed definitions exist, stay synchronized with project-local copies, accurately describe available capabilities and limitations, and document intended routing.
- Test RepoDB behavior through the public CLI using isolated repositories and local fixtures. CLI integration tests are authoritative for search/import semantics, outputs, provenance, and side effects.
- Cover missing executable/database guidance, initialized-but-stale guidance, help and search guidance, unavailable semantic capability, weak/empty results, missing source content, and import failures through artifact checks and applicable CLI tests.
- Exercise imports using local fixtures and a local HTTP server rather than network-dependent tests. Cover website single-page vs bounded crawl, local file/directory selection and supported formats, pinned repository revision, no batch import, and large-import warnings/policy handoff.
- Prefer assertions about artifact consistency, public CLI outcomes, provenance, and side effects over exact prose where equivalent clear guidance suffices.
- Prior art includes the repository's existing end-to-end CLI integration tests, which use isolated temporary Git repositories and deterministic embedding commands, plus the import and history-search integration coverage.

## Out of Scope

- Automatically forcing RepoDB use in every code-related task by default.
- Automatically refreshing an initialized database or importing sources without an agent's decision.
- Importing full Git history by default.
- Batch imports; agents can make multiple single-source import requests.
- Automatically crawling websites, crossing the explicitly supplied site's boundary, or crawling without explicit bounds.
- Automatically fetching or reconstructing missing source objects during search.
- Silently substituting exact search when semantic search is unavailable.
- Implementing scoped searches in this work; see follow-up issue #39.
- Replacing filesystem inspection, known-file reading, or Git operations with RepoDB.

## Further Notes

- The scope decision was developed from ticket #37; scoped-search implementation is tracked separately in #39.
- RepoDB's current CLI supports one file, URL, or Git repository per import request and rejects ordinary directories. The spec intentionally describes the agreed target interface, including website crawling, selected local directories, HTML/Org-mode documents, and explicit repository revision selection.
- Website snapshot update/retention semantics were resolved by #52/#54 and are recorded above. URL preview/size warnings are implemented by #43; bounded crawling remains separately scoped to #55.